//! Public P1 state and read-only report data.

use serde::Serialize;

use crate::Digest;
use crate::baseline::{LegacyDisposition, LegacyGovernance, OriginLedger};
use crate::config::RepositoryPolicy;
use crate::facts::{FactFailure, RepositoryFactsError};
use crate::finding::{EvidenceTraceabilityIssue, Finding};
use crate::phase_lock::{CampaignPhase, P1AuthorityError, P1AuthorityKind, ReadyP1Authorities};
use crate::redaction::RedactionRegistry;
use crate::rust::modules::GeneratedIncludeRegistry;
use crate::writers::WriterFamilyRegistry;

/// Explicit state of the P1 repository policy profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum PolicyState {
    /// The fixed phase-lock marker is absent, so generic Norn behavior applies.
    Absent,
    /// Every authority and fact family is valid; hard findings are available.
    Ready(PolicyReport),
    /// A marker exists but required authority or fact construction failed.
    Invalid(InvalidPolicy),
}

/// Complete deterministic output of one ready P1 evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyReport {
    source_inventory: Digest,
    findings: Vec<Finding>,
    legacy_dispositions: Vec<LegacyDisposition>,
}

impl PolicyReport {
    pub(super) fn new(
        source_inventory: Digest,
        findings: Vec<Finding>,
        legacy_dispositions: Vec<LegacyDisposition>,
    ) -> Self {
        Self {
            source_inventory,
            findings,
            legacy_dispositions,
        }
    }

    /// Return the complete analyzed source-inventory identity.
    #[must_use]
    pub const fn source_inventory(&self) -> Digest {
        self.source_inventory
    }

    /// Borrow every hard finding in canonical order.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Borrow each immutable legacy exception's derived current state.
    #[must_use]
    pub fn legacy_dispositions(&self) -> &[LegacyDisposition] {
        &self.legacy_dispositions
    }

    /// Return whether the ready repository has no hard finding.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Persistent invalid state for a repository carrying the P1 marker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InvalidPolicy {
    reason: PolicyInvalidReason,
}

impl InvalidPolicy {
    pub(super) const fn new(reason: PolicyInvalidReason) -> Self {
        Self { reason }
    }

    pub(super) fn authority(error: &P1AuthorityError) -> Self {
        Self::new(authority_reason(error))
    }

    /// Borrow the closed non-disclosing invalid-state reason.
    #[must_use]
    pub const fn reason(&self) -> &PolicyInvalidReason {
        &self.reason
    }
}

/// Closed reason a marked repository could not produce a ready report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyInvalidReason {
    /// Fixed authority acquisition, decoding, linkage, or pinning failed.
    Authority {
        /// Authority when the failure identifies one fixed input.
        authority: Option<PolicyAuthority>,
        /// Closed authority failure class.
        issue: AuthorityIssue,
    },
    /// Canonical current facts were incomplete or incoherent.
    CurrentFacts {
        /// Closed failed fact-family invariant.
        issue: CurrentFactIssue,
        /// Stable analyzer failures, when construction supplied them.
        failures: Vec<FactFailure>,
    },
    /// Complete canonical facts could not be projected losslessly.
    CurrentProjection,
    /// Current compile-test provenance differs from the immutable origin.
    CompileTestFixtureDrift,
    /// Validated policy limits could not form baseline ceilings.
    PolicyCeilings,
    /// Immutable origin and current legacy state could not be compared.
    LegacyEvaluation,
    /// Stable hard findings could not be constructed losslessly.
    FindingConstruction,
}

/// Fixed P1 authority identities used by invalid-state reports.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAuthority {
    /// Phase-lock marker and pins.
    PhaseLock,
    /// Hard repository policy.
    RepositoryPolicy,
    /// Generated-include technical registry.
    GeneratedIncludes,
    /// Immutable computed origin.
    Origin,
    /// Reviewed legacy governance.
    Governance,
    /// Compiled last-reviewed governance anchor.
    GovernanceAnchor,
    /// Reviewed unresolved-writer dispositions.
    WriterResolutions,
    /// Reviewed writer-family classifications.
    WriterFamilies,
    /// Aggregate public-plus-Codex Responses contract.
    ResponsesContract,
    /// Complete retained-artifact registry.
    RedactionRegistry,
    /// Source-review finding traceability registry.
    SourceFindings,
    /// Local gate entrypoint.
    GateEntrypoint,
    /// Local gate command manifest.
    GateManifest,
}

