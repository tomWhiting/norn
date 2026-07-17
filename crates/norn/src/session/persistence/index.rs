//! Session index maintenance for the active format-2 store.
//!
//! Every mutation takes the separate inter-process index lock across its
//! complete read-modify-rewrite transaction. The canonical index is never
//! opened with `O_APPEND`: each successful mutation publishes a validated
//! header-plus-rows replacement atomically.

use std::path::Path;
use std::time::Duration;

#[cfg(test)]
use chrono::Utc;

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::util::PrivateRoot;

#[cfg(test)]
use super::IndexCounters;
use super::lock::{IndexLock, lock_index};
use super::types::{SessionIndexEntry, SessionPersistError};

#[path = "index_codec.rs"]
mod codec;
#[path = "index_deletion.rs"]
mod deletion;
#[path = "index_deletion_recovery.rs"]
mod deletion_recovery;
#[path = "index_artifacts.rs"]
mod index_artifacts;
#[path = "publication.rs"]
mod publication;
#[path = "index_resolve.rs"]
mod resolve;
#[path = "index_timeline.rs"]
mod timeline;

#[cfg(test)]
pub(crate) use codec::{index_file_path, write_index_atomic};
pub(crate) use deletion::delete_session_transaction;
#[cfg(test)]
pub(crate) use deletion::{DeleteCheckpoint, delete_session_transaction_with_hook};
pub(crate) use publication::{publish_new_child_session, publish_new_session};
pub use resolve::{resolve_latest_session_in_working_dir, resolve_session};
pub(crate) use resolve::{
    resolve_latest_session_in_working_dir_with_deadline, resolve_session_with_deadline,
};
#[cfg(test)]
pub(crate) use timeline::append_events_transaction;
pub(crate) use timeline::{
    open_registered_timeline_bound, reconcile_registered_timeline, registered_timeline_identity,
    with_registered_timeline,
};

/// Read the complete active format-2 index after converging any durable
/// session-publication transactions left by a terminated writer.
///
/// Reads take the same inter-process lock as mutations because recovery may
/// need to publish a staged timeline and index row. An absent store remains a
/// non-mutating empty read: the preliminary descriptor-pinned open returns
/// before the lock can create the directory. This compatibility API waits
/// indefinitely for the lock; [`crate::session::SessionManager`] applies its
/// configured deadline through an internal deadline-aware variant.
pub fn read_index(data_dir: &Path) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    read_index_with_deadline(data_dir, None)
}

/// Deadline-aware index read used by [`crate::session::SessionManager`].
pub(crate) fn read_index_with_deadline(
    data_dir: &Path,
    lock_deadline: Option<Duration>,
) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    {
        let _permit = super::acquire_private_fs()?;
        match PrivateRoot::open(data_dir) {
            Ok(root) => drop(root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
    }
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    codec::read_index_in(lock.root())
}

fn lock_recovered_index(
    data_dir: &Path,
    lock_deadline: Option<Duration>,
) -> Result<IndexLock, SessionPersistError> {
    let lock = lock_index(data_dir, lock_deadline)?;
    index_artifacts::discard_temporary_indexes(lock.root())?;
    publication::recover_pending_publications(lock.root())?;
    deletion_recovery::recover_pending_deletions(lock.root())?;
    Ok(lock)
}

/// Insert `entry`, refusing an already-indexed identifier.
#[cfg(test)]
pub(crate) fn append_index_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    codec::validate_entry_path(entry)?;
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let entries = codec::read_index_in(lock.root())?;
    if entries.iter().any(|existing| existing.id == entry.id) {
        return Err(SessionPersistError::IdExists {
            id: entry.id.clone(),
        });
    }
    append_entry_assuming_locked(lock.root(), entries, entry)
}

/// Insert `entry` unless an entry with the same identifier already exists.
///
/// Returns the existing row without writing when the identifier is already
/// present. The read and full atomic rewrite share one lock acquisition.
#[cfg(test)]
pub(crate) fn insert_index_entry_if_absent(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<Option<SessionIndexEntry>, SessionPersistError> {
    codec::validate_entry_path(entry)?;
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let entries = codec::read_index_in(lock.root())?;
    if let Some(existing) = entries.iter().find(|existing| existing.id == entry.id) {
        return Ok(Some(existing.clone()));
    }
    append_entry_assuming_locked(lock.root(), entries, entry)?;
    Ok(None)
}

/// Insert the index row for a freshly minted child session.
#[cfg(test)]
pub(crate) fn insert_child_index_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    codec::validate_entry_path(entry)?;
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let entries = codec::read_index_in(lock.root())?;
    if entries.iter().any(|existing| existing.id == entry.id) {
        return Err(SessionPersistError::IdExists {
            id: entry.id.clone(),
        });
    }
    if let Some(rel_path) = entry.rel_path.as_deref()
        && entries
            .iter()
            .any(|existing| existing.rel_path.as_deref() == Some(rel_path))
    {
        return Err(SessionPersistError::ChildPathOccupied {
            rel_path: rel_path.to_owned(),
        });
    }
    append_entry_assuming_locked(lock.root(), entries, entry)
}

