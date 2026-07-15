//! Immutable repository snapshots and deterministic staged overlays.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::digest::Digest;
use crate::path::RepositoryPath;

const SNAPSHOT_IDENTITY_DOMAIN: &[u8] = b"norn-policy-owned-snapshot-1";

/// Filesystem entry class observed without following links.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// An ordinary file with owned content bytes.
    Regular,
    /// A symbolic link. Its bytes are the link target representation.
    Symlink,
    /// Any entry that is neither an ordinary file nor a symbolic link.
    Other,
}

/// One immutable entry in an owned repository snapshot.
#[derive(Clone, Eq, PartialEq)]
pub struct SnapshotEntry {
    kind: EntryKind,
    bytes: Arc<[u8]>,
}

impl SnapshotEntry {
    /// Construct an entry by taking ownership of immutable bytes.
    #[must_use]
    pub fn new(kind: EntryKind, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            kind,
            bytes: bytes.into(),
        }
    }

    /// Construct an ordinary file by taking ownership of immutable bytes.
    #[must_use]
    pub fn regular(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::new(EntryKind::Regular, bytes)
    }

    /// Construct a symbolic-link entry without following it.
    #[must_use]
    pub fn symlink(target: impl Into<Arc<[u8]>>) -> Self {
        Self::new(EntryKind::Symlink, target)
    }

    /// Construct an unsupported non-file entry.
    #[must_use]
    pub fn other(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::new(EntryKind::Other, bytes)
    }

    /// Copy bytes into an immutable entry.
    #[must_use]
    pub fn copy_from_slice(kind: EntryKind, bytes: &[u8]) -> Self {
        Self::new(kind, Arc::<[u8]>::from(bytes))
    }

    /// Return the observed entry class.
    #[must_use]
    pub const fn kind(&self) -> EntryKind {
        self.kind
    }

    /// Borrow the immutable entry bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the number of owned bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return whether the entry owns no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for SnapshotEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotEntry")
            .field("kind", &self.kind)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

/// A complete immutable repository snapshot ordered by normalized path.
#[derive(Clone, Eq, PartialEq)]
pub struct OwnedSnapshot {
    entries: BTreeMap<RepositoryPath, SnapshotEntry>,
}

impl OwnedSnapshot {
    /// Construct an empty owned snapshot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Construct a snapshot, rejecting duplicate normalized paths.
    ///
    /// Input order has no effect on stored order or later evaluation.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::DuplicateEntry`] when a path occurs twice, or
    /// [`SnapshotError::DescendantBeneathEntry`] when a leaf is also presented
    /// as an ancestor directory.
    pub fn try_from_entries<I>(entries: I) -> Result<Self, SnapshotError>
    where
        I: IntoIterator<Item = (RepositoryPath, SnapshotEntry)>,
    {
        let mut snapshot = Self::empty();
        for (path, entry) in entries {
            if snapshot.entries.insert(path.clone(), entry).is_some() {
                return Err(SnapshotError::DuplicateEntry { path });
            }
        }
        validate_entry_tree(&snapshot.entries)?;
        Ok(snapshot)
    }

    /// Return an entry by normalized path.
    #[must_use]
    pub fn get(&self, path: &RepositoryPath) -> Option<&SnapshotEntry> {
        self.entries.get(path)
    }

    /// Return whether the snapshot contains a normalized path.
    #[must_use]
    pub fn contains_path(&self, path: &RepositoryPath) -> bool {
        self.entries.contains_key(path)
    }

    /// Iterate entries in normalized path order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&RepositoryPath, &SnapshotEntry)> {
        self.entries.iter()
    }

    /// Return the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the snapshot contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Hash every normalized path, entry kind, and exact content byte.
    ///
    /// The representation is domain separated, length framed with fixed-width
    /// unsigned integers, and ordered by the snapshot's normalized path map.
    /// The entry count makes a missing or additional row part of the identity.
    #[must_use]
    pub fn canonical_identity(&self) -> Digest {
        let mut hasher = Sha256::new();
        append_identity_field(&mut hasher, SNAPSHOT_IDENTITY_DOMAIN);
        append_identity_length(&mut hasher, self.entries.len());
        for (path, entry) in &self.entries {
            append_identity_field(&mut hasher, path.as_str().as_bytes());
            hasher.update([entry_kind_tag(entry.kind())]);
            append_identity_field(&mut hasher, entry.bytes());
        }
        Digest::from_bytes(hasher.finalize().into())
    }

    /// Apply a complete staged proposal to a cloned immutable snapshot.
    ///
    /// The original snapshot remains unchanged. Create, modify, and delete
    /// preconditions are checked against the original snapshot, while the
    /// proposal's unique normalized paths make application order immaterial.
    ///
    /// # Errors
    ///
    /// Returns a conflict when a create already exists, a modify/delete target
    /// is absent, or the result would place a descendant beneath a leaf entry.
    pub fn overlay(&self, proposal: &MutationProposal) -> Result<Self, SnapshotError> {
        for mutation in proposal.iter() {
            let exists = self.entries.contains_key(mutation.path());
            match mutation.kind() {
                MutationKind::Create(_) if exists => {
                    return Err(SnapshotError::CreateTargetExists {
                        path: mutation.path().clone(),
                    });
                }
                MutationKind::Modify(_) | MutationKind::Delete if !exists => {
                    return Err(SnapshotError::MutationTargetMissing {
                        path: mutation.path().clone(),
                    });
                }
                MutationKind::Create(_) | MutationKind::Modify(_) | MutationKind::Delete => {}
            }
        }

        let mut entries = self.entries.clone();
        for mutation in proposal.iter() {
            match mutation.kind() {
                MutationKind::Create(entry) | MutationKind::Modify(entry) => {
                    entries.insert(mutation.path().clone(), entry.clone());
                }
                MutationKind::Delete => {
                    entries.remove(mutation.path());
                }
            }
        }
        validate_entry_tree(&entries)?;
        Ok(Self { entries })
    }
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

