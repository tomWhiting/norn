//! Index-first transactions that also mutate registered timeline paths.

#[cfg(test)]
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;

#[cfg(test)]
use crate::session::events::SessionEvent;
use crate::util::PrivateFileIdentity;

#[cfg(test)]
use super::super::io::{ensure_session_id_not_reserved, serialize_events};
use super::super::io::{retry_prefix_from_file, session_file_relative};
use super::super::timeline_file::{
    ExistingEventInspection, open_existing_for_append, open_session_append_bound_under,
};
use super::super::timeline_lock::{LockedTimelineFile, TimelineLockGuard};
use super::super::types::{SessionIndexEntry, SessionPersistError};

pub(crate) fn with_registered_timeline<T>(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
    operation: impl FnOnce(&crate::util::PrivateRoot, &Path) -> Result<T, SessionPersistError>,
) -> Result<T, SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let entries = super::codec::read_index_in(index_lock.root())?;
    let position = super::registered_position(&entries, registered)?;
    let relative = session_file_relative(&entries[position])?;
    let timeline_lock = TimelineLockGuard::acquire_under(index_lock.root(), &relative)?;
    let result = operation(index_lock.root(), &relative);
    drop(timeline_lock);
    result
}

pub(crate) fn registered_timeline_identity(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<PrivateFileIdentity, SessionPersistError> {
    with_registered_timeline(data_dir, registered, lock_deadline, |root, relative| {
        let file = open_existing_for_append(root, relative)?;
        Ok(PrivateFileIdentity::capture(&file)?)
    })
}

pub(crate) fn open_registered_timeline_bound(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    identity: PrivateFileIdentity,
    candidate_id: &str,
    candidate_line: &[u8],
    lock_deadline: Option<Duration>,
) -> Result<(LockedTimelineFile, ExistingEventInspection), SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let entries = super::codec::read_index_in(index_lock.root())?;
    let position = super::registered_position(&entries, registered)?;
    let relative = session_file_relative(&entries[position])?;
    let timeline_lock = TimelineLockGuard::acquire_under(index_lock.root(), &relative)?;
    let (file, inspection) = open_session_append_bound_under(
        index_lock.root(),
        &relative,
        identity,
        candidate_id,
        candidate_line,
    )?;
    Ok((
        LockedTimelineFile::new_registered(file, timeline_lock, index_lock),
        inspection,
    ))
}

#[cfg(test)]
pub(crate) fn append_events_transaction(
    data_dir: &Path,
    session_id: &str,
    events: &[SessionEvent],
) -> Result<(), SessionPersistError> {
    ensure_session_id_not_reserved(session_id)?;
    let index_lock = super::lock_recovered_index(data_dir, None)?;
    let mut entries = super::codec::read_index_in(index_lock.root())?;
    let position = entries
        .iter()
        .position(|entry| entry.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    let relative = session_file_relative(&entries[position])?;
    let timeline_lock = TimelineLockGuard::acquire_under(index_lock.root(), &relative)?;
    let mut file = open_existing_for_append(index_lock.root(), &relative)?;
    let display_path = index_lock.root().display_path(&relative);
    let facts = retry_prefix_from_file(&mut file, &display_path, events)?;
    let pending = &events[facts.retry_prefix..];
    let mut exact = facts.counters;
    for event in pending {
        exact = exact.checked_with(event).map_err(|overflow| {
            SessionPersistError::IndexCounterOverflow {
                id: session_id.to_owned(),
                field: overflow.field(),
            }
        })?;
    }
    let mut updated_entry = entries[position].clone();
    exact.apply_to(&mut updated_entry);
    if !pending.is_empty() {
        updated_entry.updated_at = Utc::now();
    }
    let index_changed = updated_entry != entries[position];
    let bytes = serialize_events(pending)?;
    if !bytes.is_empty() {
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    drop(file);
    drop(timeline_lock);

    if !index_changed {
        return Ok(());
    }
    entries[position] = updated_entry;
    if let Err(error) = super::codec::write_index_atomic_in(index_lock.root(), &entries) {
        tracing::error!(
            session_id,
            %error,
            appended = pending.len(),
            "session events are durable but index maintenance failed; resume will repair it",
        );
    }
    Ok(())
}

pub(crate) fn reconcile_registered_timeline(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    expected_identity: PrivateFileIdentity,
    lock_deadline: Option<Duration>,
) -> Result<(), SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let mut entries = super::codec::read_index_in(index_lock.root())?;
    let position = entries
        .iter()
        .position(|entry| entry.id == registered.id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: registered.id.clone(),
        })?;
    let current = &entries[position];
    if current.generation != registered.generation || current.rel_path != registered.rel_path {
        return Err(SessionPersistError::GenerationChanged {
            id: registered.id.clone(),
        });
    }
    let relative = session_file_relative(current)?;
    let timeline_lock = TimelineLockGuard::acquire_under(index_lock.root(), &relative)?;
    let mut file = open_existing_for_append(index_lock.root(), &relative)?;
    expected_identity.verify(&file).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            SessionPersistError::EventAppendConflict {
                event_id: registered.id.clone(),
                reason: "the registered session timeline changed identity",
            }
        } else {
            error.into()
        }
    })?;
    let display_path = index_lock.root().display_path(&relative);
    let facts = retry_prefix_from_file(&mut file, &display_path, &[])?;
    let mut updated = current.clone();
    facts.counters.apply_to(&mut updated);
    updated.updated_at = Utc::now();
    entries[position] = updated;
    drop(file);
    drop(timeline_lock);
    super::codec::write_index_atomic_in(index_lock.root(), &entries)?;
    Ok(())
}
