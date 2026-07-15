//! Deterministic human-review inventory for unresolved writer candidates.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;
use crate::version::DIGEST_VERSION;
use crate::writers::{
    RegistryError, SinkRegistry, UnknownSinkReason, WRITER_ANALYZER_VERSION, WriterCandidate,
    WriterCandidateForm, WriterCandidateId, WriterToken,
};

use super::{WriterResolutionCoverage, WriterResolutionCoverageError};

/// Fixed checked-in path for the generated P1 review inventory.
pub const WRITER_RESOLUTION_REVIEW_INVENTORY_PATH: &str =
    "docs/reviews/evidence/p1/writer-resolution-inventory.json";

/// First closed writer-resolution review-inventory schema.
pub const WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION: u32 = 1;

/// Complete deterministic review artifact for the exact candidate union.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriterResolutionReviewInventory {
    schema_version: u32,
    algorithms: ReviewAlgorithms,
    sink_registry: Digest,
    review_inventory: Digest,
    rows: Vec<WriterResolutionReviewRow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ReviewAlgorithms {
    writer: String,
    digest: String,
}

/// One snippet-free semantic candidate row with diagnostic snapshot spans.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriterResolutionReviewRow {
    candidate_id: WriterCandidateId,
    path: RepositoryPath,
    enclosing_item: Digest,
    normalized_call: Digest,
    candidate: WriterToken,
    reason: UnknownSinkReason,
    form: WriterCandidateForm,
    ordinal: u32,
    base_span: Option<ByteSpan>,
    current_span: Option<ByteSpan>,
}

