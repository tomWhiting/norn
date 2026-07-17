use std::fs::File;
use std::path::{Path, PathBuf};

use crate::session::events::{EventBase, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::persistence::{
    ResumeFidelity, SessionIndexEntry, SessionPersistError, SessionRecordOrigin,
};
use crate::session::store::EventStore;
use crate::util::PrivateRoot;

const BOUNDARY_LOCK_DIRECTORY: &str = ".provider-epoch-locks";

/// Trusted caller policy for opening a persisted session for execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumePolicy {
    /// Permit only a canonical visible-history representation.
    RequireCanonical,
    /// Explicitly approve a degraded projection and begin a fresh provider
    /// epoch before execution.
    ApproveFreshEpochProjection,
}

pub(super) fn authorize_resume(
    entry: &SessionIndexEntry,
    policy: ResumePolicy,
) -> Result<(), SessionPersistError> {
    match entry.fidelity {
        ResumeFidelity::Canonical => Ok(()),
        ResumeFidelity::FreshEpochProjection
            if policy == ResumePolicy::ApproveFreshEpochProjection =>
        {
            Ok(())
        }
        ResumeFidelity::FreshEpochProjection => Err(SessionPersistError::ResumeApprovalRequired {
            id: entry.id.clone(),
        }),
        ResumeFidelity::InspectOnly => Err(SessionPersistError::SessionNotResumable {
            id: entry.id.clone(),
        }),
    }
}

pub(super) struct MigratedEpochGuard {
    _lock: File,
    _permit: crate::resource::DescriptorPermit,
}

pub(super) fn lock_migrated_epoch(
    data_dir: &Path,
    entry: &SessionIndexEntry,
) -> Result<Option<MigratedEpochGuard>, SessionPersistError> {
    if !matches!(entry.origin, SessionRecordOrigin::MigratedLegacy { .. }) {
        return Ok(None);
    }
    let permit = crate::session::persistence::acquire_private_fs()?;
    let root = PrivateRoot::open(data_dir)?;
    root.create_dir_all(Path::new(BOUNDARY_LOCK_DIRECTORY))?;
    let relative = PathBuf::from(BOUNDARY_LOCK_DIRECTORY).join(format!("{}.lock", entry.id));
    let lock = root.open_lock(&relative)?;
    lock.lock()?;
    Ok(Some(MigratedEpochGuard {
        _lock: lock,
        _permit: permit,
    }))
}

pub(super) fn ensure_migrated_epoch_boundary(
    entry: &SessionIndexEntry,
    store: &EventStore,
) -> Result<bool, SessionPersistError> {
    if !matches!(entry.origin, SessionRecordOrigin::MigratedLegacy { .. }) {
        return Ok(false);
    }
    let (boundary_count, parent_id) = store.with_events(|events| {
        let count = events
            .iter()
            .filter(|event| is_migrated_boundary(event))
            .count();
        let parent = events.last().map(|event| event.base().id.clone());
        (count, parent)
    });
    match boundary_count {
        0 => {
            store.append(SessionEvent::ProviderEpochBoundary {
                base: EventBase::new(parent_id),
                reason: ProviderEpochBoundaryReason::MigratedLegacy,
            })?;
            store.checkpoint()?;
            Ok(true)
        }
        1 => Ok(false),
        count => Err(duplicate_boundary_error(entry, count)),
    }
}

pub(super) fn ensure_migrated_epoch_boundary_in_events(
    entry: &SessionIndexEntry,
    events: &mut Vec<SessionEvent>,
) -> Result<(), SessionPersistError> {
    if !matches!(entry.origin, SessionRecordOrigin::MigratedLegacy { .. }) {
        return Ok(());
    }
    let count = events
        .iter()
        .filter(|event| is_migrated_boundary(event))
        .count();
    match count {
        0 => {
            let parent_id = events.last().map(|event| event.base().id.clone());
            events.push(SessionEvent::ProviderEpochBoundary {
                base: EventBase::new(parent_id),
                reason: ProviderEpochBoundaryReason::MigratedLegacy,
            });
            Ok(())
        }
        1 => Ok(()),
        count => Err(duplicate_boundary_error(entry, count)),
    }
}

fn is_migrated_boundary(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::MigratedLegacy,
            ..
        }
    )
}

fn duplicate_boundary_error(entry: &SessionIndexEntry, count: usize) -> SessionPersistError {
    SessionPersistError::EventStore(format!(
        "session '{}' contains {count} migrated-legacy provider epoch boundaries",
        entry.id
    ))
}
