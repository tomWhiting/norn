//! Role-safe inputs for one complete P1 evaluation.

use std::collections::BTreeMap;
use std::fmt;

use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use thiserror::Error;

use crate::RepositoryPath;
use crate::baseline::{P1_BASE_COMMIT, P1_BASE_TREE};
use crate::digest::Digest;
use crate::phase_lock::{GitObjectFormat, GitObjectId};
use crate::snapshot::{EntryKind, MutationProposal, OwnedSnapshot, SnapshotEntry, SnapshotError};

const PHASE_LOCK_PATH: &str = "policy/phase-lock.json";
const GIT_INVENTORY_IDENTITY_DOMAIN: &[u8] = b"norn-policy-p1-git-tree-inventory-1";

/// Exact identity of every path, Git mode, and blob in the ratified P1 base tree.
pub const P1_BASE_GIT_INVENTORY_IDENTITY: Digest = Digest::from_bytes([
    0x88, 0x2f, 0x54, 0x56, 0x06, 0x63, 0xa2, 0xf4, 0xb4, 0xdb, 0xcc, 0xed, 0xc7, 0x38, 0xb0, 0xd9,
    0x1f, 0xf1, 0xab, 0x50, 0x8b, 0x30, 0xa8, 0xd2, 0x22, 0x8d, 0x32, 0xa3, 0x41, 0xe8, 0x9d, 0x8e,
]);

/// Exact supported Git leaf mode retained independently of analysis semantics.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GitLeafMode {
    /// Ordinary non-executable blob (`100644`).
    Regular,
    /// Ordinary executable blob (`100755`).
    Executable,
    /// Symbolic-link blob (`120000`).
    Symlink,
}

impl GitLeafMode {
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Regular => b"100644",
            Self::Executable => b"100755",
            Self::Symlink => b"120000",
        }
    }

    const fn entry_kind(self) -> EntryKind {
        match self {
            Self::Regular | Self::Executable => EntryKind::Regular,
            Self::Symlink => EntryKind::Symlink,
        }
    }
}

/// One exact Git leaf and the immutable bytes returned for its blob object.
#[derive(Clone, Eq, PartialEq)]
pub struct GitTreeLeaf {
    path: RepositoryPath,
    mode: GitLeafMode,
    object_id: GitObjectId,
    entry: SnapshotEntry,
}

impl GitTreeLeaf {
    /// Construct one leaf after validating its mode, kind, and blob identity.
    ///
    /// # Errors
    ///
    /// Rejects a non-SHA-1 object, a mode/kind mismatch, or bytes that do not
    /// hash to the declared Git blob object.
    pub fn new(
        path: RepositoryPath,
        mode: GitLeafMode,
        object_id: GitObjectId,
        entry: SnapshotEntry,
    ) -> Result<Self, GitTreeLeafError> {
        if object_id.object_format() != GitObjectFormat::Sha1 {
            return Err(GitTreeLeafError::ObjectFormat);
        }
        if entry.kind() != mode.entry_kind() {
            return Err(GitTreeLeafError::EntryKind);
        }
        if git_blob_identity(entry.bytes()) != object_id.as_str() {
            return Err(GitTreeLeafError::ObjectIdentity);
        }
        Ok(Self {
            path,
            mode,
            object_id,
            entry,
        })
    }
}

impl fmt::Debug for GitTreeLeaf {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitTreeLeaf")
            .field("mode", &self.mode)
            .field("byte_len", &self.entry.len())
            .finish_non_exhaustive()
    }
}

/// Closed failure while validating one exact Git leaf.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GitTreeLeafError {
    /// P1 accepts only SHA-1 Git object identities.
    #[error("P1 Git tree leaf object format is unsupported")]
    ObjectFormat,
    /// Git mode and analyzer entry kind disagree.
    #[error("P1 Git tree leaf mode does not match its entry kind")]
    EntryKind,
    /// Exact bytes do not match the declared Git blob object.
    #[error("P1 Git tree leaf bytes do not match its object identity")]
    ObjectIdentity,
}

/// Complete current repository snapshot with non-downgradable marker history.
#[derive(Clone, Eq, PartialEq)]
pub struct CompleteCurrentSnapshot {
    snapshot: OwnedSnapshot,
    marker_observed: bool,
}

