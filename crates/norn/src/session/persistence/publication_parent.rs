use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::ProviderStateIdentity;

use super::publication_conflict::conflict;
use super::{SessionIndexEntry, SessionPersistError};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParentPrecondition {
    id: String,
    generation: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_state_identity: Option<ProviderStateIdentity>,
}

pub(super) fn child_precondition(
    entry: &SessionIndexEntry,
    generation: Uuid,
) -> Result<ParentPrecondition, SessionPersistError> {
    let id = entry.parent_id.clone().ok_or_else(|| {
        conflict(
            &entry.id,
            "child publication requires an indexed parent identity",
        )
    })?;
    Ok(ParentPrecondition {
        id,
        generation,
        provider_state_identity: entry.provider_state_identity,
    })
}

pub(super) fn validate_parent_precondition_shape(
    entry: &SessionIndexEntry,
    parent: Option<&ParentPrecondition>,
) -> Result<(), SessionPersistError> {
    match (entry.parent_id.as_deref(), parent) {
        (None, None) => Ok(()),
        (Some(parent_id), Some(parent)) if parent.id == parent_id => Ok(()),
        (Some(_), None) => Err(conflict(
            &entry.id,
            "child publication journal lacks its durable parent precondition",
        )),
        (None, Some(_)) => Err(conflict(
            &entry.id,
            "root publication journal unexpectedly names a parent precondition",
        )),
        (Some(_), Some(_)) => Err(conflict(
            &entry.id,
            "publication parent precondition disagrees with the child index row",
        )),
    }
}

pub(super) fn validate_parent_generation(
    entries: &[SessionIndexEntry],
    parent: Option<&ParentPrecondition>,
) -> Result<(), SessionPersistError> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let current = entries
        .iter()
        .find(|entry| entry.id == parent.id && entry.generation == parent.generation)
        .ok_or_else(|| SessionPersistError::GenerationChanged {
            id: parent.id.clone(),
        })?;
    if current.provider_state_identity == parent.provider_state_identity {
        Ok(())
    } else {
        Err(SessionPersistError::ProviderStateIdentityMismatch)
    }
}
