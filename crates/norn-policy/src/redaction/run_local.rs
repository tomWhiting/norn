//! Ephemeral authority for one exact ignored P1 gate run.

use crate::OwnedSnapshot;

use super::authoring::RedactionAuthoringError;
use super::{RedactionRegistry, gate_evidence, validate_retained_artifacts};

pub(super) fn author(
    checked_registry: &RedactionRegistry,
    checked_snapshot: &OwnedSnapshot,
    run_snapshot: &OwnedSnapshot,
) -> Result<RedactionRegistry, RedactionAuthoringError> {
    if !validate_retained_artifacts(checked_registry, checked_snapshot).is_empty() {
        return Err(RedactionAuthoringError::CheckedAuthorityValidation);
    }
    let canonical_checked = super::authoring::author_checked_tree(checked_snapshot)?;
    if canonical_checked != *checked_registry {
        return Err(RedactionAuthoringError::CheckedAuthorityValidation);
    }
    let local_registry = gate_evidence::derive_target(checked_snapshot, run_snapshot)?;
    if local_registry.synthetics().len() != 0 {
        return Err(RedactionAuthoringError::RunLocalValidation);
    }

    let combined_registry = checked_registry.combined(&local_registry)?;
    let combined_snapshot = OwnedSnapshot::try_from_entries(
        checked_snapshot
            .iter()
            .chain(run_snapshot.iter())
            .map(|(path, entry)| (path.clone(), entry.clone())),
    )
    .map_err(|_| RedactionAuthoringError::CombinedSnapshot)?;
    if !validate_retained_artifacts(&combined_registry, &combined_snapshot).is_empty() {
        return Err(RedactionAuthoringError::RunLocalValidation);
    }
    Ok(local_registry)
}