impl CompleteCurrentSnapshot {
    /// Wrap one complete adapter acquisition and derive marker continuity.
    ///
    /// The adapter is responsible for complete, race-checked enumeration. This
    /// constructor deliberately requires an owned snapshot and has no `From`
    /// implementation, making that trust-boundary assertion explicit.
    #[must_use]
    pub fn from_complete_snapshot(snapshot: OwnedSnapshot) -> Self {
        let marker_observed = has_phase_lock_marker(&snapshot);
        Self {
            snapshot,
            marker_observed,
        }
    }

    /// Wrap a complete snapshot after the adapter observed the fixed marker in
    /// tracked repository history even though the current leaf is absent.
    ///
    /// This is a one-way activation: callers may preserve prior marker history,
    /// but no API can clear an observed marker. A tracked deletion therefore
    /// evaluates as an invalid required profile rather than an absent profile.
    #[must_use]
    pub fn from_complete_snapshot_with_marker_history(snapshot: OwnedSnapshot) -> Self {
        Self {
            snapshot,
            marker_observed: true,
        }
    }

    /// Apply a complete proposal while preserving prior marker observation.
    ///
    /// # Errors
    ///
    /// Returns the underlying structural or mutation-precondition error.
    pub fn overlay(&self, proposal: &MutationProposal) -> Result<Self, SnapshotError> {
        let snapshot = self.snapshot.overlay(proposal)?;
        Ok(Self {
            marker_observed: self.marker_observed || has_phase_lock_marker(&snapshot),
            snapshot,
        })
    }

    /// Borrow the complete immutable snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &OwnedSnapshot {
        &self.snapshot
    }

    /// Return whether the fixed P1 marker is present or was observed in tracked
    /// repository history by the complete-snapshot adapter.
    #[must_use]
    pub const fn marker_observed(&self) -> bool {
        self.marker_observed
    }
}

impl fmt::Debug for CompleteCurrentSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteCurrentSnapshot")
            .field("entry_count", &self.snapshot.len())
            .field("marker_observed", &self.marker_observed)
            .finish()
    }
}

/// Observed base-role snapshot whose exactness is established during acquisition.
#[derive(Clone, Eq, PartialEq)]
pub struct P1BaseSnapshot {
    commit: GitObjectId,
    tree: GitObjectId,
    snapshot: OwnedSnapshot,
    leaves: BTreeMap<RepositoryPath, GitLeafAuthority>,
}

#[derive(Clone, Eq, PartialEq)]
struct GitLeafAuthority {
    mode: GitLeafMode,
    object_id: GitObjectId,
}

impl P1BaseSnapshot {
    /// Build the base-role snapshot from a complete exact Git tree observation.
    ///
    /// # Errors
    ///
    /// Rejects non-SHA-1 commit/tree identities or an invalid/duplicate leaf
    /// inventory. Exact P1 identities are checked by authority acquisition.
    pub fn try_from_git_tree<I>(
        commit: GitObjectId,
        tree: GitObjectId,
        leaves: I,
    ) -> Result<Self, P1BaseSnapshotError>
    where
        I: IntoIterator<Item = GitTreeLeaf>,
    {
        if commit.object_format() != GitObjectFormat::Sha1
            || tree.object_format() != GitObjectFormat::Sha1
        {
            return Err(P1BaseSnapshotError::ObjectFormat);
        }
        let leaves = leaves.into_iter().collect::<Vec<_>>();
        let Ok(snapshot) = OwnedSnapshot::try_from_entries(
            leaves
                .iter()
                .map(|leaf| (leaf.path.clone(), leaf.entry.clone())),
        ) else {
            return Err(P1BaseSnapshotError::Inventory);
        };
        let mut authorities = BTreeMap::new();
        for leaf in leaves {
            if authorities
                .insert(
                    leaf.path,
                    GitLeafAuthority {
                        mode: leaf.mode,
                        object_id: leaf.object_id,
                    },
                )
                .is_some()
            {
                return Err(P1BaseSnapshotError::Inventory);
            }
        }
        Ok(Self {
            commit,
            tree,
            snapshot,
            leaves: authorities,
        })
    }

    /// Borrow the semantic analysis projection of the observed Git tree.
    #[must_use]
    pub const fn snapshot(&self) -> &OwnedSnapshot {
        &self.snapshot
    }

