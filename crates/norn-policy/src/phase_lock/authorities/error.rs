//! Non-disclosing P1 authority acquisition errors.

use std::fmt;

use thiserror::Error;

use crate::finding::EvidenceTraceabilityIssue;
use crate::responses_contract::ResponsesContractError;

/// Fixed P1 authority classes used by structural failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum P1AuthorityKind {
    /// `policy/phase-lock.json`.
    PhaseLock,
    /// `policy/repository.toml`.
    RepositoryPolicy,
    /// `policy/generated-includes.json`.
    GeneratedIncludes,
    /// `policy/origin/p1-computed.json`.
    Origin,
    /// `policy/governance/legacy.toml`.
    Governance,
    /// `policy/governance/p1-reviewed.toml`.
    GovernanceAnchor,
    /// `policy/writer-resolutions.toml`.
    WriterResolutions,
    /// `policy/writer-families.toml`.
    WriterFamilies,
    /// Aggregate public-plus-Codex Responses authority.
    ResponsesContract,
    /// `policy/redaction-registry.json`.
    RedactionRegistry,
    /// Pinned finding traceability registry.
    SourceFindings,
    /// Checked-in P1 gate entrypoint.
    GateEntrypoint,
    /// Checked-in P1 command manifest.
    GateManifest,
}

impl P1AuthorityKind {
    /// Return the stable machine-facing authority token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PhaseLock => "phase_lock",
            Self::RepositoryPolicy => "repository_policy",
            Self::GeneratedIncludes => "generated_includes",
            Self::Origin => "origin",
            Self::Governance => "governance",
            Self::GovernanceAnchor => "governance_anchor",
            Self::WriterResolutions => "writer_resolutions",
            Self::WriterFamilies => "writer_families",
            Self::ResponsesContract => "responses_contract",
            Self::RedactionRegistry => "redaction_registry",
            Self::SourceFindings => "source_findings",
            Self::GateEntrypoint => "gate_entrypoint",
            Self::GateManifest => "gate_manifest",
        }
    }
}

impl fmt::Display for P1AuthorityKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PhaseLock => "phase lock",
            Self::RepositoryPolicy => "repository policy",
            Self::GeneratedIncludes => "generated includes",
            Self::Origin => "origin",
            Self::Governance => "governance",
            Self::GovernanceAnchor => "governance anchor",
            Self::WriterResolutions => "writer resolutions",
            Self::WriterFamilies => "writer families",
            Self::ResponsesContract => "Responses contract",
            Self::RedactionRegistry => "redaction registry",
            Self::SourceFindings => "source findings",
            Self::GateEntrypoint => "gate entrypoint",
            Self::GateManifest => "gate manifest",
        })
    }
}

/// Closed errors from snapshot-owned P1 authority acquisition.
#[derive(Debug, Error)]
pub enum P1AuthorityError {
    /// A compiled fixed path is invalid in this evaluator build.
    #[error("a compiled P1 authority path is invalid")]
    CompiledPath,
    /// A required authority is absent from the current snapshot.
    #[error("required {0} authority is missing")]
    Missing(P1AuthorityKind),
    /// A required authority is not an ordinary file.
    #[error("required {0} authority is not a regular file")]
    NotRegular(P1AuthorityKind),
    /// A closed authority document failed decoding or validation.
    #[error("{0} authority is invalid")]
    Invalid(P1AuthorityKind),
    /// Authority normalization failed without exposing source content.
    #[error("{0} authority could not be normalized")]
    Normalization(P1AuthorityKind),
    /// A normalized or exact-byte identity differs from the phase lock.
    #[error("{0} authority does not match the phase lock")]
    Digest(P1AuthorityKind),
    /// The generated-include authority identity could not be encoded.
    #[error("generated-include authority identity could not be computed")]
    GeneratedIncludeIdentity,
    /// The supplied base snapshot or generated registry is not the exact P1 base.
    #[error("exact P1 base reconstruction failed")]
    ExactBase,
    /// Exact-base facts could not produce the immutable origin.
    #[error("exact P1 origin reconstruction failed")]
    OriginReconstruction,
    /// The checked-in origin differs from complete exact-base reconstruction.
    #[error("origin authority differs from exact P1 reconstruction")]
    OriginMismatch,
    /// Policy ceilings could not be converted into canonical baseline limits.
    #[error("repository-policy ceilings are invalid")]
    PolicyCeilings,
    /// Governance does not exactly cover the immutable legacy origin set.
    #[error("legacy governance does not match the immutable origin")]
    GovernanceLink,
    /// The reviewed anchor does not exactly cover the immutable legacy origin.
    #[error("reviewed governance anchor does not match the immutable origin")]
    GovernanceAnchorLink,
    /// Current governance loosens the compiled reviewed anchor.
    #[error("legacy governance does not tighten the reviewed anchor")]
    GovernanceTransition,
    /// Writer families do not exactly cover compatible immutable operations.
    #[error("writer-family authority does not match the immutable origin")]
    WriterFamilyLink,
    /// The complete Responses corpus or its fixed sources are invalid.
    #[error("Responses contract authority is invalid")]
    ResponsesContract,
    /// Valid Responses evidence disagrees with its closed traceability authority.
    #[error("Responses evidence traceability is incomplete or inconsistent")]
    EvidenceTraceability {
        /// Closed semantic disagreement class.
        issue: EvidenceTraceabilityIssue,
        /// Number of disagreements in the reported class; always nonzero.
        count: u64,
    },
}

impl From<ResponsesContractError> for P1AuthorityError {
    fn from(error: ResponsesContractError) -> Self {
        match error {
            ResponsesContractError::EvidenceTraceability { issue, count } => {
                Self::EvidenceTraceability { issue, count }
            }
            _ => Self::ResponsesContract,
        }
    }
}
