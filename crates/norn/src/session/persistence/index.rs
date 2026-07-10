//! Session index maintenance: `{data_dir}/index.jsonl`.
//!
//! All mutating entry points ([`append_index_entry`],
//! [`update_index_entry`], [`remove_index_entry`]) serialise across
//! processes via the advisory lock in `super::lock` (H18), and
//! rewrites stay atomic (write-to-tmp + fsync + rename) for torn-write
//! protection.
//!
//! Every lock-taking function accepts an optional acquisition deadline:
//! `None` waits indefinitely (the OS lock primitive's own behaviour),
//! `Some(d)` fails with [`SessionPersistError::IndexLockTimeout`] when
//! the lock cannot be acquired within `d`, leaving the index untouched.

use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::util::PrivateRoot;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::io::{ensure_session_id_path_safe, session_file_relative};
use super::lock::lock_index;
use super::types::{SessionIndexEntry, SessionPersistError};

const INDEX_FILE_NAME: &str = "index.jsonl";

/// Return the session index file path under `data_dir`.
#[must_use]
pub fn index_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INDEX_FILE_NAME)
}

/// Read every [`SessionIndexEntry`] from `{data_dir}/index.jsonl`.
///
/// Returns an empty vector when the file does not exist. Empty lines are
/// skipped. A non-empty line that fails to parse or whose entry is unsafe
/// (e.g. torn by a crash or carrying a persistence-reserved ID) is skipped
/// with a `tracing::warn!` carrying its 1-based line number — one corrupt or
/// hostile entry must never make every other session unlistable. The call
/// fails only if the file itself is unreadable.
pub fn read_index(data_dir: &Path) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let root = match PrivateRoot::open(data_dir) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SessionPersistError::Io(error)),
    };
    read_index_in(&root)
}

fn read_index_in(root: &PrivateRoot) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let file = match root.open_read(Path::new(INDEX_FILE_NAME)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SessionPersistError::Io(error)),
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionIndexEntry>(&line) {
            Ok(entry) => match validate_index_entry(&entry) {
                Ok(()) => out.push(entry),
                Err(error) => {
                    tracing::warn!(
                        line = idx + 1,
                        %error,
                        "skipping unsafe session index line",
                    );
                }
            },
            Err(error) => {
                tracing::warn!(
                    line = idx + 1,
                    %error,
                    "skipping corrupt session index line",
                );
            }
        }
    }
    Ok(out)
}

/// Atomically rewrite `{data_dir}/index.jsonl` with `entries`.
///
/// Writes go to a unique `index.jsonl.tmp.UUID` file first, are flushed
/// and `fsync`-ed, then renamed over the canonical path. On any failure
/// between write and rename the tmp file is removed (a cleanup failure
/// is logged with the path) before the original error is propagated, so
/// a partial write never silently leaves a stale `.tmp` behind.
///
/// This is the raw rewrite primitive: it does **not** take the index
/// lock. Callers replacing the index based on a previously read snapshot
/// must hold the lock across read and rewrite (as
/// [`update_index_entry`] and [`remove_index_entry`] do) or a concurrent
/// [`append_index_entry`] from another process can be dropped.
pub fn write_index_atomic(
    data_dir: &Path,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    let root = PrivateRoot::create(data_dir)?;
    write_index_atomic_in(&root, entries)
}

fn write_index_atomic_in(
    root: &PrivateRoot,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    for entry in entries {
        validate_index_entry(entry)?;
    }
    let final_path = Path::new(INDEX_FILE_NAME);
    let tmp_name = format!("index.jsonl.tmp.{}", Uuid::new_v4());
    let tmp_path = Path::new(&tmp_name);

    if let Err(err) = write_jsonl_atomic(root, tmp_path, entries) {
        remove_tmp_after_failure(root, tmp_path);
        return Err(err);
    }
    if let Err(err) = root.rename(tmp_path, final_path) {
        remove_tmp_after_failure(root, tmp_path);
        return Err(SessionPersistError::Io(err));
    }
    Ok(())
}

/// Best-effort removal of a temporary index file left by a failed
/// write or rename. A cleanup failure is logged with the path so a
/// lingering `.tmp` is never silent; `NotFound` is fine (the failure
/// may have happened before the tmp file was created).
fn remove_tmp_after_failure(root: &PrivateRoot, tmp_path: &Path) {
    if let Err(cleanup_error) = root.remove_file(tmp_path)
        && cleanup_error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            tmp_path = %root.display_path(tmp_path).display(),
            %cleanup_error,
            "failed to remove temporary session index file after a \
             failed atomic rewrite; remove it manually if it lingers",
        );
    }
}

