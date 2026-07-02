//! Session JSONL file I/O (NC-002 R2): versioned header, tolerant read,
//! append.
//!
//! Index maintenance lives in [`super::index`].

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::session::events::{EventId, SessionEvent};

use super::index::{read_index, sum_usage_from_events, update_session_index};
use super::replay::ReplayArtifacts;
use super::types::{SESSION_FORMAT_VERSION, SessionFileHeader, SessionPersistError};

/// Return the JSONL file path for `session_id` under `data_dir`.
///
/// Callers passing an id that did not come out of the session index must
/// validate it first (see [`is_reserved_session_id`] and the manager's
/// explicit-ID validation) — the mapping itself is mechanical.
#[must_use]
pub fn session_file_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join(format!("{session_id}.jsonl"))
}

/// Name stems the persistence layer reserves for its own files in the
/// session data directory.
///
/// Session IDs and persistence-owned files share the data directory:
/// [`session_file_path`] maps an id to `{id}.jsonl`, so the id `"index"`
/// would name `index.jsonl` — the session index itself. A reserved stem
/// excludes the stem and its entire `.`-extended family (`index`,
/// `index.jsonl`, `index.lock`, `index.jsonl.tmp.{uuid}`, …) from the
/// session-id namespace, matched ASCII-case-insensitively because the
/// default macOS and Windows filesystems are case-insensitive.
///
/// **Adding a new persistence-owned file?** Name it
/// `<reserved-stem>.<suffix>` (already excluded), or add its stem here —
/// never claim a name session IDs can reach.
pub const RESERVED_SESSION_ID_STEMS: &[&str] = &["index"];

/// Whether `id` is reserved by the persistence layer and may never be
/// used as a session ID (see [`RESERVED_SESSION_ID_STEMS`]).
#[must_use]
pub fn is_reserved_session_id(id: &str) -> bool {
    RESERVED_SESSION_ID_STEMS.iter().any(|stem| {
        // `get` (not `split_at`) so a multi-byte char straddling the
        // boundary yields `None` instead of panicking — such an id can
        // never match an ASCII stem anyway.
        let Some(head) = id.get(..stem.len()) else {
            return false;
        };
        let rest = &id[stem.len()..];
        head.eq_ignore_ascii_case(stem) && (rest.is_empty() || rest.starts_with('.'))
    })
}

/// Reject `id` with [`SessionPersistError::InvalidSessionId`] when it is
/// reserved by the persistence layer. Every boundary where a session ID
/// selects a file in the data directory calls this — the manager's
/// explicit-ID validation, index insertion, event append/read, and sink
/// open — so a reserved ID can never reach [`session_file_path`].
pub(crate) fn ensure_session_id_not_reserved(id: &str) -> Result<(), SessionPersistError> {
    if is_reserved_session_id(id) {
        return Err(SessionPersistError::InvalidSessionId {
            id: id.to_owned(),
            reason: format!(
                "collides with the session persistence layer's own files \
                 (reserved name stems and their '.'-extended families: {})",
                RESERVED_SESSION_ID_STEMS.join(", "),
            ),
        });
    }
    Ok(())
}

/// Open (or create) the session JSONL file at `path` in append mode,
/// creating parent directories as needed.
///
/// Creation stamps the [`SessionFileHeader`] (carrying
/// [`SESSION_FORMAT_VERSION`]) **atomically with the file's appearance**:
/// the header is written and `fsync`-ed to a same-directory temp file,
/// then [`std::fs::hard_link`]-ed into place at `path`. Because the link
/// is the first moment `path` exists, the file is never observable
/// without its header — closing the residual race where a create winner
/// (exclusive `create_new` + a separate header `write_all`) could be
/// preempted between the two steps, letting a racing loser append its
/// first event ahead of the header and leaving it as a permanently
/// skipped corrupt line at line 2. Exactly one opener wins the link; the
/// loser gets `AlreadyExists` and takes the reopen path, so two processes
/// racing the first open can never both stamp a header. A pre-existing
/// **empty** file (creator crashed between creation and the header write,
/// or external truncation) is never retro-stamped: writing a header on
/// "observed empty" is exactly the check-then-write race the atomic link
/// closes, and a headerless file loads fine as pre-versioning format.
///
/// When the file is non-empty and its last byte is not `\n` — a torn
/// final line left by a crash (`ENOSPC`, `kill -9`, power loss) in a
/// previous process — the tear is healed before the handle is returned:
/// a lone `\n` terminates the partial line so it becomes exactly one
/// corrupt line for the tolerant reader to skip, and the first append
/// through this handle starts on a fresh line instead of concatenating
/// onto the torn bytes (H19, reopen half).
pub(crate) fn open_session_append(path: &Path) -> Result<File, SessionPersistError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Fast path: the file already exists → reopen and heal directly. This
    // skips the temp-file stamp (which needs directory write permission the
    // append itself does not), preserving the contract that a subsequent
    // append to an existing session stays durable even when the data dir
    // has been made read-only. A racing first-create that lands between
    // this check and the stamp below still resolves correctly: the stamp's
    // `AlreadyExists` arm falls through to the same reopen path, and the
    // winner's header is always present because the file only becomes
    // visible via the atomic link.
    if path.exists() {
        return reopen_and_heal(path);
    }
    match stamp_header_atomically(path) {
        // We created the file; it already contains its header. Return a
        // fresh append handle onto the linked inode.
        Ok(()) => Ok(OpenOptions::new().read(true).append(true).open(path)?),
        // Another opener already created the file (or it pre-existed):
        // reopen and heal any torn final line, never retro-stamping.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => reopen_and_heal(path),
        Err(error) => Err(SessionPersistError::Io(error)),
    }
}