/// Closed authority acquisition failure class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityIssue {
    /// A compiled fixed path was invalid.
    CompiledPath,
    /// A fixed input was missing.
    Missing,
    /// A fixed input was not a regular file.
    NotRegular,
    /// A fixed input failed closed decoding or validation.
    Invalid,
    /// Semantic normalization failed.
    Normalization,
    /// A normalized or exact-byte digest differed from the lock.
    Digest,
    /// Generated-include technical identity could not be encoded.
    GeneratedIncludeIdentity,
    /// Exact-base acquisition failed.
    ExactBase,
    /// Immutable origin reconstruction failed.
    OriginReconstruction,
    /// Checked-in origin differed from complete reconstruction.
    OriginMismatch,
    /// Policy ceilings were structurally impossible.
    PolicyCeilings,
    /// Legacy governance did not cover exact origin.
    GovernanceLink,
    /// Current governance loosened the last reviewed anchor.
    GovernanceTransition,
    /// Writer-family authority did not cover exact origin.
    WriterFamilyLink,
    /// Responses contract authority was invalid.
    ResponsesContract,
    /// Responses evidence disagreed with its closed traceability authority.
    EvidenceTraceability {
        /// Closed semantic disagreement class.
        issue: EvidenceTraceabilityIssue,
        /// Number of disagreements in the reported class.
        count: u64,
    },
}

/// Closed canonical current-fact failure class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentFactIssue {
    /// Cargo discovery was invalid.
    Cargo,
    /// Rust reachability was invalid.
    Modules,
    /// One or more analyzers reported construction failures.
    ConstructionFailures,
    /// Writer inventory was unavailable or stale.
    WriterInventory,
    /// Classified source inventory was incomplete or stale.
    SourceInventory,
    /// Compile-test fixture provenance was incomplete or incoherent.
    CompileTestFixtures,
    /// Production source inventory was incomplete or stale.
    ProductionInventory,
    /// Item facts referenced no classified source.
    ItemSource,
    /// Debt facts referenced no classified source.
    DebtSource,
    /// Writer facts referenced no production source.
    WriterSource,
}

#[derive(Clone, Copy)]
pub(super) struct AuthorityView<'a> {
    pub(super) repository_policy: &'a RepositoryPolicy,
    pub(super) generated_includes: &'a GeneratedIncludeRegistry,
    pub(super) origin: &'a OriginLedger,
    pub(super) governance: &'a LegacyGovernance,
    pub(super) writer_families: &'a WriterFamilyRegistry,
    pub(super) redaction: &'a RedactionRegistry,
    pub(super) active_phase: CampaignPhase,
}

impl<'a> AuthorityView<'a> {
    pub(super) const fn from_ready(ready: &'a ReadyP1Authorities) -> Self {
        Self {
            repository_policy: ready.repository_policy(),
            generated_includes: ready.generated_includes(),
            origin: ready.origin(),
            governance: ready.governance(),
            writer_families: ready.writer_families(),
            redaction: ready.redaction(),
            active_phase: ready.lock().active_phase(),
        }
    }
}

pub(super) fn invalid_current_facts(
    error: RepositoryFactsError,
    failures: &[FactFailure],
) -> InvalidPolicy {
    InvalidPolicy::new(PolicyInvalidReason::CurrentFacts {
        issue: current_fact_issue(error),
        failures: failures.to_vec(),
    })
}

