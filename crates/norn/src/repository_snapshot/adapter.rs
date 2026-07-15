//! Public acquired-snapshot boundary shared by CLI and runtime callers.

use std::path::Path;
use std::sync::Arc;

use norn_policy::{
    CompleteCurrentSnapshot, EntryKind, GitLeafMode, GitTreeLeaf, P1BaseSnapshot,
    P1EvaluationInput, SnapshotEntry,
};

use super::current::{self, CurrentSnapshotSeal};
use super::error::SnapshotAdapterError;
use super::git::GitRunner;
use super::git_batch::BlobStore;
use super::workspace::WorkspaceRoot;

/// One canonical Git workspace and its descriptor-pinned filesystem root.
pub struct RepositorySnapshotAdapter {
    workspace: WorkspaceRoot,
    git: GitRunner,
}

impl RepositorySnapshotAdapter {
    /// Discover, canonicalize, and pin the repository containing `start`.
    ///
    /// All Git subprocesses clear inherited `GIT_*` authority redirects and use
    /// `--no-replace-objects`. Present repositories fail closed on platforms
    /// without descriptor-relative no-follow traversal.
    ///
    /// # Errors
    ///
    /// Returns a closed acquisition error when discovery, descriptor pinning,
    /// root identity fails. The ratified SHA-1 requirement is deferred until a
    /// marked repository requests exact P1-base acquisition, so unprofiled
    /// repositories remain format-neutral.
    pub fn discover(start: &Path) -> Result<Self, SnapshotAdapterError> {
        let start = std::fs::canonicalize(start)
            .map_err(|error| SnapshotAdapterError::filesystem(&error))?;
        let root = GitRunner::discover_root(&start)?;
        let workspace = WorkspaceRoot::open(root.clone())?;
        let git = GitRunner::new(root.clone());
        workspace.verify_named_identity()?;
        if GitRunner::discover_root(git.root())? != root {
            return Err(SnapshotAdapterError::RepositoryRoot);
        }
        workspace.verify_named_identity()?;
        Ok(Self { workspace, git })
    }

    /// Acquire one complete current tree using the A/read/B/re-read/C protocol.
    ///
    /// # Errors
    ///
    /// Fails rather than retrying when inventory, identity, mode, kind, or
    /// content differs anywhere in the acquisition sequence.
    pub fn acquire_current(&self) -> Result<AcquiredCurrentSnapshot, SnapshotAdapterError> {
        let acquired = current::acquire(&self.workspace, &self.git)?;
        Ok(AcquiredCurrentSnapshot {
            snapshot: acquired.snapshot,
            seal: acquired.seal,
        })
    }

    /// Reconstruct the immutable ratified P1 base from exact Git objects.
    ///
    /// # Errors
    ///
    /// Rejects any commit/tree mismatch, unsupported leaf, malformed Git
    /// protocol response, missing blob, or blob whose bytes do not hash to its
    /// declared identity.
    pub fn acquire_p1_base(&self) -> Result<P1BaseSnapshot, SnapshotAdapterError> {
        use norn_policy::baseline::{P1_BASE_COMMIT, P1_BASE_TREE};

        self.workspace.verify_named_identity()?;
        self.git.verify_object_format()?;
        let (commit, tree) = self.git.verify_base(P1_BASE_COMMIT, P1_BASE_TREE)?;
        let records = self.git.base_tree(&tree)?;
        let blobs = BlobStore::acquire(&self.git, &records)?;
        let leaves = records
            .into_iter()
            .map(|record| {
                let Some(bytes) = blobs.get(&record.object_id) else {
                    return Err(SnapshotAdapterError::BaseIdentity);
                };
                let kind = match record.mode {
                    GitLeafMode::Regular | GitLeafMode::Executable => EntryKind::Regular,
                    GitLeafMode::Symlink => EntryKind::Symlink,
                };
                let entry = SnapshotEntry::new(kind, Arc::clone(bytes));
                GitTreeLeaf::new(record.path, record.mode, record.object_id, entry)
                    .map_err(base_leaf_error)
            })
            .collect::<Result<Vec<_>, SnapshotAdapterError>>()?;
        let base =
            P1BaseSnapshot::try_from_git_tree(commit, tree, leaves).map_err(base_snapshot_error)?;
        self.workspace.verify_named_identity()?;
        Ok(base)
    }