#[cfg(test)]
fn append_entry_assuming_locked(
    root: &PrivateRoot,
    mut entries: Vec<SessionIndexEntry>,
    entry: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    entries.push(entry.clone());
    codec::write_index_atomic_in(root, &entries)
}

/// Mutate one matching row and atomically rewrite the complete strict index.
#[cfg(test)]
pub(crate) fn update_index_entry(
    data_dir: &Path,
    session_id: &str,
    lock_deadline: Option<Duration>,
    mutator: impl FnOnce(&mut SessionIndexEntry),
) -> Result<(), SessionPersistError> {
    mutate_index_entry(data_dir, session_id, lock_deadline, |entry| {
        mutator(entry);
        Ok(())
    })
}

#[cfg(test)]
fn mutate_index_entry(
    data_dir: &Path,
    session_id: &str,
    lock_deadline: Option<Duration>,
    mutator: impl FnOnce(&mut SessionIndexEntry) -> Result<(), SessionPersistError>,
) -> Result<(), SessionPersistError> {
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let mut entries = codec::read_index_in(lock.root())?;
    let position = entries
        .iter()
        .position(|entry| entry.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    let generation = entries[position].generation;
    mutator(&mut entries[position])?;
    if entries[position].generation != generation {
        return Err(SessionPersistError::GenerationChanged {
            id: session_id.to_owned(),
        });
    }
    codec::write_index_atomic_in(lock.root(), &entries)
}

/// Re-read one previously resolved row under the recovered index lock and
/// prove that it still names the same immutable session incarnation.
pub(crate) fn revalidate_registered_entry(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let entries = codec::read_index_in(lock.root())?;
    let position = registered_position(&entries, registered)?;
    Ok(entries[position].clone())
}

/// Execute an artifact mutation while the registered session generation and
/// the index lock remain pinned. The caller reuses the index transaction's
/// descriptor admission and must not acquire a second private-fs permit.
pub(crate) fn with_registered_generation<T>(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
    operation: impl FnOnce(&PrivateRoot) -> Result<T, SessionPersistError>,
) -> Result<T, SessionPersistError> {
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let entries = codec::read_index_in(lock.root())?;
    registered_position(&entries, registered)?;
    operation(lock.root())
}

/// Conditionally mutate a row only while it remains the exact generation and
/// timeline path previously resolved by the caller.
pub(crate) fn update_registered_entry(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
    mutator: impl FnOnce(&mut SessionIndexEntry),
) -> Result<SessionIndexEntry, SessionPersistError> {
    let lock = lock_recovered_index(data_dir, lock_deadline)?;
    let mut entries = codec::read_index_in(lock.root())?;
    let position = registered_position(&entries, registered)?;
    mutator(&mut entries[position]);
    if entries[position].generation != registered.generation
        || entries[position].rel_path != registered.rel_path
    {
        return Err(SessionPersistError::GenerationChanged {
            id: registered.id.clone(),
        });
    }
    let updated = entries[position].clone();
    codec::write_index_atomic_in(lock.root(), &entries)?;
    Ok(updated)
}

fn registered_position(
    entries: &[SessionIndexEntry],
    registered: &SessionIndexEntry,
) -> Result<usize, SessionPersistError> {
    entries
        .iter()
        .position(|entry| {
            entry.id == registered.id
                && entry.generation == registered.generation
                && entry.rel_path == registered.rel_path
        })
        .ok_or_else(|| SessionPersistError::GenerationChanged {
            id: registered.id.clone(),
        })
}

/// Reconcile advisory event and usage counters for an existing row.
///
/// Timelines remain authoritative: these counters are a repairable listing
/// cache because a crash may occur after an event append and before this
/// separate atomic index update.
#[cfg(test)]
pub(crate) fn update_session_index(
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
    mutate_index_entry(data_dir, session_id, lock_deadline, |entry| {
        let delta = IndexCounters {
            event_count: new_event_count,
            total_input_tokens: usage_delta.input_tokens,
            total_output_tokens: usage_delta.output_tokens,
            total_cache_read_tokens: usage_delta.cache_read_tokens,
        };
        let updated = IndexCounters::from_entry(entry)
            .checked_add(delta)
            .map_err(|overflow| SessionPersistError::IndexCounterOverflow {
                id: entry.id.clone(),
                field: overflow.field(),
            })?;
        updated.apply_to(entry);
        entry.updated_at = Utc::now();
        Ok(())
    })
}

/// Sum index-tracked usage for an in-memory, non-authoritative history.
///
/// Totals cap at `u64::MAX`. Active format-2 persistence never calls this
/// convenience helper: strict reads and every durable mutation use checked
/// [`super::IndexCounters`] so a capped value cannot enter the session index.
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

#[cfg(test)]
#[path = "index_security_tests.rs"]
mod security_tests;

#[cfg(test)]
#[path = "index_strict_tests.rs"]
mod strict_tests;
