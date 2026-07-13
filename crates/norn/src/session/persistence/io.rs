//! Session JSONL file I/O (NC-002 R2): versioned header, tolerant read,
//! append.
//!
//! Index maintenance lives in [`super::index`].

use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use crate::resource::DescriptorPermit;
use crate::session::events::SessionEvent;
use crate::util::{PrivateFileIdentity, PrivateRoot, validate_private_component};

use super::acquire_private_fs;
use super::index::{read_index, sum_usage_from_events, update_session_index};
use super::types::{
    SESSION_FORMAT_VERSION, SessionFileHeader, SessionIndexEntry, SessionPersistError,
};

#[derive(Debug)]
pub(crate) struct AdmittedSessionFile {
    file: File,
    _permit: DescriptorPermit,
}

impl Deref for AdmittedSessionFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl DerefMut for AdmittedSessionFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

#[cfg(test)]
pub(crate) use super::event_reader::read_session_events_from;
pub use super::event_reader::{read_session_events, read_session_events_for_entry};

/// Return the flat JSONL file path for `session_id` under `data_dir`.
///
/// Test-only raw legacy path derivation. Production code must use the
/// validated descriptor-relative session APIs.
#[cfg(test)]
pub(crate) fn session_file_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join(format!("{session_id}.jsonl"))
}

/// Resolve an index entry's session file path: the entry's
/// [`rel_path`](SessionIndexEntry::rel_path) joined onto `data_dir` when
/// present (child sessions), otherwise the flat legacy derivation
/// (`{data_dir}/{id}.jsonl`). Legacy rows have no `rel_path` and keep
/// resolving exactly as before the nested layout existed — zero
/// migration.
#[cfg(test)]
pub(crate) fn resolved_session_file_path(data_dir: &Path, entry: &SessionIndexEntry) -> PathBuf {
    entry.rel_path.as_ref().map_or_else(
        || session_file_path(data_dir, &entry.id),
        |rel| data_dir.join(rel),
    )
}

/// Name stems the persistence layer reserves for its own files in the
/// session data directory.
///
/// Session IDs and persistence-owned files share the data directory:
/// A flat legacy session maps an id to `{id}.jsonl`, so the id `"index"`
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
/// open — so a reserved ID can never select a persistence-owned filename.
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

/// Reject session identifiers that cannot be one private path component.
pub(crate) fn ensure_session_id_path_safe(id: &str) -> Result<(), SessionPersistError> {
    ensure_session_id_not_reserved(id)?;
    let invalid = |reason: &str| SessionPersistError::InvalidSessionId {
        id: id.to_owned(),
        reason: reason.to_owned(),
    };
    validate_private_component(id, "session id").map_err(|error| invalid(&error.to_string()))?;
    let Some(first) = id.chars().next() else {
        return Err(invalid("must not be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(invalid("must start with an ASCII letter or digit"));
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(invalid(
            "may contain only ASCII letters, digits, '-', '_', and '.'",
        ));
    }
    Ok(())
}

pub(crate) fn session_file_relative(
    entry: &SessionIndexEntry,
) -> Result<PathBuf, SessionPersistError> {
    ensure_session_id_path_safe(&entry.id)?;
    let Some(relative) = entry.rel_path.as_deref() else {
        return Ok(PathBuf::from(format!("{}.jsonl", entry.id)));
    };
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let valid = matches!(components.as_slice(), [
        std::path::Component::Normal(root),
        std::path::Component::Normal(children),
        std::path::Component::Normal(file),
    ] if children == &std::ffi::OsStr::new("children")
        && path_component_is_safe(root)
        && path_component_is_safe(file)
        && Path::new(file).extension().is_some_and(|extension| extension == "jsonl"));
    if !valid {
        return Err(SessionPersistError::InvalidSessionId {
            id: entry.id.clone(),
            reason: "indexed rel_path must have the safe '<root>/children/<file>.jsonl' shape"
                .to_owned(),
        });
    }
    Ok(PathBuf::from(relative))
}

fn path_component_is_safe(component: &std::ffi::OsStr) -> bool {
    component
        .to_str()
        .is_some_and(|value| validate_private_component(value, "session path component").is_ok())
}

/// Open (or create) the session JSONL file at `path` in append mode,
/// creating parent directories as needed.
///
/// Creation stamps the [`SessionFileHeader`] (carrying
/// [`SESSION_FORMAT_VERSION`]) **atomically with the file's appearance**:
/// the header is written and `fsync`-ed to a same-directory temp file,
/// then published with a descriptor-relative no-replace primitive at
/// `path` (`linkat` on supported POSIX-style Unix; unsupported targets fail
/// closed). Because publication is
/// the first moment `path` exists, the file is never observable
/// without its header — closing the residual race where a create winner
/// (exclusive `create_new` + a separate header `write_all`) could be
/// preempted between the two steps, letting a racing loser append its
/// first event ahead of the header and leaving it as a permanently
/// skipped corrupt line at line 2. Exactly one opener wins publication; the
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
pub(crate) fn open_session_append(path: &Path) -> Result<AdmittedSessionFile, SessionPersistError> {
    let permit = acquire_private_fs()?;
    let parent = path.parent().ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file must have an absolute parent directory",
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file path has no final component",
        ))
    })?;
    let root = PrivateRoot::create(parent)?;
    let file = open_session_append_under(&root, Path::new(file_name))?;
    Ok(AdmittedSessionFile {
        file,
        _permit: permit,
    })
}

