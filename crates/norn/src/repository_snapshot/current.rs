//! Complete current-tree acquisition and stability seals.

use std::collections::BTreeMap;

use norn_policy::{CompleteCurrentSnapshot, OwnedSnapshot, RepositoryPath};

use super::error::SnapshotAdapterError;
use super::git::{GitInventory, GitRunner};
use super::workspace::{WorkspaceObservation, WorkspaceRoot};

pub(super) struct CurrentAcquisition {
    pub(super) snapshot: CompleteCurrentSnapshot,
    pub(super) seal: CurrentSnapshotSeal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CurrentSnapshotSeal {
    inventory: GitInventory,
    observations: BTreeMap<RepositoryPath, WorkspaceObservation>,
}

pub(super) fn acquire(
    workspace: &WorkspaceRoot,
    git: &GitRunner,
) -> Result<CurrentAcquisition, SnapshotAdapterError> {
    let inventory_a = stable_inventory(workspace, git)?;
    let observations_a = observe_inventory(workspace, &inventory_a)?;
    let inventory_b = stable_inventory(workspace, git)?;
    if inventory_a != inventory_b {
        return Err(SnapshotAdapterError::SnapshotChanged);
    }
    let observations_b = observe_inventory(workspace, &inventory_b)?;
    if observations_a != observations_b {
        return Err(SnapshotAdapterError::SnapshotChanged);
    }
    let inventory_c = stable_inventory(workspace, git)?;
    if inventory_b != inventory_c {
        return Err(SnapshotAdapterError::SnapshotChanged);
    }

    let entries = observations_b.iter().filter_map(|(path, observation)| {
        observation
            .entry()
            .map(|entry| (path.clone(), entry.clone()))
    });
    let snapshot = OwnedSnapshot::try_from_entries(entries)
        .map_err(|error| snapshot_structure_error(&error))?;
    let snapshot = if inventory_c.marker_observed() {
        CompleteCurrentSnapshot::from_complete_snapshot_with_marker_history(snapshot)
    } else {
        CompleteCurrentSnapshot::from_complete_snapshot(snapshot)
    };
    Ok(CurrentAcquisition {
        snapshot,
        seal: CurrentSnapshotSeal {
            inventory: inventory_c,
            observations: observations_b,
        },
    })
}

fn snapshot_structure_error(error: &norn_policy::SnapshotError) -> SnapshotAdapterError {
    match error {
        norn_policy::SnapshotError::DuplicateEntry { .. }
        | norn_policy::SnapshotError::DuplicateMutation { .. }
        | norn_policy::SnapshotError::CreateTargetExists { .. }
        | norn_policy::SnapshotError::MutationTargetMissing { .. }
        | norn_policy::SnapshotError::DescendantBeneathEntry { .. } => {
            SnapshotAdapterError::SnapshotStructure
        }
    }
}

pub(super) fn revalidate(
    workspace: &WorkspaceRoot,
    git: &GitRunner,
    expected: &CurrentSnapshotSeal,
) -> Result<(), SnapshotAdapterError> {
    let observed = acquire(workspace, git)?;
    if &observed.seal != expected {
        return Err(SnapshotAdapterError::SnapshotChanged);
    }
    Ok(())
}

fn stable_inventory(
    workspace: &WorkspaceRoot,
    git: &GitRunner,
) -> Result<GitInventory, SnapshotAdapterError> {
    workspace.verify_named_identity()?;
    let inventory = git.current_inventory()?;
    workspace.verify_named_identity()?;
    Ok(inventory)
}

fn observe_inventory(
    workspace: &WorkspaceRoot,
    inventory: &GitInventory,
) -> Result<BTreeMap<RepositoryPath, WorkspaceObservation>, SnapshotAdapterError> {
    inventory
        .paths()
        .map(|path| workspace.observe(path).map(|entry| (path.clone(), entry)))
        .collect()
}
