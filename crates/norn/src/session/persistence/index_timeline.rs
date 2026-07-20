//! Index-first transactions that also mutate registered timeline paths.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;

use crate::provider::ProviderStateIdentity;
use crate::session::events::SessionEvent;
use crate::session::events::{EventBase, ProviderEpochBoundaryReason};
use crate::util::PrivateFileIdentity;

#[cfg(test)]
use super::super::io::{ensure_session_id_not_reserved, serialize_events};
use super::super::io::{retry_prefix_from_file, session_file_relative};
use super::super::timeline_file::{
    ExistingEventInspection, open_existing_for_append, open_session_append_bound_under,
};
use super::super::timeline_lock::{LockedTimelineFile, TimelineLockGuard};
use super::super::types::{SessionIndexEntry, SessionPersistError};

/// Result of validating or durably adopting a provider-state identity.
pub(crate) struct ProviderAffinityBinding {
    pub(crate) entry: SessionIndexEntry,
    /// The durable adoption boundary a previously loaded store must mirror in
    /// memory before it can construct another provider request.
    pub(crate) adoption_boundary: Option<SessionEvent>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AffinityBindingCheckpoint {
    BoundaryDurable,
}

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
    provider_state_identity: Option<ProviderStateIdentity>,
    candidate_id: &str,
    candidate_line: &[u8],
    lock_deadline: Option<Duration>,
) -> Result<(LockedTimelineFile, ExistingEventInspection), SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let entries = super::codec::read_index_in(index_lock.root())?;
    let position = super::registered_position(&entries, registered)?;
    if entries[position].provider_state_identity != provider_state_identity {
        return Err(SessionPersistError::ProviderStateIdentityMismatch);
    }
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

/// Bind one managed session generation to a provider identity exactly once,
/// or validate its existing binding.
///
/// A row that was observed unbound cannot safely attribute historical response
/// anchors to the first identity it adopts. Under the canonical index-then-
/// timeline lock order, this transaction fsyncs a dedicated epoch boundary
/// before publishing the identity-bearing index row. A terminated writer can
/// therefore leave the row unbound or leave an extra durable boundary, but can
/// never leave a bound row whose old provider anchor remains active.
pub(crate) fn validate_or_bind_provider_state_identity(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    requested: Option<ProviderStateIdentity>,
    lock_deadline: Option<Duration>,
) -> Result<ProviderAffinityBinding, SessionPersistError> {
    validate_or_bind_provider_state_identity_inner(
        data_dir,
        registered,
        requested,
        lock_deadline,
        &mut || Ok(()),
    )
}

#[cfg(test)]
pub(crate) fn validate_or_bind_provider_state_identity_with_hook(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    requested: Option<ProviderStateIdentity>,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(AffinityBindingCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<ProviderAffinityBinding, SessionPersistError> {
    validate_or_bind_provider_state_identity_inner(
        data_dir,
        registered,
        requested,
        lock_deadline,
        &mut || checkpoint(AffinityBindingCheckpoint::BoundaryDurable),
    )
}

fn validate_or_bind_provider_state_identity_inner(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    requested: Option<ProviderStateIdentity>,
    lock_deadline: Option<Duration>,
    boundary_durable: &mut impl FnMut() -> Result<(), SessionPersistError>,
) -> Result<ProviderAffinityBinding, SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let mut entries = super::codec::read_index_in(index_lock.root())?;
    let position = super::registered_position(&entries, registered)?;
    let current = entries[position].provider_state_identity;

    if registered.provider_state_identity.is_some() && registered.provider_state_identity != current
    {
        return Err(SessionPersistError::ProviderStateIdentityMismatch);
    }

    match (current, requested) {
        (Some(bound), Some(candidate)) if bound == candidate => {
            if registered.provider_state_identity.is_some() {
                return Ok(ProviderAffinityBinding {
                    entry: entries[position].clone(),
                    adoption_boundary: None,
                });
            }
        }
        (Some(_), Some(_)) => return Err(SessionPersistError::ProviderStateIdentityMismatch),
        (Some(_), None) => return Err(SessionPersistError::ProviderStateIdentityRequired),
        (None, None) => {
            return Ok(ProviderAffinityBinding {
                entry: entries[position].clone(),
                adoption_boundary: None,
            });
        }
        (None, Some(_)) => {}
    }

    let candidate = requested.ok_or(SessionPersistError::ProviderStateIdentityRequired)?;
    let relative = session_file_relative(&entries[position])?;
    let timeline_lock = TimelineLockGuard::acquire_under(index_lock.root(), &relative)?;
    let mut file = open_existing_for_append(index_lock.root(), &relative)?;
    let display_path = index_lock.root().display_path(&relative);
    let facts = retry_prefix_from_file(&mut file, &display_path, &[])?;
    let (boundary, exact) = match facts.tail {
        Some(event) if is_provider_identity_adoption(&event) => (event, facts.counters),
        tail => {
            let event = SessionEvent::ProviderEpochBoundary {
                base: EventBase::new(tail.map(|event| event.base().id.clone())),
                reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
            };
            let exact = facts.counters.checked_with(&event).map_err(|overflow| {
                SessionPersistError::IndexCounterOverflow {
                    id: registered.id.clone(),
                    field: overflow.field(),
                }
            })?;
            let mut encoded = Vec::new();
            serde_json::to_writer(&mut encoded, &event)?;
            encoded.push(b'\n');
            file.write_all(&encoded)?;
            file.sync_all()?;
            (event, exact)
        }
    };
    boundary_durable()?;
    exact.apply_to(&mut entries[position]);
    entries[position].provider_state_identity = Some(candidate);
    entries[position].updated_at = Utc::now();
    let entry = entries[position].clone();
    drop(file);
    drop(timeline_lock);
    super::codec::write_index_atomic_in(index_lock.root(), &entries)?;
    Ok(ProviderAffinityBinding {
        entry,
        adoption_boundary: Some(boundary),
    })
}

fn is_provider_identity_adoption(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
            ..
        }
    )
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
