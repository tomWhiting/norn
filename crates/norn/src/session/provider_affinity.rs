//! Provider-state affinity owned by an event store.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;

use crate::provider::ProviderStateIdentity;

use super::events::{EventBase, EventId, ProviderEpochBoundaryReason, SessionEvent};
use super::persistence::index::{
    ProviderAffinityTransition, validate_or_bind_provider_state_identity,
};
use super::{SessionIndexEntry, SessionPersistError};

#[derive(Clone, Debug)]
pub(super) struct ManagedProviderAffinity {
    data_dir: PathBuf,
    registered: SessionIndexEntry,
    lock_deadline: Option<Duration>,
}

impl ManagedProviderAffinity {
    pub(super) fn new(
        data_dir: PathBuf,
        registered: SessionIndexEntry,
        lock_deadline: Option<Duration>,
    ) -> Self {
        Self {
            data_dir,
            registered,
            lock_deadline,
        }
    }

    fn validate_or_bind(
        &self,
        requested: Option<ProviderStateIdentity>,
    ) -> Result<(Option<ProviderStateIdentity>, Option<SessionEvent>), SessionPersistError> {
        let binding = validate_or_bind_provider_state_identity(
            &self.data_dir,
            &self.registered,
            requested,
            self.lock_deadline,
        )?;
        let boundary = match binding.transition {
            ProviderAffinityTransition::Validated => None,
            ProviderAffinityTransition::Adopted(boundary) => Some(*boundary),
            ProviderAffinityTransition::AlreadyBoundByPeer => {
                return Err(SessionPersistError::ProviderStateIdentityReopenRequired);
            }
        };
        Ok((binding.entry.provider_state_identity, boundary))
    }
}

pub(super) struct ProviderAffinity {
    identity: OnceLock<ProviderStateIdentity>,
    managed: Option<ManagedProviderAffinity>,
    transition: Mutex<Option<SessionEvent>>,
}

impl ProviderAffinity {
    pub(super) const fn sinkless() -> Self {
        Self {
            identity: OnceLock::new(),
            managed: None,
            transition: Mutex::new(None),
        }
    }

    pub(super) fn managed(authority: ManagedProviderAffinity) -> Self {
        let identity = authority
            .registered
            .provider_state_identity
            .map_or_else(OnceLock::new, OnceLock::from);
        Self {
            identity,
            managed: Some(authority),
            transition: Mutex::new(None),
        }
    }

    pub(super) fn identity(&self) -> Option<ProviderStateIdentity> {
        self.identity.get().copied()
    }

    pub(super) fn validate_or_bind(
        &self,
        requested: Option<ProviderStateIdentity>,
        unmanaged_adoption_parent: Option<&EventId>,
        publish_unmanaged_adoption: impl FnOnce(&SessionEvent) -> Result<(), SessionPersistError>,
    ) -> Result<Option<SessionEvent>, SessionPersistError> {
        let mut pending_adoption = self.transition.lock();
        if let Some(bound) = self.identity() {
            validate_bound(bound, requested)?;
            return Ok(None);
        }

        let (durable, adoption_boundary) = if let Some(authority) = &self.managed {
            authority.validate_or_bind(requested)?
        } else {
            let Some(identity) = requested else {
                return Ok(None);
            };
            let adoption = pending_adoption.take().or_else(|| {
                unmanaged_adoption_parent.map(|previous_id| SessionEvent::ProviderEpochBoundary {
                    base: EventBase::new(Some(previous_id.clone())),
                    reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
                })
            });
            if let Some(event) = adoption {
                if event.base().parent_id.as_ref() != unmanaged_adoption_parent {
                    *pending_adoption = Some(event);
                    return Err(SessionPersistError::EventStore(
                        "event store changed while provider identity adoption was pending"
                            .to_owned(),
                    ));
                }
                if let Err(error) = publish_unmanaged_adoption(&event) {
                    *pending_adoption = Some(event);
                    return Err(error);
                }
            }
            (Some(identity), None)
        };
        let Some(identity) = durable else {
            return Ok(None);
        };
        match self.identity.set(identity) {
            Ok(()) => Ok(adoption_boundary),
            Err(candidate) => {
                validate_bound(
                    self.identity()
                        .ok_or(SessionPersistError::ProviderStateIdentityMismatch)?,
                    Some(candidate),
                )?;
                Ok(adoption_boundary)
            }
        }
    }
}

fn validate_bound(
    bound: ProviderStateIdentity,
    requested: Option<ProviderStateIdentity>,
) -> Result<(), SessionPersistError> {
    match requested {
        Some(candidate) if candidate == bound => Ok(()),
        Some(_) => Err(SessionPersistError::ProviderStateIdentityMismatch),
        None => Err(SessionPersistError::ProviderStateIdentityRequired),
    }
}