    pub(crate) fn validate_p1_identity(&self) -> Result<(), P1BaseIdentityError> {
        if self.commit.as_str() != P1_BASE_COMMIT {
            return Err(P1BaseIdentityError::Commit);
        }
        if self.tree.as_str() != P1_BASE_TREE {
            return Err(P1BaseIdentityError::Tree);
        }
        if self.git_inventory_identity() != P1_BASE_GIT_INVENTORY_IDENTITY {
            return Err(P1BaseIdentityError::Inventory);
        }
        Ok(())
    }

    /// Hash every observed leaf path, exact Git mode, and blob identity.
    #[must_use]
    pub fn git_inventory_identity(&self) -> Digest {
        let mut hasher = Sha256::new();
        append_identity_field(&mut hasher, GIT_INVENTORY_IDENTITY_DOMAIN);
        append_identity_length(&mut hasher, self.leaves.len());
        for (path, leaf) in &self.leaves {
            append_identity_field(&mut hasher, path.as_str().as_bytes());
            append_identity_field(&mut hasher, leaf.mode.as_bytes());
            append_identity_field(&mut hasher, leaf.object_id.as_str().as_bytes());
        }
        Digest::from_bytes(hasher.finalize().into())
    }
}

impl fmt::Debug for P1BaseSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("P1BaseSnapshot")
            .field("entry_count", &self.snapshot.len())
            .finish_non_exhaustive()
    }
}

/// Closed structural failure while constructing a base-role observation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum P1BaseSnapshotError {
    /// P1 accepts only SHA-1 Git object identities.
    #[error("P1 base Git object format is unsupported")]
    ObjectFormat,
    /// The complete leaf inventory was duplicated or structurally invalid.
    #[error("P1 base Git leaf inventory is invalid")]
    Inventory,
}

/// Closed mismatch between an observed base role and ratified P1 authority.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum P1BaseIdentityError {
    /// The adapter reported a commit other than the ratified P1 base.
    #[error("observed P1 base commit is not ratified")]
    Commit,
    /// The adapter reported a tree other than the ratified P1 base tree.
    #[error("observed P1 base tree is not ratified")]
    Tree,
    /// A path, mode, or blob identity differs from the ratified inventory.
    #[error("observed P1 base Git inventory is not ratified")]
    Inventory,
}

/// Role-safe borrowed inputs for one P1 evaluation.
#[derive(Clone, Copy)]
pub struct P1EvaluationInput<'a> {
    current: &'a CompleteCurrentSnapshot,
    base: Option<&'a P1BaseSnapshot>,
}

impl<'a> P1EvaluationInput<'a> {
    /// Bind distinct current and base roles into one evaluation request.
    ///
    /// ```compile_fail
    /// use norn_policy::{CompleteCurrentSnapshot, P1BaseSnapshot, P1EvaluationInput};
    /// fn swapped(current: &CompleteCurrentSnapshot, base: &P1BaseSnapshot) {
    ///     P1EvaluationInput::new(base, current);
    /// }
    /// ```
    #[must_use]
    pub const fn new(current: &'a CompleteCurrentSnapshot, base: &'a P1BaseSnapshot) -> Self {
        Self {
            current,
            base: Some(base),
        }
    }

    /// Bind a current snapshot before a P1 base is required.
    ///
    /// This form can evaluate only to `Absent` when the marker has never been
    /// observed, or `Invalid` when a required marker exists without an exact
    /// base. It cannot produce a ready P1 result.
    #[must_use]
    pub const fn current_only(current: &'a CompleteCurrentSnapshot) -> Self {
        Self {
            current,
            base: None,
        }
    }

    pub(crate) const fn current(self) -> &'a CompleteCurrentSnapshot {
        self.current
    }

    pub(crate) const fn base(self) -> Option<&'a P1BaseSnapshot> {
        self.base
    }
}

fn has_phase_lock_marker(snapshot: &OwnedSnapshot) -> bool {
    snapshot
        .iter()
        .any(|(path, _entry)| path.as_str() == PHASE_LOCK_PATH)
}

fn git_blob_identity(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn append_identity_field(hasher: &mut Sha256, value: &[u8]) {
    append_identity_length(hasher, value.len());
    hasher.update(value);
}

fn append_identity_length(hasher: &mut Sha256, length: usize) {
    let native = length.to_be_bytes();
    hasher.update(&[0_u8; 16][native.len()..]);
    hasher.update(native);
}
