use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::publication_conflict::conflict;
use super::{SessionIndexEntry, SessionPersistError};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParentPrecondition {
    id: String,
    generation: Uuid,
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
    Ok(ParentPrecondition { id, generation })
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
    if entries
        .iter()
        .any(|entry| entry.id == parent.id && entry.generation == parent.generation)
    {
        Ok(())
    } else {
        Err(SessionPersistError::GenerationChanged {
            id: parent.id.clone(),
        })
    }
}
