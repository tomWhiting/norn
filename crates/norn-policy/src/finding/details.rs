//! Code-implied finding payloads.

use serde::Serialize;

use crate::digest::Digest;

use super::{
    CargoManifestIssue, CargoTargetIssue, CargoTargetKind, DebtConstructKind, DebtTargetKind,
    EvidenceRedactionIssue, EvidenceTraceabilityIssue, FindingCode, FindingPhase,
    FindingRuleFamily, GeneratedIncludeIssue, LegacyChangeIssue, LegacyKind, ModuleResolutionIssue,
    ModuleShapeIssue, PolicyInput, PolicyInputIssue, UnknownWriterIssue, UnsupportedEntryKind,
    WriterClassificationIssue,
};

/// A repository-path finding with one exact, code-implied payload shape.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "code", content = "fields")]
pub enum RepositoryFinding {
    /// A required policy input is absent.
    #[serde(rename = "policy.input_missing")]
    PolicyInputMissing {
        /// Closed input identity.
        input: PolicyInput,
    },
    /// A required policy input could not be acquired.
    #[serde(rename = "policy.input_unreadable")]
    PolicyInputUnreadable {
        /// Closed input identity.
        input: PolicyInput,
        /// Closed acquisition failure.
        issue: PolicyInputIssue,
    },
    /// A required policy input is structurally invalid.
    #[serde(rename = "policy.input_invalid")]
    PolicyInputInvalid {
        /// Closed input identity.
        input: PolicyInput,
        /// Closed structural failure.
        issue: PolicyInputIssue,
    },
    /// A policy document uses an unsupported schema version.
    #[serde(rename = "policy.schema_unknown")]
    UnknownSchemaVersion {
        /// Closed input identity.
        input: PolicyInput,
        /// Unsupported numeric schema version.
        schema_version: u64,
    },
    /// A pinned digest does not match computed content.
    #[serde(rename = "policy.digest_mismatch")]
    DigestMismatch {
        /// Closed input identity.
        input: PolicyInput,
        /// Authority digest.
        expected: Digest,
        /// Observed digest.
        actual: Digest,
    },
    /// A snapshot entry is a symbolic link.
    #[serde(rename = "snapshot.symlink")]
    SymlinkEntry,
    /// A snapshot entry is not a supported ordinary file.
    #[serde(rename = "snapshot.unsupported_entry")]
    UnsupportedEntry {
        /// Closed observed entry class.
        actual: UnsupportedEntryKind,
    },
    /// A Cargo manifest cannot be analyzed strictly.
    #[serde(rename = "rust.manifest_invalid")]
    InvalidCargoManifest {
        /// Closed manifest-discovery failure.
        issue: CargoManifestIssue,
    },
    /// A Cargo target cannot be classified strictly.
    #[serde(rename = "rust.target_invalid")]
    InvalidCargoTarget {
        /// Target class when Cargo supplied one.
        #[serde(skip_serializing_if = "Option::is_none")]
        target_kind: Option<CargoTargetKind>,
        /// Closed target-discovery failure.
        issue: CargoTargetIssue,
    },
    /// A Rust source is not classified by production or test authority.
    #[serde(rename = "rust.source_unclassified")]
    UnclassifiedRustSource,
    /// Rust module resolution failed closed.
    #[serde(rename = "rust.module_resolution")]
    ModuleResolution {
        /// Closed module-resolution failure.
        issue: ModuleResolutionIssue,
    },
    /// A generated include is absent, changed, or unregistered.
    #[serde(rename = "rust.generated_include")]
    GeneratedInclude {
        /// Closed generated-include authority failure.
        issue: GeneratedIncludeIssue,
    },
    /// A production Rust file exceeds its applicable limit.
    #[serde(rename = "rust.loc_exceeded")]
    ProductionLocExceeded {
        /// Observed production LOC.
        actual: u64,
        /// Applicable hard limit.
        limit: u64,
    },
    /// A production `mod.rs` contains a prohibited top-level form.
    #[serde(rename = "rust.module_shape")]
    ModuleShape {
        /// Closed top-level construct class.
        construct_kind: ModuleShapeIssue,
    },
    /// An origin production item moved behind a test-only predicate.
    #[serde(rename = "rust.production_hidden_as_test")]
    ProductionHiddenAsTest {
        /// Complete stable item-group identity.
        fingerprint: Digest,
        /// Number of reclassified multiset occurrences.
        count: u64,
    },
    /// A prohibited source, manifest, or command construct is active.
    #[serde(rename = "debt.prohibited")]
    ProhibitedDebt {
        /// Closed compilation-target class.
        target_kind: DebtTargetKind,
        /// Closed prohibited construct class.
        construct_kind: DebtConstructKind,
        /// Complete stable occurrence identity.
        fingerprint: Digest,
    },
    /// A legacy exception differs from its immutable origin or state.
    #[serde(rename = "baseline.legacy_changed")]
    LegacyExceptionChanged {
        /// Complete immutable origin identity.
        origin: Digest,
        /// Closed exception family.
        kind: LegacyKind,
        /// Closed comparison failure.
        issue: LegacyChangeIssue,
    },
    /// An active legacy exception reached or passed its due phase.
    #[serde(rename = "baseline.legacy_overdue")]
    LegacyExceptionOverdue {
        /// Complete immutable origin identity.
        origin: Digest,
        /// Closed exception family.
        kind: LegacyKind,
        /// Reviewed hard due phase.
        due_phase: FindingPhase,
    },
    /// A possible writer operation cannot be resolved to a registered sink.
    #[serde(rename = "writer.unknown_sink")]
    UnknownWriterSink {
        /// Complete stable unresolved-candidate identity.
        fingerprint: Digest,
        /// Closed resolution failure.
        issue: UnknownWriterIssue,
    },
    /// A writer operation has an invalid classification row.
    #[serde(rename = "writer.classification")]
    WriterClassification {
        /// Exact issue payload emitted by classification validation.
        issue: WriterClassificationIssue,
    },
    /// Finding-to-evidence traceability is incomplete or inconsistent.
    #[serde(rename = "evidence.traceability")]
    EvidenceTraceability {
        /// Closed traceability failure.
        issue: EvidenceTraceabilityIssue,
        /// Number of affected rows or identities.
        count: u64,
    },
    /// A required analyzer family is unavailable.
    #[serde(rename = "engine.rule_unavailable")]
    RuleFamilyUnavailable {
        /// Closed required analyzer family.
        rule: FindingRuleFamily,
    },
}