fn open_session_append_under(
    root: &PrivateRoot,
    relative: &Path,
) -> Result<File, SessionPersistError> {
    if let Some(parent) = relative.parent() {
        root.create_dir_all(parent)?;
    }
    // Fast path: securely attempt the reopen and heal directly. This skips
    // the temp-file stamp (which needs directory write permission the append
    // itself does not), preserving the contract that a subsequent append to
    // an existing session stays durable even when the data dir has been made
    // read-only. A racing first-create that lands after the `NotFound` result
    // still resolves correctly: the stamp's `AlreadyExists` arm falls through
    // to the same reopen path, and the winner's header is always present
    // because the file only becomes visible via atomic no-replace publication.
    match reopen_and_heal(root, relative) {
        Ok(file) => return Ok(file),
        Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match stamp_header_atomically(root, relative) {
        // We created the file; it already contains its header. Return a
        // fresh append handle onto the published inode.
        Ok(()) => Ok(root.open_read_append(relative)?),
        // Another opener already created the file (or it pre-existed):
        // reopen and heal any torn final line, never retro-stamping.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reopen_and_heal(root, relative)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn open_session_append_for_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
) -> Result<AdmittedSessionFile, SessionPersistError> {
    let permit = acquire_private_fs()?;
    let relative = session_file_relative(entry)?;
    let root = PrivateRoot::create(data_dir)?;
    let file = open_session_append_under(&root, &relative)?;
    Ok(AdmittedSessionFile {
        file,
        _permit: permit,
    })
}

/// Reopen an already-bound session path, verify its inode, then heal its tail.
pub(crate) fn open_session_append_bound(
    path: &Path,
    identity: PrivateFileIdentity,
) -> Result<AdmittedSessionFile, SessionPersistError> {
    let permit = acquire_private_fs()?;
    let parent = path.parent().ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file must have an absolute parent directory",
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file path has no final component",
        ))
    })?;
    let root = PrivateRoot::open(parent)?;
    let file = reopen_bound_and_heal(&root, Path::new(file_name), identity)?;
    Ok(AdmittedSessionFile {
        file,
        _permit: permit,
    })
}