fn authority_reason(error: &P1AuthorityError) -> PolicyInvalidReason {
    let (authority, issue) = match error {
        P1AuthorityError::CompiledPath => (None, AuthorityIssue::CompiledPath),
        P1AuthorityError::Missing(kind) => (Some((*kind).into()), AuthorityIssue::Missing),
        P1AuthorityError::NotRegular(kind) => (Some((*kind).into()), AuthorityIssue::NotRegular),
        P1AuthorityError::Invalid(kind) => (Some((*kind).into()), AuthorityIssue::Invalid),
        P1AuthorityError::Normalization(kind) => {
            (Some((*kind).into()), AuthorityIssue::Normalization)
        }
        P1AuthorityError::Digest(kind) => (Some((*kind).into()), AuthorityIssue::Digest),
        P1AuthorityError::GeneratedIncludeIdentity => (
            Some(PolicyAuthority::GeneratedIncludes),
            AuthorityIssue::GeneratedIncludeIdentity,
        ),
        P1AuthorityError::ExactBase => (None, AuthorityIssue::ExactBase),
        P1AuthorityError::OriginReconstruction => (
            Some(PolicyAuthority::Origin),
            AuthorityIssue::OriginReconstruction,
        ),
        P1AuthorityError::OriginMismatch => (
            Some(PolicyAuthority::Origin),
            AuthorityIssue::OriginMismatch,
        ),
        P1AuthorityError::PolicyCeilings => (
            Some(PolicyAuthority::RepositoryPolicy),
            AuthorityIssue::PolicyCeilings,
        ),
        P1AuthorityError::GovernanceLink => (
            Some(PolicyAuthority::Governance),
            AuthorityIssue::GovernanceLink,
        ),
        P1AuthorityError::GovernanceAnchorLink => (
            Some(PolicyAuthority::GovernanceAnchor),
            AuthorityIssue::GovernanceLink,
        ),
        P1AuthorityError::GovernanceTransition => (
            Some(PolicyAuthority::Governance),
            AuthorityIssue::GovernanceTransition,
        ),
        P1AuthorityError::WriterFamilyLink => (
            Some(PolicyAuthority::WriterFamilies),
            AuthorityIssue::WriterFamilyLink,
        ),
        P1AuthorityError::ResponsesContract => (
            Some(PolicyAuthority::ResponsesContract),
            AuthorityIssue::ResponsesContract,
        ),
        P1AuthorityError::EvidenceTraceability { issue, count } => (
            Some(PolicyAuthority::ResponsesContract),
            AuthorityIssue::EvidenceTraceability {
                issue: *issue,
                count: *count,
            },
        ),
    };
    PolicyInvalidReason::Authority { authority, issue }
}

impl From<P1AuthorityKind> for PolicyAuthority {
    fn from(value: P1AuthorityKind) -> Self {
        match value {
            P1AuthorityKind::PhaseLock => Self::PhaseLock,
            P1AuthorityKind::RepositoryPolicy => Self::RepositoryPolicy,
            P1AuthorityKind::GeneratedIncludes => Self::GeneratedIncludes,
            P1AuthorityKind::Origin => Self::Origin,
            P1AuthorityKind::Governance => Self::Governance,
            P1AuthorityKind::GovernanceAnchor => Self::GovernanceAnchor,
            P1AuthorityKind::WriterResolutions => Self::WriterResolutions,
            P1AuthorityKind::WriterFamilies => Self::WriterFamilies,
            P1AuthorityKind::ResponsesContract => Self::ResponsesContract,
            P1AuthorityKind::RedactionRegistry => Self::RedactionRegistry,
            P1AuthorityKind::SourceFindings => Self::SourceFindings,
            P1AuthorityKind::GateEntrypoint => Self::GateEntrypoint,
            P1AuthorityKind::GateManifest => Self::GateManifest,
        }
    }
}

const fn current_fact_issue(error: RepositoryFactsError) -> CurrentFactIssue {
    match error {
        RepositoryFactsError::Cargo => CurrentFactIssue::Cargo,
        RepositoryFactsError::Modules => CurrentFactIssue::Modules,
        RepositoryFactsError::ConstructionFailures => CurrentFactIssue::ConstructionFailures,
        RepositoryFactsError::WriterUnavailable
        | RepositoryFactsError::WriterMetadata
        | RepositoryFactsError::WriterRegistry
        | RepositoryFactsError::WriterInventoryLength
        | RepositoryFactsError::WriterInventoryRow { .. } => CurrentFactIssue::WriterInventory,
        RepositoryFactsError::SourceDigest
        | RepositoryFactsError::SourceInventoryLength
        | RepositoryFactsError::SourceInventoryRow { .. } => CurrentFactIssue::SourceInventory,
        RepositoryFactsError::CompileTestFixtureProjection
        | RepositoryFactsError::CompileTestFixtureOrder { .. }
        | RepositoryFactsError::CompileTestFixtureSource { .. }
        | RepositoryFactsError::CompileTestFixtureClassification { .. } => {
            CurrentFactIssue::CompileTestFixtures
        }
        RepositoryFactsError::ProductionInventoryLength
        | RepositoryFactsError::ProductionInventoryRow { .. } => {
            CurrentFactIssue::ProductionInventory
        }
        RepositoryFactsError::ItemSource { .. } => CurrentFactIssue::ItemSource,
        RepositoryFactsError::DebtSource { .. } => CurrentFactIssue::DebtSource,
        RepositoryFactsError::WriterSource { .. } => CurrentFactIssue::WriterSource,
    }
}
