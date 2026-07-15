//! Exact base/current semantic-union coverage for reviewed candidates.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::digest::{Digest, digest_bytes};

use crate::writers::{WriterCandidate, WriterCandidateId};

const REVIEW_INVENTORY_DOMAIN: &[u8] = b"norn-writer-resolution-inventory-1";

/// Opaque proof of one exact base/current unresolved-candidate semantic union.
#[derive(Clone, Debug)]
pub struct WriterResolutionCoverage {
    candidates: BTreeMap<WriterCandidateId, WriterCandidate>,
    review_inventory: Digest,
}

impl WriterResolutionCoverage {
    /// Construct exact union coverage for immutable-base and current candidates.
    ///
    /// Identical candidates present in both inventories collapse to one union
    /// member. A repeated identity within either inventory is rejected, while
    /// the same identity carrying different semantics is always a collision.
    /// Diagnostic byte spans do not participate in semantic equality.
    ///
    /// # Errors
    ///
    /// Returns a duplicate or candidate-identity collision error.
    pub fn for_snapshots(
        base: &[WriterCandidate],
        current: &[WriterCandidate],
    ) -> Result<Self, WriterResolutionCoverageError> {
        let base = unique_inventory(base)?;
        let current = unique_inventory(current)?;
        let mut candidates = base;
        for (id, candidate) in current {
            if let Some(existing) = candidates.get(&id) {
                if !existing.same_semantics(&candidate) {
                    return Err(WriterResolutionCoverageError::Collision { candidate: id });
                }
                continue;
            }
            candidates.insert(id, candidate);
        }
        let review_inventory = review_inventory_digest(&candidates);
        Ok(Self {
            candidates,
            review_inventory,
        })
    }

    /// Return the digest of the complete semantic union.
    #[must_use]
    pub const fn review_inventory(&self) -> Digest {
        self.review_inventory
    }

    /// Return the number of unique semantic candidates under review.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Return whether the semantic union has no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub(super) fn candidates(
        &self,
    ) -> impl ExactSizeIterator<Item = (&WriterCandidateId, &WriterCandidate)> {
        self.candidates.iter()
    }

    pub(super) fn contains(&self, candidate: WriterCandidateId) -> bool {
        self.candidates.contains_key(&candidate)
    }
}

fn unique_inventory(
    candidates: &[WriterCandidate],
) -> Result<BTreeMap<WriterCandidateId, WriterCandidate>, WriterResolutionCoverageError> {
    let mut unique: BTreeMap<WriterCandidateId, WriterCandidate> = BTreeMap::new();
    for candidate in candidates {
        let id = candidate.id();
        if let Some(existing) = unique.get(&id) {
            return if existing.same_semantics(candidate) {
                Err(WriterResolutionCoverageError::Duplicate { candidate: id })
            } else {
                Err(WriterResolutionCoverageError::Collision { candidate: id })
            };
        }
        unique.insert(id, candidate.clone());
    }
    Ok(unique)
}

fn review_inventory_digest(candidates: &BTreeMap<WriterCandidateId, WriterCandidate>) -> Digest {
    let mut framed = Vec::new();
    field(&mut framed, REVIEW_INVENTORY_DOMAIN);
    for (id, candidate) in candidates {
        field(&mut framed, id.digest().as_bytes());
        field(&mut framed, candidate.path().as_str().as_bytes());
        field(&mut framed, candidate.enclosing_item().as_bytes());
        field(&mut framed, candidate.normalized_call().as_bytes());
        field(&mut framed, candidate.candidate().as_str().as_bytes());
        field(&mut framed, candidate.reason().token().as_bytes());
        field(&mut framed, candidate.form().token().as_bytes());
        field(&mut framed, &candidate.ordinal().to_be_bytes());
    }
    digest_bytes(&framed)
}

fn field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}

/// Failure to construct exact base/current candidate coverage.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WriterResolutionCoverageError {
    /// One snapshot repeated the same semantic candidate identity.
    #[error("writer candidate inventory contains a duplicate identity")]
    Duplicate {
        /// Repeated stable candidate identity.
        candidate: WriterCandidateId,
    },
    /// One candidate identity was associated with different semantics.
    #[error("writer candidate identity collides across different semantics")]
    Collision {
        /// Colliding stable candidate identity.
        candidate: WriterCandidateId,
    },
}
