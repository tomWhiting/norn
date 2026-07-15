//! Closed review-inventory model, identity, and deterministic encoding.

use serde::Serialize;
use thiserror::Error;

use crate::baseline::ProductionLocClass;
use crate::digest::{CanonicalJsonError, digest_json};
use crate::facts::SourceInventoryEntry;
use crate::rust::modules::CompileTestFixtureFact;
use crate::writers::{OperationKind, SinkDiscovery, WriterRole, WriterToken};
use crate::{Digest, RepositoryPath};

pub(super) const REVIEW_INVENTORY_SCHEMA_VERSION: u32 = 1;
const REVIEW_INVENTORY_IDENTITY_DOMAIN: &str = "norn-policy-p1-review-inventory-1";

/// Complete deterministic inventory of P1 decisions that cannot be inferred.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct P1ReviewInventory {
    pub(super) schema_version: u32,
    pub(super) base_commit: String,
    pub(super) base_tree: String,
    pub(super) origin_digest: Digest,
    pub(super) base_source_inventory: Vec<SourceInventoryEntry>,
    pub(super) current_source_inventory: Vec<SourceInventoryEntry>,
    pub(super) base_compile_test_fixtures: Vec<CompileTestFixtureFact>,
    pub(super) current_compile_test_fixtures: Vec<CompileTestFixtureFact>,
    pub(super) loc_exceptions: Vec<LocReviewRequirement>,
    pub(super) debt_exceptions: Vec<DebtReviewRequirement>,
    pub(super) writer_operations: Vec<WriterReviewRequirement>,
}

impl P1ReviewInventory {
    /// Hash the complete normalized review inventory under its fixed P1 domain.
    ///
    /// # Errors
    ///
    /// Returns an error only if the closed value cannot be represented as
    /// canonical JSON.
    pub fn canonical_identity(&self) -> Result<Digest, P1ReviewIdentityError> {
        let input = P1ReviewIdentityInput {
            domain: REVIEW_INVENTORY_IDENTITY_DOMAIN,
            inventory: self,
        };
        let value = serde_json::to_value(input).map_err(P1ReviewIdentityError::Serialization)?;
        digest_json(&value).map_err(P1ReviewIdentityError::Canonical)
    }

    /// Borrow the exact ratified base commit named by this inventory.
    #[must_use]
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Borrow the exact ratified base tree named by this inventory.
    #[must_use]
    pub fn base_tree(&self) -> &str {
        &self.base_tree
    }

    /// Return the normalized immutable-origin identity reviewed by this inventory.
    #[must_use]
    pub const fn origin_digest(&self) -> Digest {
        self.origin_digest
    }

    /// Borrow every exact source row from the immutable base.
    #[must_use]
    pub fn base_source_inventory(&self) -> &[SourceInventoryEntry] {
        &self.base_source_inventory
    }

    /// Borrow every exact source row from the complete current snapshot.
    #[must_use]
    pub fn current_source_inventory(&self) -> &[SourceInventoryEntry] {
        &self.current_source_inventory
    }

    /// Borrow every immutable-base compile-test fixture row.
    #[must_use]
    pub fn base_compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        &self.base_compile_test_fixtures
    }

    /// Borrow every current compile-test fixture row.
    #[must_use]
    pub fn current_compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        &self.current_compile_test_fixtures
    }

    /// Borrow over-limit immutable-origin rows requiring governance metadata.
    #[must_use]
    pub fn loc_exceptions(&self) -> &[LocReviewRequirement] {
        &self.loc_exceptions
    }

    /// Borrow prohibited-debt rows requiring governance metadata.
    #[must_use]
    pub fn debt_exceptions(&self) -> &[DebtReviewRequirement] {
        &self.debt_exceptions
    }

    /// Borrow the exact base/current writer-operation union.
    #[must_use]
    pub fn writer_operations(&self) -> &[WriterReviewRequirement] {
        &self.writer_operations
    }

    pub(super) fn encode_document(&self) -> Result<Vec<u8>, P1ReviewEncodeError> {
        let document = P1ReviewInventoryDocument {
            inventory_identity: self
                .canonical_identity()
                .map_err(P1ReviewEncodeError::Identity)?,
            inventory: self,
        };
        let mut bytes =
            serde_json::to_vec_pretty(&document).map_err(P1ReviewEncodeError::Serialization)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[derive(Serialize)]
struct P1ReviewIdentityInput<'a> {
    domain: &'static str,
    inventory: &'a P1ReviewInventory,
}

#[derive(Serialize)]
struct P1ReviewInventoryDocument<'a> {
    inventory_identity: Digest,
    #[serde(flatten)]
    inventory: &'a P1ReviewInventory,
}

/// One immutable over-limit source requiring reviewed governance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LocReviewRequirement {
    /// Immutable origin identity requiring a LOC exception.
    pub origin_id: Digest,
    /// Source path belonging to the immutable fact.
    pub path: RepositoryPath,
    /// Ceiling class selected by production reachability.
    pub loc_class: ProductionLocClass,
    /// Exact production LOC recorded at the P1 base.
    pub production_loc: u32,
    /// Compiled P1 ceiling exceeded by this row.
    pub baseline_limit: u32,
}

/// One immutable prohibited-debt occurrence requiring reviewed governance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DebtReviewRequirement {
    /// Immutable origin identity requiring a debt exception.
    pub origin_id: Digest,
    /// Source path containing the prohibited occurrence.
    pub path: RepositoryPath,
    /// Stable prohibited-debt fingerprint.
    pub fingerprint: Digest,
    /// Collision-preserving occurrence ordinal.
    pub ordinal: u32,
}

/// One stable writer operation requiring exactly one reviewed classification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriterReviewRequirement {
    /// Stable operation identity used by reviewed classifications.
    pub operation_id: Digest,
    /// Source path containing the operation.
    pub path: RepositoryPath,
    /// Registered sink identity resolved by the analyzer.
    pub sink: WriterToken,
    /// Filesystem operation class.
    pub operation_kind: OperationKind,
    /// Semantic role constraining valid classifications.
    pub role: WriterRole,
    /// Analyzer discovery route.
    pub discovery: SinkDiscovery,
    /// Collision-preserving operation ordinal.
    pub ordinal: u32,
    /// Source span in the immutable base, when present there.
    pub base_span: Option<ReviewSpan>,
    /// Source span in the current tree, when present there.
    pub current_span: Option<ReviewSpan>,
}

/// One exact source span retained only to support human review.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewSpan {
    /// Inclusive byte offset.
    pub start: u64,
    /// Exclusive byte offset.
    pub end: u64,
}

/// Deterministic review-inventory encoding failure.
#[derive(Debug, Error)]
pub enum P1ReviewEncodeError {
    /// The normalized review-inventory identity could not be generated.
    #[error("P1 review inventory identity could not be generated")]
    Identity(#[source] P1ReviewIdentityError),
    /// The closed review inventory could not be represented as JSON.
    #[error("P1 review inventory could not be encoded")]
    Serialization(#[source] serde_json::Error),
}

/// Deterministic review-inventory identity failure.
#[derive(Debug, Error)]
pub enum P1ReviewIdentityError {
    /// The closed inventory could not be represented as JSON.
    #[error("P1 review inventory could not be normalized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("P1 review inventory canonical identity could not be encoded")]
    Canonical(#[source] CanonicalJsonError),
}