/// Registered-entry counterpart of [`open_session_append_bound`].
pub(crate) fn open_session_append_for_entry_bound(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    identity: PrivateFileIdentity,
) -> Result<AdmittedSessionFile, SessionPersistError> {
    let permit = acquire_private_fs()?;
    let relative = session_file_relative(entry)?;
    let root = PrivateRoot::open(data_dir)?;
    let file = reopen_bound_and_heal(&root, &relative, identity)?;
    Ok(AdmittedSessionFile {
        file,
        _permit: permit,
    })
}

/// Stamp the versioned header into `path` atomically with the file
/// becoming visible.
///
/// Writes the header to a uniquely-named temp file in the same directory,
/// `fsync`s it, then publishes it using the platform's descriptor-relative
/// no-replace primitive. Success means this caller
/// created `path` with its header already durable; an `AlreadyExists`
/// error means another opener won publication (or the file pre-existed).
/// The temp file is always removed, whether publication wins, loses, or errors,
/// so a leftover temp never accumulates.
fn stamp_header_atomically(root: &PrivateRoot, path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session file path has no valid final component",
            )
        })?;
    // Same directory as `path` (POSIX publication uses a hard link), uniquely named
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
        let mut tmp = root.create_new(&tmp_path)?;
        tmp.write_all(&header)?;
        // Durably land the header bytes before publication makes the inode
        // reachable at `path`.
        tmp.sync_all()
    })();
    let link_result = write_result.and_then(|()| root.publish_new(&tmp_path, path));

    // Best-effort cleanup on every outcome. A failure to remove the temp
    // never corrupts the session and never fails the open — the orphan is
    // an inert `.jsonl.tmp.*` file the reader and listing ignore — but it
    // must never pass silently.
    if let Err(error) = root.remove_file(&tmp_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %root.display_path(&tmp_path).display(),
            %error,
            "failed to remove session header temp file after stamping; \
             the orphan is inert and ignored by readers",
        );
    }

    link_result
}

/// Reopen an existing session file in append mode and heal a torn final
/// line (H19, reopen half). Never retro-stamps a header.
fn reopen_and_heal(root: &PrivateRoot, path: &Path) -> Result<File, SessionPersistError> {
    let mut file = root.open_read_append(path)?;
    heal_torn_tail(&mut file, &root.display_path(path))?;
    Ok(file)
}

fn reopen_bound_and_heal(
    root: &PrivateRoot,
    path: &Path,
    identity: PrivateFileIdentity,
) -> Result<File, SessionPersistError> {
    let mut file = root.open_read_append(path)?;
    identity.verify(&file)?;
    heal_torn_tail(&mut file, &root.display_path(path))?;
    Ok(file)
}

fn heal_torn_tail(file: &mut File, display_path: &Path) -> Result<(), SessionPersistError> {
    let len = file.metadata()?.len();
    if len > 0 {
        file.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            // O_APPEND ignores the read cursor: this lands at EOF.
            file.write_all(b"\n")?;
            tracing::warn!(
                path = %display_path.display(),
                "healed torn final line in session file on reopen; \
                 the tolerant reader will skip the corrupt line",
            );
        }
    }
    Ok(())
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
    // The mandatory index lookup doubles as the path resolution: a child
    // entry's `rel_path` locates the nested file; legacy/root entries fall
    // through to the flat derivation.
    let Some(entry) = read_index(data_dir)?
        .into_iter()
        .find(|e| e.id == session_id)
    else {
        return Err(SessionPersistError::NotFound {
            input: session_id.to_owned(),
        });
    };
    let relative = session_file_relative(&entry)?;
    let permit = acquire_private_fs()?;
    let root = PrivateRoot::create(data_dir)?;
    let mut file = open_session_append_under(&root, &relative)?;
    let mut buf = Vec::new();
    for event in events {
        serde_json::to_writer(&mut buf, event)?;
        buf.push(b'\n');
    }
    file.write_all(&buf)?;
    file.sync_all()?;
    drop(file);
    drop(root);
    drop(permit);

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
