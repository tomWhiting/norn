//! Strict TOML authority for reviewed writer-candidate resolutions.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::str::Utf8Error;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::version::DIGEST_VERSION;
use crate::writers::{
    RegistryError, SinkRegistry, WRITER_ANALYZER_VERSION, WriterCandidate, WriterCandidateId,
    WriterToken,
};

use super::coverage::{WriterResolutionCoverage, WriterResolutionCoverageError};
use super::model::{
    WRITER_RESOLUTION_SCHEMA_VERSION, WriterResolution, WriterResolutionDisposition,
};

/// Validated review authority covering the exact base/current candidate union.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterResolutionAuthority {
    schema_version: u32,
    algorithms: ResolutionAlgorithms,
    sink_registry: Digest,
    review_inventory: Digest,
    non_writer_reviews: Vec<WriterToken>,
    resolutions: Vec<WriterResolution>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResolutionAlgorithms {
    writer: String,
    digest: String,
}

impl WriterResolutionAuthority {
    /// Construct deterministic authority from already reviewed dispositions.
    ///
    /// # Errors
    ///
    /// Applies the same exact metadata, order, coverage, sink, and vocabulary
    /// validation as the checked-in TOML decoder.
    pub fn author_p1(
        base: &[WriterCandidate],
        current: &[WriterCandidate],
        registry: &SinkRegistry,
        non_writer_reviews: Vec<WriterToken>,
        resolutions: Vec<WriterResolution>,
    ) -> Result<Self, WriterResolutionAuthorityError> {
        let coverage = coverage(base, current)?;
        Self {
            schema_version: WRITER_RESOLUTION_SCHEMA_VERSION,
            algorithms: ResolutionAlgorithms {
                writer: WRITER_ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            sink_registry: registry.digest(),
            review_inventory: coverage.review_inventory(),
            non_writer_reviews,
            resolutions,
        }
        .validate(&coverage, registry)
    }

    /// Decode and validate a complete `policy/writer-resolutions.toml`.
    ///
    /// The authority binds the exact writer and digest algorithms, current sink
    /// registry, and semantic union of immutable-base and current candidates.
    /// Every candidate requires exactly one disposition and every resolution
    /// must name a union member. Resolved-sink rows only prove that the named
    /// sink exists in the current registry; form and flow applicability remain
    /// the responsibility of the later analyzer-application boundary.
    ///
    /// # Errors
    ///
    /// Rejects non-UTF-8 or open-schema TOML, unsupported metadata, binding
    /// drift, unordered or duplicated rows, identity collisions, incomplete
    /// coverage, unknown sinks, and an inexact non-writer vocabulary.
    pub fn decode_p1(
        bytes: &[u8],
        base: &[WriterCandidate],
        current: &[WriterCandidate],
        registry: &SinkRegistry,
    ) -> Result<Self, WriterResolutionAuthorityError> {
        let text = std::str::from_utf8(bytes).map_err(WriterResolutionAuthorityError::Utf8)?;
        let authority: Self = toml::from_str(text).map_err(WriterResolutionAuthorityError::Toml)?;
        let coverage = coverage(base, current)?;
        authority.validate(&coverage, registry)
    }

    /// Return the closed resolution-authority schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the exact sink-registry digest bound by this authority.
    #[must_use]
    pub const fn sink_registry(&self) -> Digest {
        self.sink_registry
    }

    /// Return the exact base/current semantic-union digest.
    #[must_use]
    pub const fn review_inventory(&self) -> Digest {
        self.review_inventory
    }

    /// Borrow the exact sorted non-writer review vocabulary.
    #[must_use]
    pub fn non_writer_reviews(&self) -> &[WriterToken] {
        &self.non_writer_reviews
    }

    /// Borrow the exact sorted candidate resolutions.
    #[must_use]
    pub fn resolutions(&self) -> &[WriterResolution] {
        &self.resolutions
    }

    /// Find the one reviewed resolution for a covered candidate.
    #[must_use]
    pub fn resolution_for(&self, candidate: WriterCandidateId) -> Option<&WriterResolution> {
        match self
            .resolutions
            .binary_search_by_key(&candidate, WriterResolution::candidate)
        {
            Ok(index) => self.resolutions.get(index),
            Err(_) => None,
        }
    }

    /// Hash normalized authority semantics rather than TOML formatting.
    ///
    /// # Errors
    ///
    /// Returns an error only if the closed model cannot be represented as
    /// canonical JSON.
    pub fn normalized_digest(&self) -> Result<Digest, WriterResolutionDigestError> {
        let value =
            serde_json::to_value(self).map_err(WriterResolutionDigestError::Serialization)?;
        digest_json(&value).map_err(WriterResolutionDigestError::Canonical)
    }

    /// Encode one deterministic checked-in P1 TOML document.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the validated authority cannot be
    /// represented as TOML.
    pub fn encode_p1(&self) -> Result<Vec<u8>, WriterResolutionEncodeError> {
        let mut text = toml::to_string(self).map_err(WriterResolutionEncodeError::Serialization)?;
        text.truncate(text.trim_end_matches('\n').len());
        text.push('\n');
        Ok(text.into_bytes())
    }

    fn validate(
        self,
        coverage: &WriterResolutionCoverage,
        registry: &SinkRegistry,
    ) -> Result<Self, WriterResolutionAuthorityError> {
        validate_metadata(&self, coverage, registry)?;
        validate_review_order(&self.non_writer_reviews)?;
        validate_resolution_order(&self.resolutions)?;
        validate_coverage(coverage, &self.resolutions)?;
        validate_dispositions(registry, &self.non_writer_reviews, &self.resolutions)?;
        Ok(self)
    }
}

fn coverage(
    base: &[WriterCandidate],
    current: &[WriterCandidate],
) -> Result<WriterResolutionCoverage, WriterResolutionAuthorityError> {
    WriterResolutionCoverage::for_snapshots(base, current).map_err(|error| match error {
        WriterResolutionCoverageError::Duplicate { candidate } => {
            WriterResolutionAuthorityError::DuplicateCandidate { candidate }
        }
        WriterResolutionCoverageError::Collision { candidate } => {
            WriterResolutionAuthorityError::CandidateCollision { candidate }
        }
    })
}

fn validate_metadata(
    authority: &WriterResolutionAuthority,
    coverage: &WriterResolutionCoverage,
    registry: &SinkRegistry,
) -> Result<(), WriterResolutionAuthorityError> {
    if authority.schema_version != WRITER_RESOLUTION_SCHEMA_VERSION {
        return Err(WriterResolutionAuthorityError::SchemaVersion);
    }
    if authority.algorithms.writer != WRITER_ANALYZER_VERSION {
        return Err(WriterResolutionAuthorityError::WriterAlgorithm);
    }
    if authority.algorithms.digest != DIGEST_VERSION {
        return Err(WriterResolutionAuthorityError::DigestAlgorithm);
    }
    registry
        .validate()
        .map_err(WriterResolutionAuthorityError::Registry)?;
    if authority.sink_registry != registry.digest() {
        return Err(WriterResolutionAuthorityError::SinkRegistry);
    }
    if authority.review_inventory != coverage.review_inventory() {
        return Err(WriterResolutionAuthorityError::ReviewInventory);
    }
    Ok(())
}

fn validate_review_order(reviews: &[WriterToken]) -> Result<(), WriterResolutionAuthorityError> {
    for pair in reviews.windows(2) {
        match pair[0].cmp(&pair[1]) {
            Ordering::Less => {}
            Ordering::Equal => return Err(WriterResolutionAuthorityError::DuplicateReview),
            Ordering::Greater => return Err(WriterResolutionAuthorityError::ReviewOrder),
        }
    }
    Ok(())
}

fn validate_resolution_order(
    resolutions: &[WriterResolution],
) -> Result<(), WriterResolutionAuthorityError> {
    for pair in resolutions.windows(2) {
        match pair[0].candidate().cmp(&pair[1].candidate()) {
            Ordering::Less => {}
            Ordering::Greater => return Err(WriterResolutionAuthorityError::ResolutionOrder),
            Ordering::Equal if pair[0].disposition() == pair[1].disposition() => {
                return Err(WriterResolutionAuthorityError::DuplicateResolution {
                    candidate: pair[0].candidate(),
                });
            }
            Ordering::Equal => {
                return Err(WriterResolutionAuthorityError::ResolutionCollision {
                    candidate: pair[0].candidate(),
                });
            }
        }
    }
    Ok(())
}

fn validate_coverage(
    coverage: &WriterResolutionCoverage,
    resolutions: &[WriterResolution],
) -> Result<(), WriterResolutionAuthorityError> {
    for resolution in resolutions {
        if !coverage.contains(resolution.candidate()) {
            return Err(WriterResolutionAuthorityError::StaleResolution {
                candidate: resolution.candidate(),
            });
        }
    }
    let resolved = resolutions
        .iter()
        .map(WriterResolution::candidate)
        .collect::<BTreeSet<_>>();
    for (candidate, _) in coverage.candidates() {
        if !resolved.contains(candidate) {
            return Err(WriterResolutionAuthorityError::MissingResolution {
                candidate: *candidate,
            });
        }
    }
    Ok(())
}

fn validate_dispositions(
    registry: &SinkRegistry,
    review_vocabulary: &[WriterToken],
    resolutions: &[WriterResolution],
) -> Result<(), WriterResolutionAuthorityError> {
    let declared = review_vocabulary.iter().collect::<BTreeSet<_>>();
    let mut used = BTreeSet::new();
    for resolution in resolutions {
        match resolution.disposition() {
            WriterResolutionDisposition::ResolvedSink { sink } => {
                if !registry.specs().iter().any(|spec| spec.id() == sink) {
                    return Err(WriterResolutionAuthorityError::UnknownSink {
                        candidate: resolution.candidate(),
                    });
                }
            }
            WriterResolutionDisposition::ReviewedNonWriter { review } => {
                if !declared.contains(review) {
                    return Err(WriterResolutionAuthorityError::MissingReview {
                        candidate: resolution.candidate(),
                    });
                }
                used.insert(review);
            }
        }
    }
    if declared != used {
        return Err(WriterResolutionAuthorityError::StaleReview);
    }
    Ok(())
}

/// Closed structural failure while decoding reviewed writer resolutions.
#[derive(Debug, Error)]
pub enum WriterResolutionAuthorityError {
    /// Bytes were not complete UTF-8.
    #[error("writer resolution authority is not UTF-8")]
    Utf8(#[source] Utf8Error),
    /// TOML was malformed, duplicated a key, or violated the closed schema.
    #[error("writer resolution authority is not valid closed-schema TOML")]
    Toml(#[source] toml::de::Error),
    /// The authority schema version is unsupported.
    #[error("writer resolution authority schema version is unsupported")]
    SchemaVersion,
    /// The authority names a different writer analyzer.
    #[error("writer resolution authority has the wrong writer algorithm")]
    WriterAlgorithm,
    /// The authority names a different canonical digest algorithm.
    #[error("writer resolution authority has the wrong digest algorithm")]
    DigestAlgorithm,
    /// The supplied sink registry was internally invalid.
    #[error("writer resolution sink registry is invalid")]
    Registry(#[source] RegistryError),
    /// The authority binds a different sink-registry digest.
    #[error("writer resolution authority binds a different sink registry")]
    SinkRegistry,
    /// One candidate inventory repeated an identity.
    #[error("writer resolution candidate inventory contains a duplicate")]
    DuplicateCandidate {
        /// Repeated candidate identity.
        candidate: WriterCandidateId,
    },
    /// One candidate identity carried different semantic content.
    #[error("writer resolution candidate identity has a semantic collision")]
    CandidateCollision {
        /// Colliding candidate identity.
        candidate: WriterCandidateId,
    },
    /// The authority binds a different base/current semantic union.
    #[error("writer resolution authority binds a different review inventory")]
    ReviewInventory,
    /// Non-writer review tokens were not strictly sorted.
    #[error("writer resolution non-writer reviews are not strictly sorted")]
    ReviewOrder,
    /// A non-writer review token was declared more than once.
    #[error("writer resolution non-writer review is duplicated")]
    DuplicateReview,
    /// Resolution rows were not strictly sorted by candidate identity.
    #[error("writer resolution rows are not strictly sorted")]
    ResolutionOrder,
    /// An identical candidate disposition was repeated.
    #[error("writer resolution row is duplicated")]
    DuplicateResolution {
        /// Repeated candidate identity.
        candidate: WriterCandidateId,
    },
    /// One candidate was assigned conflicting dispositions.
    #[error("writer resolution row has a disposition collision")]
    ResolutionCollision {
        /// Conflicting candidate identity.
        candidate: WriterCandidateId,
    },
    /// A current or base candidate has no reviewed disposition.
    #[error("writer resolution authority omits a candidate")]
    MissingResolution {
        /// Candidate omitted from reviewed authority.
        candidate: WriterCandidateId,
    },
    /// A resolution names no current or base candidate.
    #[error("writer resolution authority contains a stale candidate")]
    StaleResolution {
        /// Candidate absent from the reviewed union.
        candidate: WriterCandidateId,
    },
    /// A resolved-sink disposition names no current registered sink.
    #[error("writer resolution authority names an unknown sink")]
    UnknownSink {
        /// Candidate whose disposition names an absent sink.
        candidate: WriterCandidateId,
    },
    /// A reviewed-non-writer disposition names no declared review token.
    #[error("writer resolution authority omits a used non-writer review token")]
    MissingReview {
        /// Candidate whose review token was undeclared.
        candidate: WriterCandidateId,
    },
    /// A declared non-writer review token is unused.
    #[error("writer resolution authority contains an unused non-writer review token")]
    StaleReview,
}

/// Failure to hash normalized writer-resolution semantics.
#[derive(Debug, Error)]
pub enum WriterResolutionDigestError {
    /// The closed model could not be represented as JSON.
    #[error("writer resolution authority could not be normalized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("writer resolution authority canonical digest failed")]
    Canonical(#[source] CanonicalJsonError),
}

/// Failure to encode deterministic checked-in writer-resolution TOML.
#[derive(Debug, Error)]
pub enum WriterResolutionEncodeError {
    /// TOML serialization failed.
    #[error("writer resolution authority could not be encoded")]
    Serialization(#[source] toml::ser::Error),
}