impl WriterResolutionReviewInventory {
    /// Construct the exact base/current semantic-union review artifact.
    ///
    /// Input order is irrelevant. Span-only movement collapses into one row
    /// carrying both diagnostic spans. All identity-bearing fields come from
    /// the canonical candidate selected by [`WriterResolutionCoverage`].
    ///
    /// # Errors
    ///
    /// Rejects an invalid sink registry, a duplicate candidate within either
    /// snapshot, or one identity carrying different semantic content.
    pub fn author_p1(
        base: &[WriterCandidate],
        current: &[WriterCandidate],
        registry: &SinkRegistry,
    ) -> Result<Self, WriterResolutionReviewInventoryError> {
        registry
            .validate()
            .map_err(WriterResolutionReviewInventoryError::Registry)?;
        let coverage =
            WriterResolutionCoverage::for_snapshots(base, current).map_err(
                |error| match error {
                    WriterResolutionCoverageError::Duplicate { candidate } => {
                        WriterResolutionReviewInventoryError::DuplicateCandidate { candidate }
                    }
                    WriterResolutionCoverageError::Collision { candidate } => {
                        WriterResolutionReviewInventoryError::CandidateCollision { candidate }
                    }
                },
            )?;
        let base_spans = spans_by_id(base);
        let current_spans = spans_by_id(current);
        let rows = coverage
            .candidates()
            .map(|(id, candidate)| WriterResolutionReviewRow {
                candidate_id: *id,
                path: candidate.path().clone(),
                enclosing_item: candidate.enclosing_item(),
                normalized_call: candidate.normalized_call(),
                candidate: candidate.candidate().clone(),
                reason: candidate.reason(),
                form: candidate.form(),
                ordinal: candidate.ordinal(),
                base_span: base_spans.get(id).copied(),
                current_span: current_spans.get(id).copied(),
            })
            .collect();
        Ok(Self {
            schema_version: WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION,
            algorithms: ReviewAlgorithms {
                writer: WRITER_ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            sink_registry: registry.digest(),
            review_inventory: coverage.review_inventory(),
            rows,
        })
    }

    /// Return the closed review-inventory schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the exact sink-registry digest used to discover candidates.
    #[must_use]
    pub const fn sink_registry(&self) -> Digest {
        self.sink_registry
    }

    /// Return the span-independent semantic-union digest.
    #[must_use]
    pub const fn review_inventory(&self) -> Digest {
        self.review_inventory
    }

    /// Borrow candidate-ID-sorted review rows.
    #[must_use]
    pub fn rows(&self) -> &[WriterResolutionReviewRow] {
        &self.rows
    }

    /// Hash the complete normalized review artifact, including both spans.
    ///
    /// # Errors
    ///
    /// Returns an error only if the closed model cannot be represented as
    /// canonical JSON.
    pub fn canonical_identity(&self) -> Result<Digest, WriterResolutionReviewDigestError> {
        let value =
            serde_json::to_value(self).map_err(WriterResolutionReviewDigestError::Serialization)?;
        digest_json(&value).map_err(WriterResolutionReviewDigestError::Canonical)
    }

    /// Encode deterministic pretty JSON with exactly one trailing newline.
    ///
    /// # Errors
    ///
    /// Returns an error if the closed review artifact cannot be serialized.
    pub fn encode_p1_pretty(&self) -> Result<Vec<u8>, WriterResolutionReviewEncodeError> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(WriterResolutionReviewEncodeError::Serialization)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl WriterResolutionReviewRow {
    /// Return the stable candidate identity.
    #[must_use]
    pub const fn candidate_id(&self) -> WriterCandidateId {
        self.candidate_id
    }

    /// Return the repository-relative candidate path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the enclosing-item identity.
    #[must_use]
    pub const fn enclosing_item(&self) -> Digest {
        self.enclosing_item
    }

    /// Return the normalized call identity.
    #[must_use]
    pub const fn normalized_call(&self) -> Digest {
        self.normalized_call
    }

    /// Return the normalized candidate token.
    #[must_use]
    pub const fn candidate(&self) -> &WriterToken {
        &self.candidate
    }

    /// Return the closed uncertainty reason.
    #[must_use]
    pub const fn reason(&self) -> UnknownSinkReason {
        self.reason
    }

    /// Return the closed syntax or authority form.
    #[must_use]
    pub const fn form(&self) -> WriterCandidateForm {
        self.form
    }

    /// Return the semantic multiset ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Return the immutable-base diagnostic span, when present.
    #[must_use]
    pub const fn base_span(&self) -> Option<ByteSpan> {
        self.base_span
    }

    /// Return the current diagnostic span, when present.
    #[must_use]
    pub const fn current_span(&self) -> Option<ByteSpan> {
        self.current_span
    }
}

fn spans_by_id(candidates: &[WriterCandidate]) -> BTreeMap<WriterCandidateId, ByteSpan> {
    candidates
        .iter()
        .map(|candidate| (candidate.id(), candidate.span()))
        .collect()
}

/// Failure to construct exact writer-resolution review inventory.
#[derive(Debug, Error)]
pub enum WriterResolutionReviewInventoryError {
    /// The supplied sink registry was internally invalid.
    #[error("writer-resolution review sink registry is invalid")]
    Registry(#[source] RegistryError),
    /// One snapshot repeated a candidate identity.
    #[error("writer-resolution review candidate is duplicated")]
    DuplicateCandidate {
        /// Repeated candidate identity.
        candidate: WriterCandidateId,
    },
    /// One identity carried different semantic content.
    #[error("writer-resolution review candidate identity collides")]
    CandidateCollision {
        /// Colliding candidate identity.
        candidate: WriterCandidateId,
    },
}

/// Failure to hash the normalized review artifact.
#[derive(Debug, Error)]
pub enum WriterResolutionReviewDigestError {
    /// The closed model could not be represented as JSON.
    #[error("writer-resolution review inventory could not be normalized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("writer-resolution review inventory canonical digest failed")]
    Canonical(#[source] CanonicalJsonError),
}

/// Failure to encode deterministic review JSON.
#[derive(Debug, Error)]
pub enum WriterResolutionReviewEncodeError {
    /// Pretty JSON serialization failed.
    #[error("writer-resolution review inventory could not be encoded")]
    Serialization(#[source] serde_json::Error),
}