impl RepositoryFinding {
    pub(super) const fn code(&self) -> FindingCode {
        match self {
            Self::PolicyInputMissing { .. } => FindingCode::PolicyInputMissing,
            Self::PolicyInputUnreadable { .. } => FindingCode::PolicyInputUnreadable,
            Self::PolicyInputInvalid { .. } => FindingCode::PolicyInputInvalid,
            Self::UnknownSchemaVersion { .. } => FindingCode::UnknownSchemaVersion,
            Self::DigestMismatch { .. } => FindingCode::DigestMismatch,
            Self::SymlinkEntry => FindingCode::SymlinkEntry,
            Self::UnsupportedEntry { .. } => FindingCode::UnsupportedEntry,
            Self::InvalidCargoManifest { .. } => FindingCode::InvalidCargoManifest,
            Self::InvalidCargoTarget { .. } => FindingCode::InvalidCargoTarget,
            Self::UnclassifiedRustSource => FindingCode::UnclassifiedRustSource,
            Self::ModuleResolution { .. } => FindingCode::ModuleResolution,
            Self::GeneratedInclude { .. } => FindingCode::GeneratedInclude,
            Self::ProductionLocExceeded { .. } => FindingCode::ProductionLocExceeded,
            Self::ModuleShape { .. } => FindingCode::ModuleShape,
            Self::ProductionHiddenAsTest { .. } => FindingCode::ProductionHiddenAsTest,
            Self::ProhibitedDebt { .. } => FindingCode::ProhibitedDebt,
            Self::LegacyExceptionChanged { .. } => FindingCode::LegacyExceptionChanged,
            Self::LegacyExceptionOverdue { .. } => FindingCode::LegacyExceptionOverdue,
            Self::UnknownWriterSink { .. } => FindingCode::UnknownWriterSink,
            Self::WriterClassification { .. } => FindingCode::WriterClassification,
            Self::EvidenceTraceability { .. } => FindingCode::EvidenceTraceability,
            Self::RuleFamilyUnavailable { .. } => FindingCode::RuleFamilyUnavailable,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "code", content = "fields")]
pub(super) enum ArtifactFinding {
    #[serde(rename = "evidence.redaction")]
    EvidenceRedaction { issue: EvidenceRedactionIssue },
}

impl ArtifactFinding {
    const fn code(&self) -> FindingCode {
        match self {
            Self::EvidenceRedaction { .. } => FindingCode::EvidenceRedaction,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub(super) enum FindingBody {
    Repository(RepositoryFinding),
    Artifact(ArtifactFinding),
}

impl FindingBody {
    pub(super) const fn code(&self) -> FindingCode {
        match self {
            Self::Repository(finding) => finding.code(),
            Self::Artifact(finding) => finding.code(),
        }
    }

    pub(super) const fn repository(&self) -> Option<&RepositoryFinding> {
        match self {
            Self::Repository(finding) => Some(finding),
            Self::Artifact(_) => None,
        }
    }

    pub(super) const fn evidence_redaction_issue(&self) -> Option<EvidenceRedactionIssue> {
        match self {
            Self::Repository(_) => None,
            Self::Artifact(ArtifactFinding::EvidenceRedaction { issue }) => Some(*issue),
        }
    }
}