fn write_jsonl_atomic<T: Serialize>(
    root: &PrivateRoot,
    tmp_path: &Path,
    rows: &[T],
) -> Result<(), SessionPersistError> {
    let file = root.create_new(tmp_path)?;
    let mut writer = BufWriter::new(file);
    for row in rows {
        serde_json::to_writer(&mut writer, row)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(())
}

/// Append `entry` to the session index in `O_APPEND` mode, holding the
/// inter-process index lock so the append cannot interleave with a
/// concurrent read-modify-rewrite from another process.
///
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely;
/// exceeding a deadline returns
/// [`SessionPersistError::IndexLockTimeout`] with nothing written).
pub fn append_index_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    append_entry_assuming_locked(lock.root(), entry)
}

/// Insert `entry` into the session index unless an entry with the same
/// `id` already exists, holding the inter-process index lock across the
/// existence check and the append so two processes (or threads) racing
/// the same ID can never both insert (the idempotent
/// open-or-resume primitive — see
/// [`SessionManager::open_or_resume`](crate::session::SessionManager::open_or_resume)).
///
/// Returns `Ok(Some(existing))` with the already-indexed entry when the
/// ID is taken (nothing is written), and `Ok(None)` when `entry` was
/// appended.
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely).
pub fn insert_index_entry_if_absent(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<Option<SessionIndexEntry>, SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    if let Some(existing) = read_index_in(lock.root())?
        .into_iter()
        .find(|e| e.id == entry.id)
    {
        return Ok(Some(existing));
    }
    append_entry_assuming_locked(lock.root(), entry)?;
    Ok(None)
}

/// Insert `entry` for a **brand-new** session, refusing typed when the
/// ID is already in use — in the index *or* as an orphan `{id}.jsonl`
/// session file on disk (an index wipe/restore or a hand-copied file
/// leaves exactly that state, and the sink would otherwise silently
/// **append** to the foreign history). Both checks and the append hold
/// the inter-process index lock, so two racing creates with the same ID
/// can never both succeed (the create-exactly-this primitive — see
/// [`SessionManager::create_with_id`](crate::session::SessionManager::create_with_id)).
///
/// # Errors
///
/// [`SessionPersistError::IdExists`] when the ID is taken (nothing is
/// written), plus index I/O failures.
///
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely).
pub fn insert_index_entry_for_new_session(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    if read_index_in(lock.root())?.iter().any(|e| e.id == entry.id)
        || lock
            .root()
            .regular_file_exists(Path::new(&format!("{}.jsonl", entry.id)))?
    {
        return Err(SessionPersistError::IdExists {
            id: entry.id.clone(),
        });
    }
    append_entry_assuming_locked(lock.root(), entry)
}

/// Insert the index row for a freshly minted **child** session, refusing
/// typed when the child's id or its on-disk location is already claimed.
/// The whole check-and-append holds the inter-process index lock, so two
/// processes minting into the same data directory can never both claim a
/// row (the in-process allocation authority is the parent's
/// [`SessionBinding`](crate::session::SessionBinding) lock; this is the
/// cross-process half).
///
/// # Errors
///
/// [`SessionPersistError::IdExists`] when a row with the same id exists,
/// [`SessionPersistError::ChildPathOccupied`] when any row already claims
/// the same `rel_path` (an orphan row from external tampering — the mint
/// must never adopt or overwrite it), plus index I/O failures.
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely).
pub fn insert_child_index_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    let existing = read_index_in(lock.root())?;
    if existing.iter().any(|e| e.id == entry.id) {
        return Err(SessionPersistError::IdExists {
            id: entry.id.clone(),
        });
    }
    if let Some(rel_path) = entry.rel_path.as_deref()
        && existing
            .iter()
            .any(|e| e.rel_path.as_deref() == Some(rel_path))
    {
        return Err(SessionPersistError::ChildPathOccupied {
            rel_path: rel_path.to_owned(),
        });
    }
    append_entry_assuming_locked(lock.root(), entry)
}