const fn entry_kind_tag(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Regular => 0,
        EntryKind::Symlink => 1,
        EntryKind::Other => 2,
    }
}

fn validate_entry_tree(
    entries: &BTreeMap<RepositoryPath, SnapshotEntry>,
) -> Result<(), SnapshotError> {
    for descendant in entries.keys() {
        let mut ancestor = descendant.parent();
        while let Some(path) = ancestor {
            if let Some(entry) = entries.get(&path) {
                return Err(SnapshotError::DescendantBeneathEntry {
                    ancestor: path,
                    ancestor_kind: entry.kind(),
                    descendant: descendant.clone(),
                });
            }
            ancestor = path.parent();
        }
    }
    Ok(())
}

impl fmt::Debug for OwnedSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .entries
            .iter()
            .map(|(path, entry)| (path.as_str(), entry.kind(), entry.len()));
        formatter
            .debug_map()
            .entries(entries.map(|entry| (entry.0, (entry.1, entry.2))))
            .finish()
    }
}

/// The exact operation proposed for one normalized path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationKind {
    /// Add a path that is absent from the current snapshot.
    Create(SnapshotEntry),
    /// Replace a path that exists in the current snapshot.
    Modify(SnapshotEntry),
    /// Remove a path that exists in the current snapshot.
    Delete,
}

/// One staged mutation with an explicit create/modify/delete precondition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotMutation {
    path: RepositoryPath,
    kind: MutationKind,
}

impl SnapshotMutation {
    /// Stage creation of a previously absent path.
    #[must_use]
    pub const fn create(path: RepositoryPath, entry: SnapshotEntry) -> Self {
        Self {
            path,
            kind: MutationKind::Create(entry),
        }
    }

    /// Stage replacement of an existing path.
    #[must_use]
    pub const fn modify(path: RepositoryPath, entry: SnapshotEntry) -> Self {
        Self {
            path,
            kind: MutationKind::Modify(entry),
        }
    }

    /// Stage deletion of an existing path.
    #[must_use]
    pub const fn delete(path: RepositoryPath) -> Self {
        Self {
            path,
            kind: MutationKind::Delete,
        }
    }

    /// Return the normalized target path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the proposed operation.
    #[must_use]
    pub const fn kind(&self) -> &MutationKind {
        &self.kind
    }
}

/// A deterministic multi-path proposal with at most one operation per path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationProposal {
    mutations: BTreeMap<RepositoryPath, SnapshotMutation>,
}

impl MutationProposal {
    /// Construct a proposal, rejecting duplicate normalized targets.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::DuplicateMutation`] when a path occurs twice.
    pub fn try_from_mutations<I>(mutations: I) -> Result<Self, SnapshotError>
    where
        I: IntoIterator<Item = SnapshotMutation>,
    {
        let mut by_path = BTreeMap::new();
        for mutation in mutations {
            let path = mutation.path.clone();
            if by_path.insert(path.clone(), mutation).is_some() {
                return Err(SnapshotError::DuplicateMutation { path });
            }
        }
        Ok(Self { mutations: by_path })
    }

    /// Iterate proposed mutations in normalized path order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &SnapshotMutation> {
        self.mutations.values()
    }

    /// Return the number of distinct target paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mutations.len()
    }

    /// Return whether the proposal contains no mutations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }
}

/// Deterministic snapshot or overlay construction failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SnapshotError {
    /// A complete snapshot supplied the same normalized path twice.
    #[error("snapshot contains duplicate entry {path}")]
    DuplicateEntry {
        /// Duplicated repository-relative path.
        path: RepositoryPath,
    },
    /// A staged proposal supplied more than one operation for a path.
    #[error("proposal contains duplicate mutation {path}")]
    DuplicateMutation {
        /// Duplicated repository-relative path.
        path: RepositoryPath,
    },
    /// A create operation targeted an existing entry.
    #[error("create target already exists: {path}")]
    CreateTargetExists {
        /// Conflicting repository-relative path.
        path: RepositoryPath,
    },
    /// A modify or delete operation targeted an absent entry.
    #[error("mutation target is missing: {path}")]
    MutationTargetMissing {
        /// Missing repository-relative path.
        path: RepositoryPath,
    },
    /// A leaf snapshot entry was also presented as an ancestor directory.
    #[error("snapshot entry {ancestor} ({ancestor_kind:?}) cannot contain descendant {descendant}")]
    DescendantBeneathEntry {
        /// Leaf entry incorrectly used as a directory.
        ancestor: RepositoryPath,
        /// Observed leaf entry class.
        ancestor_kind: EntryKind,
        /// First normalized descendant beneath the leaf.
        descendant: RepositoryPath,
    },
}
