//! Exact source identity and deterministic writer inventories.

use serde::Serialize;

use crate::digest::{Digest, digest_bytes};
use crate::path::RepositoryPath;

use super::{WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION, WriterOperation, WriterToken};
use crate::writers::candidate::WriterCandidate;

/// Exact source identity covered by one registry-local writer scan.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WriterSourceIdentity {
    pub(crate) path: RepositoryPath,
    pub(crate) content: Digest,
}

impl WriterSourceIdentity {
    pub(crate) fn from_bytes(path: RepositoryPath, bytes: &[u8]) -> Self {
        Self {
            path,
            content: digest_bytes(bytes),
        }
    }

    /// Return the exact analyzed source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the digest of the exact analyzed source bytes.
    #[must_use]
    pub const fn content(&self) -> Digest {
        self.content
    }
}

/// Deterministic writer inventory for one explicit source set and registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriterInventory {
    pub(crate) schema_version: u32,
    pub(crate) analyzer_version: WriterToken,
    pub(crate) sources: Vec<WriterSourceIdentity>,
    pub(crate) source_inventory_digest: Digest,
    pub(crate) registry_digest: Digest,
    pub(crate) operations: Vec<WriterOperation>,
    pub(crate) candidates: Vec<WriterCandidate>,
    pub(crate) unobserved_required_sinks: Vec<WriterToken>,
}

impl WriterInventory {
    /// Return the inventory schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Borrow the exact sorted source identities analyzed by this inventory.
    #[must_use]
    pub fn sources(&self) -> &[WriterSourceIdentity] {
        &self.sources
    }

    /// Return the digest binding the complete source identity inventory.
    #[must_use]
    pub const fn source_inventory_digest(&self) -> Digest {
        self.source_inventory_digest
    }

    /// Return the exact semantic registry digest used by the scan.
    #[must_use]
    pub const fn registry_digest(&self) -> Digest {
        self.registry_digest
    }

    /// Borrow resolved operations in deterministic order.
    #[must_use]
    pub fn operations(&self) -> &[WriterOperation] {
        &self.operations
    }

    /// Borrow unresolved candidates in deterministic order.
    #[must_use]
    pub fn candidates(&self) -> &[WriterCandidate] {
        &self.candidates
    }

    /// Borrow required registry entries not observed in the analyzed sources.
    #[must_use]
    pub fn unobserved_required_sinks(&self) -> &[WriterToken] {
        &self.unobserved_required_sinks
    }

    /// Return whether this supplied registry and source set has no open state.
    ///
    /// This is registry-local completeness. Canonical repository completeness
    /// additionally verifies exact source and reviewed-registry authority.
    #[must_use]
    pub fn is_registry_complete(&self) -> bool {
        self.candidates.is_empty() && self.unobserved_required_sinks.is_empty()
    }

    pub(crate) fn has_valid_metadata(&self) -> bool {
        self.schema_version == WRITER_SCHEMA_VERSION
            && self.analyzer_version.as_str() == WRITER_ANALYZER_VERSION
            && writer_source_inventory_digest(&self.sources) == self.source_inventory_digest
    }
}

pub(crate) fn writer_source_inventory_digest(sources: &[WriterSourceIdentity]) -> Digest {
    let mut framed = Vec::new();
    inventory_field(&mut framed, b"norn-writer-source-inventory-1");
    for source in sources {
        inventory_field(&mut framed, source.path.as_str().as_bytes());
        inventory_field(&mut framed, source.content.as_bytes());
    }
    digest_bytes(&framed)
}

fn inventory_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