/// Stamp the versioned header into `path` atomically with the file
/// becoming visible.
///
/// Writes the header to a uniquely-named temp file in the same directory,
/// `fsync`s it, then hard-links it to `path`. Success means this caller
/// created `path` with its header already durable; an `AlreadyExists`
/// error means another opener won the link (or the file pre-existed). The
/// temp file is always removed, whether the link wins, loses, or errors,
/// so a leftover temp never accumulates.
fn stamp_header_atomically(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session file path has no valid final component",
            )
        })?;
    // Same directory as `path` (a hard link requires it), uniquely named
    // so concurrent creators never collide on the temp itself. The
    // `.jsonl.tmp.*` shape stays inside the reserved family the reader and
    // listing already ignore.
    let tmp_path = parent.join(format!("{file_name}.tmp.{}", uuid::Uuid::new_v4()));

    let mut header = serde_json::to_vec(&SessionFileHeader {
        version: SESSION_FORMAT_VERSION,
    })
    .map_err(std::io::Error::other)?;
    header.push(b'\n');

    let write_result = (|| -> std::io::Result<()> {
        let mut tmp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        tmp.write_all(&header)?;
        // Durably land the header bytes before the link makes the inode
        // reachable at `path`.
        tmp.sync_all()
    })();
    let link_result = write_result.and_then(|()| fs::hard_link(&tmp_path, path));

    // Best-effort cleanup on every outcome. A failure to remove the temp
    // never corrupts the session and never fails the open — the orphan is
    // an inert `.jsonl.tmp.*` file the reader and listing ignore — but it
    // must never pass silently.
    if let Err(error) = fs::remove_file(&tmp_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %tmp_path.display(),
            %error,
            "failed to remove session header temp file after stamping; \
             the orphan is inert and ignored by readers",
        );
    }

    link_result
}

/// Reopen an existing session file in append mode and heal a torn final
/// line (H19, reopen half). Never retro-stamps a header.
fn reopen_and_heal(path: &Path) -> Result<File, SessionPersistError> {
    let mut file = OpenOptions::new().read(true).append(true).open(path)?;
    let len = file.metadata()?.len();
    if len > 0 {
        file.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            // O_APPEND ignores the read cursor: this lands at EOF.
            file.write_all(b"\n")?;
            tracing::warn!(
                path = %path.display(),
                "healed torn final line in session file on reopen; \
                 the tolerant reader will skip the corrupt line",
            );
        }
    }
    Ok(file)
}

/// Tolerantly read `{data_dir}/{session_id}.jsonl` in a single pass,
/// producing every resume artifact at once (H19 / R4).
///
/// Returns empty [`ReplayArtifacts`] when the file does not exist. Each
/// recovered event is folded into the artifact accumulators (usage
/// rollup, compaction supersession marks) as it is parsed, so opening a
/// session never re-walks the history per consumer. Line handling:
///
/// * an optional [`SessionFileHeader`] first line populates
///   [`ReplayArtifacts::format_version`] (absent on pre-versioning
///   files — they still load);
/// * empty / whitespace-only lines are skipped silently;
/// * a non-empty line that is not valid JSON (e.g. a torn final line
///   from `ENOSPC` or `kill -9` mid-write) or is valid JSON that does
///   not match the [`SessionEvent`] schema (e.g. an unknown variant from
///   a newer writer) is skipped with a `tracing::warn!` and counted in
///   [`ReplayArtifacts::skipped_lines`];
/// * a line whose [`EventId`] was already seen earlier in the file (a
///   crash-retry artifact: the first attempt persisted but reported
///   failure, so the documented-safe retry wrote the event again) keeps
///   its first occurrence and skips the duplicate with a
///   `tracing::warn!`, also counted in
///   [`ReplayArtifacts::skipped_lines`].
///
/// The call fails when `session_id` is reserved by the persistence layer
/// ([`SessionPersistError::InvalidSessionId`] — the id would select a
/// persistence-owned file such as the session index, never session data)
/// or when the file as a whole is unreadable (open or stream-level I/O
/// error) — a torn final line never prevents resume.
pub fn read_session_events(
    data_dir: &Path,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    ensure_session_id_not_reserved(session_id)?;
    let path = session_file_path(data_dir, session_id);
    if !path.exists() {
        return Ok(ReplayArtifacts::default());
    }
    let file = File::open(&path)?;
    read_session_events_from(BufReader::new(file), session_id)
}