/// The raw `O_APPEND` index-entry write shared by [`append_index_entry`]
/// and [`insert_index_entry_if_absent`]. The caller MUST already hold the
/// inter-process index lock — the lock is not re-entrant (each
/// acquisition opens its own file description), so taking it here again
/// would deadlock.
///
/// Rejects entries whose `id` is reserved by the persistence layer
/// ([`SessionPersistError::InvalidSessionId`]): an indexed reserved id
/// would later route session I/O onto a persistence-owned file (e.g.
/// `delete("index")` removing the index itself), so it must never enter
/// the index through any insertion path.
fn append_entry_assuming_locked(
    root: &PrivateRoot,
    entry: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    validate_index_entry(entry)?;
    let file = root.open_append_create(Path::new(INDEX_FILE_NAME))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, entry)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(())
}

fn validate_index_entry(entry: &SessionIndexEntry) -> Result<(), SessionPersistError> {
    ensure_session_id_path_safe(&entry.id)?;
    session_file_relative(entry)?;
    if let Some(parent_id) = entry.parent_id.as_deref() {
        ensure_session_id_path_safe(parent_id)?;
    }
    Ok(())
}

/// Mutate the index entry matching `session_id` via `mutator`, then
/// rewrite the index atomically. The whole read-modify-rewrite holds the
/// inter-process index lock so concurrent creates and updates from other
/// processes are never lost to a stale-snapshot rewrite (H18). Returns
/// [`SessionPersistError::NotFound`] when no entry matches.
///
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely;
/// exceeding a deadline returns
/// [`SessionPersistError::IndexLockTimeout`] with the index untouched).
pub fn update_index_entry(
    data_dir: &Path,
    session_id: &str,
    lock_deadline: Option<Duration>,
    mutator: impl FnOnce(&mut SessionIndexEntry),
) -> Result<(), SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    let mut entries = read_index_in(lock.root())?;
    let pos = entries
        .iter()
        .position(|e| e.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    mutator(&mut entries[pos]);
    write_index_atomic_in(lock.root(), &entries)
}

/// Update the session index entry for `session_id` to reflect newly
/// persisted events without touching the session JSONL file.
///
/// Use it when events were already durably persisted through a sink (the
/// `JsonlSink` write-through path) and only the index needs to catch up —
/// calling `append_events` in that case double-writes every event and
/// breaks resume. The index-registered sink every
/// [`SessionManager`](crate::session::SessionManager) open installs
/// performs this update itself (batched per its `DurabilityPolicy`,
/// flushed by `EventStore::checkpoint` and on drop), so embedders must
/// **not** call this function as well — doing so double-counts every
/// event. It remains public for consumers reconciling
/// externally-written files.
///
/// `new_event_count` is the number of events landed since the last
/// index update. `usage_delta` is the cumulative token usage for those
/// events; only `input_tokens`, `output_tokens`, and `cache_read_tokens`
/// are recorded — other [`Usage`] fields are ignored because the index
/// schema does not store them.
///
/// When `new_event_count` is zero AND every relevant Usage field is
/// zero, the call is a no-op that returns `Ok(())` without touching the
/// index. The index entry MUST already exist; a missing entry returns
/// [`SessionPersistError::NotFound`].
///
/// `lock_deadline` bounds the index-lock wait (`None` = wait
/// indefinitely).
pub fn update_session_index(
    data_dir: &Path,
    session_id: &str,
    new_event_count: u64,
    usage_delta: &Usage,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    if new_event_count == 0
        && usage_delta.input_tokens == 0
        && usage_delta.output_tokens == 0
        && usage_delta.cache_read_tokens == 0
    {
        return Ok(());
    }
    update_index_entry(data_dir, session_id, lock_deadline, |entry| {
        entry.event_count = entry.event_count.saturating_add(new_event_count);
        entry.updated_at = Utc::now();
        entry.total_input_tokens = entry
            .total_input_tokens
            .saturating_add(usage_delta.input_tokens);
        entry.total_output_tokens = entry
            .total_output_tokens
            .saturating_add(usage_delta.output_tokens);
        entry.total_cache_read_tokens = entry
            .total_cache_read_tokens
            .saturating_add(usage_delta.cache_read_tokens);
    })
}

/// Sum `AssistantMessage` usage fields across `events` into a single
/// [`Usage`]. Non-assistant events contribute zero. Only the three
/// fields the session index tracks (`input_tokens`, `output_tokens`,
/// `cache_read_tokens`) are populated; `cache_write_tokens` and
/// `cost_usd` are left at their defaults.
#[must_use]
pub fn sum_usage_from_events(events: &[SessionEvent]) -> Usage {
    let mut total = Usage::default();
    for event in events {
        if let SessionEvent::AssistantMessage { usage, .. } = event {
            total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
            total.cache_read_tokens = total
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
        }
    }
    total
}

