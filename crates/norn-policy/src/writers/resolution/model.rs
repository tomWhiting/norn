//! Closed semantic model for writer-candidate dispositions.

use serde::{Deserialize, Serialize};

use crate::writers::{WriterCandidateId, WriterToken};

/// First closed writer-resolution authority schema.
pub const WRITER_RESOLUTION_SCHEMA_VERSION: u32 = 1;

/// Fixed checked-in P1 writer-resolution authority path.
pub const WRITER_RESOLUTION_AUTHORITY_PATH: &str = "policy/writer-resolutions.toml";

/// The only reviewed outcomes admitted for an unresolved writer candidate.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriterResolutionDisposition {
    /// Resolve the candidate to an exact current sink-registry entry.
    ResolvedSink {
        /// Stable registered sink token.
        sink: WriterToken,
    },
    /// Resolve the candidate to one reviewed non-writer vocabulary token.
    ReviewedNonWriter {
        /// Stable token linking independently reviewed justification evidence.
        review: WriterToken,
    },
}

impl WriterResolutionDisposition {
    /// Return the resolved sink token, when this is a sink disposition.
    #[must_use]
    pub const fn resolved_sink(&self) -> Option<&WriterToken> {
        match self {
            Self::ResolvedSink { sink } => Some(sink),
            Self::ReviewedNonWriter { .. } => None,
        }
    }

    /// Return the non-writer review token, when this is a reviewed exclusion.
    #[must_use]
    pub const fn non_writer_review(&self) -> Option<&WriterToken> {
        match self {
            Self::ReviewedNonWriter { review } => Some(review),
            Self::ResolvedSink { .. } => None,
        }
    }
}

/// One exact reviewed disposition for one stable unresolved candidate.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterResolution {
    pub(super) candidate: WriterCandidateId,
    pub(super) disposition: WriterResolutionDisposition,
}

impl WriterResolution {
    /// Construct a reviewed resolution to an exact sink token.
    #[must_use]
    pub const fn resolved_sink(candidate: WriterCandidateId, sink: WriterToken) -> Self {
        Self {
            candidate,
            disposition: WriterResolutionDisposition::ResolvedSink { sink },
        }
    }

    /// Construct a reviewed exclusion linked to one review token.
    #[must_use]
    pub const fn reviewed_non_writer(candidate: WriterCandidateId, review: WriterToken) -> Self {
        Self {
            candidate,
            disposition: WriterResolutionDisposition::ReviewedNonWriter { review },
        }
    }

    /// Return the exact candidate identity covered by this row.
    #[must_use]
    pub const fn candidate(&self) -> WriterCandidateId {
        self.candidate
    }

    /// Borrow the closed reviewed disposition.
    #[must_use]
    pub const fn disposition(&self) -> &WriterResolutionDisposition {
        &self.disposition
    }
}