/// The single-pass tolerant reader behind [`read_session_events`],
/// generic over the byte source so tests can instrument the reader and
/// prove the history is traversed exactly once.
pub(crate) fn read_session_events_from<R: BufRead>(
    reader: R,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let mut artifacts = ReplayArtifacts::default();
    let mut seen_first_content_line = false;
    let mut seen_ids: HashSet<EventId> = HashSet::new();
    for (idx, raw) in reader.split(b'\n').enumerate() {
        let raw = raw?;
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !seen_first_content_line {
            seen_first_content_line = true;
            if let Ok(header) = serde_json::from_slice::<SessionFileHeader>(&raw) {
                artifacts.format_version = Some(header.version);
                if header.version > SESSION_FORMAT_VERSION {
                    tracing::warn!(
                        session_id,
                        file_version = header.version,
                        reader_version = SESSION_FORMAT_VERSION,
                        "session file written by a newer norn; \
                         unknown event variants will be skipped",
                    );
                }
                continue;
            }
        }
        match serde_json::from_slice::<SessionEvent>(&raw) {
            Ok(event) => {
                let id = event.base().id.clone();
                if seen_ids.insert(id.clone()) {
                    artifacts.push(event);
                } else {
                    artifacts.skipped_lines = artifacts.skipped_lines.saturating_add(1);
                    tracing::warn!(
                        session_id,
                        line = idx + 1,
                        event_id = %id,
                        "skipping duplicate session event line \
                         (crash-retry artifact); first occurrence kept",
                    );
                }
            }
            Err(error) => {
                artifacts.skipped_lines = artifacts.skipped_lines.saturating_add(1);
                tracing::warn!(
                    session_id,
                    line = idx + 1,
                    %error,
                    "skipping corrupt or unknown session event line",
                );
            }
        }
    }
    if artifacts.skipped_lines > 0 {
        tracing::warn!(
            session_id,
            skipped = artifacts.skipped_lines,
            recovered = artifacts.events.len(),
            "session file contained unparseable or duplicate lines; \
             recovered events were loaded, the rest were skipped",
        );
    }
    Ok(artifacts)
}

/// Append `events` to `{data_dir}/{session_id}.jsonl` and update the
/// matching index entry's `event_count`, usage totals, and `updated_at`.
///
/// `disabled = true` short-circuits the call with `Ok(())` and performs
/// no filesystem work — this is the `--no-session` path.
///
/// Empty `events` is a no-op. A reserved `session_id` (one that would
/// select a persistence-owned file — see [`is_reserved_session_id`])
/// returns [`SessionPersistError::InvalidSessionId`] before anything is
/// touched. The index entry MUST already exist and is verified **before**
/// any event bytes are written; a missing entry returns
/// [`SessionPersistError::NotFound`] with the session file untouched. The session JSONL file and its parent directory are
/// created on first write (with a version header line), and the whole
/// batch is flushed and `fsync`-ed.
///
/// `Ok(())` means exactly: the events are durable in the session file.
/// The index update runs after that point and is best-effort — a
/// failure there is logged at error level and does **not** fail the
/// call, because returning an error for an already-durable batch would
/// invite a retry that duplicates every event. The stale entry is
/// repaired by the self-maintenance pass in
/// [`SessionManager::resume`](crate::session::SessionManager::resume).
/// An error return therefore always means "nothing from this batch was
/// written", so retrying the same batch is safe.
pub fn append_events(
    data_dir: &Path,
    session_id: &str,
    events: &[SessionEvent],
    disabled: bool,
) -> Result<(), SessionPersistError> {
    if disabled || events.is_empty() {
        return Ok(());
    }
    ensure_session_id_not_reserved(session_id)?;
    if !read_index(data_dir)?.iter().any(|e| e.id == session_id) {
        return Err(SessionPersistError::NotFound {
            input: session_id.to_owned(),
        });
    }
    let path = session_file_path(data_dir, session_id);
    let mut file = open_session_append(&path)?;
    let mut buf = Vec::new();
    for event in events {
        serde_json::to_writer(&mut buf, event)?;
        buf.push(b'\n');
    }
    file.write_all(&buf)?;
    file.sync_all()?;

    let appended = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let usage_delta = sum_usage_from_events(events);
    // The batch path has no per-caller lock configuration; it keeps the
    // indefinite index-lock wait (the documented default of every
    // lock-taking index function).
    if let Err(error) = update_session_index(data_dir, session_id, appended, &usage_delta, None) {
        tracing::error!(
            session_id,
            %error,
            appended,
            "session events are durable but the index entry could not \
             be updated; the index is stale until the next resume \
             repairs it",
        );
    }
    Ok(())
}