/// Resolve a user-supplied identifier (empty = latest, full ID, name, or
/// >=8-character ID prefix) against the index.
///
/// An entry whose `id` is reserved by the persistence layer can only exist
/// through a hand-edited index (every insertion path rejects reserved IDs).
/// [`read_index`] discards that unsafe row, so resolving it returns
/// [`SessionPersistError::NotFound`] rather than handing it to a caller that
/// could route session I/O onto a persistence-owned file.
pub fn resolve_session(
    data_dir: &Path,
    input: &str,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entry = resolve_in_entries(read_index(data_dir)?, input)?;
    ensure_session_id_path_safe(&entry.id)?;
    Ok(entry)
}

/// Resolve the most recently updated session whose indexed working
/// directory matches `working_dir`.
///
/// The exact stored path is checked first so non-existent historical
/// working directories can still match their original string. When both
/// paths can be canonicalised, the canonical forms are compared as well
/// so symlinked or syntactically different references to the same
/// directory resolve to the same session.
pub fn resolve_latest_session_in_working_dir(
    data_dir: &Path,
    working_dir: &Path,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entries = read_index(data_dir)?;
    let entry = resolve_latest_in_working_dir_entries(entries, working_dir)?;
    ensure_session_id_path_safe(&entry.id)?;
    Ok(entry)
}

fn resolve_latest_in_working_dir_entries(
    entries: Vec<SessionIndexEntry>,
    working_dir: &Path,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let canonical_working_dir = fs::canonicalize(working_dir).ok();
    entries
        .into_iter()
        .filter(|entry| {
            working_dir_matches(
                &entry.working_dir,
                working_dir,
                canonical_working_dir.as_deref(),
            )
        })
        .max_by_key(|entry| entry.updated_at)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: format!(
                "<no sessions in working directory {}>",
                working_dir.display()
            ),
        })
}

fn working_dir_matches(
    stored: &str,
    working_dir: &Path,
    canonical_working_dir: Option<&Path>,
) -> bool {
    let stored_path = Path::new(stored);
    if stored_path == working_dir {
        return true;
    }

    if let Some(canonical_working_dir) = canonical_working_dir
        && let Ok(canonical_stored) = fs::canonicalize(stored_path)
    {
        return canonical_stored == canonical_working_dir;
    }

    false
}

/// The resolution rules of [`resolve_session`], over an already-read
/// index snapshot.
fn resolve_in_entries(
    entries: Vec<SessionIndexEntry>,
    input: &str,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return entries
            .into_iter()
            .max_by_key(|e| e.updated_at)
            .ok_or_else(|| SessionPersistError::NotFound {
                input: "<no sessions>".to_owned(),
            });
    }

    if let Some(entry) = entries.iter().find(|e| e.id == trimmed) {
        return Ok(entry.clone());
    }
    if let Some(entry) = entries.iter().find(|e| e.name.as_deref() == Some(trimmed)) {
        return Ok(entry.clone());
    }

    if trimmed.len() < 8 {
        return Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        });
    }

    let matches: Vec<&SessionIndexEntry> = entries
        .iter()
        .filter(|e| e.id.starts_with(trimmed))
        .collect();
    match matches.as_slice() {
        [] => Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        }),
        [only] => Ok((*only).clone()),
        many => Err(SessionPersistError::AmbiguousPrefix {
            prefix: trimmed.to_owned(),
            matches: many.iter().map(|e| e.id.clone()).collect(),
        }),
    }
}

/// Remove the entry with `session_id` from the index, holding the
/// inter-process index lock across the read-modify-rewrite. Returns
/// [`SessionPersistError::NotFound`] when no entry matches.
///
/// `lock_deadline` bounds the lock wait (`None` = wait indefinitely).
pub fn remove_index_entry(
    data_dir: &Path,
    session_id: &str,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    let mut entries = read_index_in(lock.root())?;
    let pos = entries
        .iter()
        .position(|e| e.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    entries.remove(pos);
    write_index_atomic_in(lock.root(), &entries)
}

#[cfg(test)]
#[path = "index_security_tests.rs"]
mod security_tests;