    /// Acquire the complete current role and the exact base when required.
    ///
    /// # Errors
    ///
    /// Returns a complete-current acquisition failure. When the marker is
    /// present but the exact base cannot be established, the returned input
    /// deliberately carries no base so the pure evaluator produces its closed
    /// persistent `Invalid` state. An unmarked repository does not need to
    /// contain the ratified Norn base object in order to evaluate as `Absent`.
    pub fn acquire_p1(&self) -> Result<AcquiredP1Repository, SnapshotAdapterError> {
        let current = self.acquire_current()?;
        let base = if current.snapshot.marker_observed() {
            match self.acquire_p1_base() {
                Ok(base) => P1BaseAcquisition::Acquired(base),
                Err(error) => P1BaseAcquisition::Unavailable(error),
            }
        } else {
            P1BaseAcquisition::NotRequired
        };
        Ok(AcquiredP1Repository { current, base })
    }

    /// Re-run the complete no-retry protocol and compare its private seal.
    ///
    /// Call this while the workspace policy coordinator remains held and
    /// immediately before publication.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotAdapterError::SnapshotChanged`] for any stable-inventory
    /// or content-stamp mismatch.
    pub fn revalidate_current(
        &self,
        acquired: &AcquiredCurrentSnapshot,
    ) -> Result<(), SnapshotAdapterError> {
        current::revalidate(&self.workspace, &self.git, &acquired.seal)
    }
}

fn base_leaf_error(error: norn_policy::GitTreeLeafError) -> SnapshotAdapterError {
    match error {
        norn_policy::GitTreeLeafError::ObjectFormat
        | norn_policy::GitTreeLeafError::EntryKind
        | norn_policy::GitTreeLeafError::ObjectIdentity => SnapshotAdapterError::BaseIdentity,
    }
}

fn base_snapshot_error(error: norn_policy::P1BaseSnapshotError) -> SnapshotAdapterError {
    match error {
        norn_policy::P1BaseSnapshotError::ObjectFormat
        | norn_policy::P1BaseSnapshotError::Inventory => SnapshotAdapterError::BaseIdentity,
    }
}

impl std::fmt::Debug for RepositorySnapshotAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RepositorySnapshotAdapter(..)")
    }
}

/// Complete current snapshot plus an unforgeable production revalidation seal.
pub struct AcquiredCurrentSnapshot {
    snapshot: CompleteCurrentSnapshot,
    seal: CurrentSnapshotSeal,
}

impl AcquiredCurrentSnapshot {
    /// Borrow the pure evaluator's complete-current role.
    #[must_use]
    pub const fn snapshot(&self) -> &CompleteCurrentSnapshot {
        &self.snapshot
    }
}

impl std::fmt::Debug for AcquiredCurrentSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcquiredCurrentSnapshot")
            .field("entry_count", &self.snapshot.snapshot().len())
            .finish_non_exhaustive()
    }
}

/// Role-safe current/base pair acquired through the production boundary.
pub struct AcquiredP1Repository {
    current: AcquiredCurrentSnapshot,
    base: P1BaseAcquisition,
}

impl AcquiredP1Repository {
    /// Construct the pure evaluator's borrowed role-safe input.
    #[must_use]
    pub const fn evaluation_input(&self) -> P1EvaluationInput<'_> {
        match &self.base {
            P1BaseAcquisition::Acquired(base) => {
                P1EvaluationInput::new(&self.current.snapshot, base)
            }
            P1BaseAcquisition::NotRequired | P1BaseAcquisition::Unavailable(_) => {
                P1EvaluationInput::current_only(&self.current.snapshot)
            }
        }
    }

    /// Return the retained typed base-acquisition failure, when a marked
    /// repository could not establish the ratified base.
    ///
    /// The pure evaluator still receives a missing base and therefore produces
    /// its persistent closed `Invalid` state. Retaining the failure prevents
    /// callers from losing the operational reason at the acquisition boundary.
    #[must_use]
    pub const fn base_failure(&self) -> Option<&SnapshotAdapterError> {
        match &self.base {
            P1BaseAcquisition::Unavailable(error) => Some(error),
            P1BaseAcquisition::NotRequired | P1BaseAcquisition::Acquired(_) => None,
        }
    }

    /// Borrow the current acquisition for immediate prepublication revalidation.
    #[must_use]
    pub const fn current(&self) -> &AcquiredCurrentSnapshot {
        &self.current
    }
}

enum P1BaseAcquisition {
    NotRequired,
    Acquired(P1BaseSnapshot),
    Unavailable(SnapshotAdapterError),
}

impl std::fmt::Debug for AcquiredP1Repository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcquiredP1Repository")
            .field(
                "current_entry_count",
                &self.current.snapshot.snapshot().len(),
            )
            .finish_non_exhaustive()
    }
}
