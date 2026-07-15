//! Closed, non-disclosing repository-snapshot failures.

use std::io;

use norn_policy::RepositoryPath;
use thiserror::Error;

use crate::resource::DescriptorAdmissionError;

/// One fixed Git operation performed by the adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitOperation {
    /// Discover the canonical repository root.
    DiscoverRoot,
    /// Verify the repository object format.
    ObjectFormat,
    /// Resolve the ratified P1 commit object.
    VerifyBaseCommit,
    /// Resolve the ratified P1 tree object.
    VerifyBaseTree,
    /// Enumerate the exact ratified P1 tree.
    EnumerateBaseTree,
    /// Read immutable Git blobs through the batch protocol.
    ReadBaseBlobs,
    /// Enumerate tracked index entries.
    EnumerateTracked,
    /// Enumerate untracked, non-ignored entries.
    EnumerateUntracked,
    /// Determine whether reachable repository history has carried the P1 marker.
    MarkerHistory,
}

impl std::fmt::Display for GitOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::DiscoverRoot => "repository root discovery",
            Self::ObjectFormat => "Git object-format verification",
            Self::VerifyBaseCommit => "P1 base commit verification",
            Self::VerifyBaseTree => "P1 base tree verification",
            Self::EnumerateBaseTree => "P1 base tree enumeration",
            Self::ReadBaseBlobs => "P1 base blob acquisition",
            Self::EnumerateTracked => "tracked repository enumeration",
            Self::EnumerateUntracked => "untracked repository enumeration",
            Self::MarkerHistory => "P1 marker history inspection",
        })
    }
}

/// Stable reason a workspace leaf could not enter a complete snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceEntryIssue {
    /// The leaf could not be securely opened or read.
    Unreadable,
    /// The leaf is neither a regular file nor a symbolic link.
    UnsupportedKind,
    /// Its identity, kind, mode, or content changed during acquisition.
    Changed,
}

impl std::fmt::Display for WorkspaceEntryIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unreadable => "cannot be read without following links",
            Self::UnsupportedKind => "has an unsupported filesystem kind",
            Self::Changed => "changed during snapshot acquisition",
        })
    }
}

/// Failure to acquire or revalidate a complete repository observation.
#[derive(Debug, Error)]
pub enum SnapshotAdapterError {
    /// The platform lacks the descriptor-relative no-follow primitives required.
    #[error("repository snapshots require a supported descriptor-capable Unix target")]
    UnsupportedPlatform,
    /// A filesystem operation failed before a repository-relative path existed.
    #[error("repository snapshot filesystem operation failed with {kind:?}")]
    Filesystem {
        /// Stable I/O classification; no absolute path is retained.
        kind: io::ErrorKind,
    },
    /// The process-wide descriptor governor rejected an operation.
    #[error(transparent)]
    DescriptorAdmission(#[from] DescriptorAdmissionError),
    /// Git could not be started without exposing its environment or path.
    #[error("{operation} could not start Git: {kind:?}")]
    GitSpawn {
        /// Fixed operation that failed.
        operation: GitOperation,
        /// Stable I/O classification.
        kind: io::ErrorKind,
    },
    /// Communication with a started Git plumbing process failed.
    #[error("{operation} Git protocol I/O failed with {kind:?}")]
    GitIo {
        /// Fixed operation that failed.
        operation: GitOperation,
        /// Stable I/O classification.
        kind: io::ErrorKind,
    },
    /// Git returned a non-success status. Raw output is deliberately discarded.
    #[error("{0} failed")]
    GitExit(GitOperation),
    /// Git returned bytes outside the operation's closed grammar.
    #[error("{0} returned an invalid protocol response")]
    GitProtocol(GitOperation),
    /// Git returned text that was not complete valid UTF-8.
    #[error("{operation} returned {kind} text encoding")]
    GitEncoding {
        /// Fixed operation that failed.
        operation: GitOperation,
        /// Whether the sequence ended early or contained an invalid byte.
        kind: GitEncodingIssue,
    },
    /// The discovered root was not one unambiguous absolute UTF-8 path.
    #[error("Git repository root is not representable by this adapter")]
    RepositoryRoot,
    /// The repository is not using the ratified SHA-1 object format.
    #[error("repository Git object format is not the ratified SHA-1 format")]
    ObjectFormat,
    /// A raw Git path cannot be represented by the normalized policy path type.
    #[error("Git inventory contains an unsupported repository path")]
    RepositoryPath,
    /// The index contains a merge stage other than zero.
    #[error("Git index contains unmerged entries")]
    UnmergedIndex,
    /// A tracked path is omitted from the worktree by skip-worktree authority.
    #[error("Git index contains sparse or skip-worktree entries")]
    SparseIndex,
    /// A tree or index entry uses a mode/type outside the closed P1 model.
    #[error("Git inventory contains an unsupported entry mode or type")]
    GitEntry,
    /// Two Git inventory rows normalize to the same path.
    #[error("Git inventory contains duplicate or conflicting paths")]
    DuplicateInventory,
    /// The ratified commit, tree, leaf, or aggregate inventory did not verify.
    #[error("ratified P1 base Git identity could not be established")]
    BaseIdentity,
    /// A present workspace leaf could not be acquired completely.
    #[error("repository entry {path} {issue}")]
    WorkspaceEntry {
        /// Normalized repository-relative path only.
        path: RepositoryPath,
        /// Stable failure class.
        issue: WorkspaceEntryIssue,
    },
    /// Inventory or stamps differed across the no-retry acquisition sequence.
    #[error("repository changed during snapshot acquisition")]
    SnapshotChanged,
    /// The policy snapshot rejected the complete observed entry set.
    #[error("complete repository observation has an invalid entry structure")]
    SnapshotStructure,
    /// An allocation required by the exact repository contents failed.
    #[error("repository snapshot contents could not be represented in memory")]
    Capacity,
}

/// Closed UTF-8 failure class without retaining source bytes or offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitEncodingIssue {
    /// The final UTF-8 sequence ended before it was complete.
    Incomplete,
    /// A byte sequence was not legal UTF-8.
    Invalid,
}

impl std::fmt::Display for GitEncodingIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Incomplete => "incomplete",
            Self::Invalid => "invalid",
        })
    }
}

impl SnapshotAdapterError {
    pub(super) fn filesystem(error: &io::Error) -> Self {
        Self::Filesystem { kind: error.kind() }
    }

    pub(super) fn git_spawn(operation: GitOperation, error: &io::Error) -> Self {
        Self::GitSpawn {
            operation,
            kind: error.kind(),
        }
    }

    pub(super) fn git_io(operation: GitOperation, error: &io::Error) -> Self {
        Self::GitIo {
            operation,
            kind: error.kind(),
        }
    }

    pub(super) fn git_encoding(operation: GitOperation, error: std::str::Utf8Error) -> Self {
        let kind = if error.error_len().is_some() {
            GitEncodingIssue::Invalid
        } else {
            GitEncodingIssue::Incomplete
        };
        Self::GitEncoding { operation, kind }
    }
}
