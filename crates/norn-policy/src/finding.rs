//! Stable policy findings containing no source snippets or rendered prose.

use std::cmp::Ordering;
use std::fmt;

use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::version::ANALYZER_VERSION;

mod details;
mod location;
mod policy;
mod rust;
mod writer;

pub use details::RepositoryFinding;
pub use location::{ArtifactIdentity, FindingLocation};
pub use policy::{
    EvidenceRedactionIssue, EvidenceTraceabilityIssue, FindingPhase, FindingRuleFamily,
    LegacyChangeIssue, LegacyKind, PolicyInput, PolicyInputIssue,
};
pub use rust::{
    CargoManifestIssue, CargoTargetIssue, CargoTargetKind, DebtConstructKind, DebtTargetKind,
    GeneratedIncludeIssue, ModuleResolutionIssue, ModuleShapeIssue, UnsupportedEntryKind,
};
pub use writer::{UnknownWriterIssue, WriterClassificationIssue};

use crate::path::RepositoryPath;
use details::{ArtifactFinding, FindingBody};

/// A stable closed policy-finding code.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FindingCode {
    /// A required policy input is absent.
    PolicyInputMissing,
    /// A required policy input could not be acquired.
    PolicyInputUnreadable,
    /// A required policy input is structurally invalid.
    PolicyInputInvalid,
    /// A policy document uses an unsupported schema version.
    UnknownSchemaVersion,
    /// A pinned digest does not match computed content.
    DigestMismatch,
    /// A snapshot entry is a symbolic link.
    SymlinkEntry,
    /// A snapshot entry is not a supported ordinary file.
    UnsupportedEntry,
    /// A Cargo manifest cannot be analyzed strictly.
    InvalidCargoManifest,
    /// A Cargo target cannot be classified strictly.
    InvalidCargoTarget,
    /// A Rust source file is not classified by a production or test target.
    UnclassifiedRustSource,
    /// Rust module resolution is missing, ambiguous, cyclic, or outside authority.
    ModuleResolution,
    /// A generated include is absent, changed, or unregistered.
    GeneratedInclude,
    /// A production Rust file exceeds its applicable limit.
    ProductionLocExceeded,
    /// A production `mod.rs` contains a prohibited top-level form.
    ModuleShape,
    /// An origin production item was moved behind a test-only predicate.
    ProductionHiddenAsTest,
    /// A prohibited source, manifest, or command construct is active.
    ProhibitedDebt,
    /// An active legacy exception differs from its immutable origin.
    LegacyExceptionChanged,
    /// An active legacy exception reached or passed its due phase.
    LegacyExceptionOverdue,
    /// A possible writer operation cannot be resolved to a registered sink.
    UnknownWriterSink,
    /// A writer operation has an invalid classification row.
    WriterClassification,
    /// A retained artifact violates its closed redaction schema.
    EvidenceRedaction,
    /// Finding-to-evidence traceability is incomplete or inconsistent.
    EvidenceTraceability,
    /// A required analyzer family is unavailable.
    RuleFamilyUnavailable,
}

impl FindingCode {
    /// Return the stable machine-facing code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyInputMissing => "policy.input_missing",
            Self::PolicyInputUnreadable => "policy.input_unreadable",
            Self::PolicyInputInvalid => "policy.input_invalid",
            Self::UnknownSchemaVersion => "policy.schema_unknown",
            Self::DigestMismatch => "policy.digest_mismatch",
            Self::SymlinkEntry => "snapshot.symlink",
            Self::UnsupportedEntry => "snapshot.unsupported_entry",
            Self::InvalidCargoManifest => "rust.manifest_invalid",
            Self::InvalidCargoTarget => "rust.target_invalid",
            Self::UnclassifiedRustSource => "rust.source_unclassified",
            Self::ModuleResolution => "rust.module_resolution",
            Self::GeneratedInclude => "rust.generated_include",
            Self::ProductionLocExceeded => "rust.loc_exceeded",
            Self::ModuleShape => "rust.module_shape",
            Self::ProductionHiddenAsTest => "rust.production_hidden_as_test",
            Self::ProhibitedDebt => "debt.prohibited",
            Self::LegacyExceptionChanged => "baseline.legacy_changed",
            Self::LegacyExceptionOverdue => "baseline.legacy_overdue",
            Self::UnknownWriterSink => "writer.unknown_sink",
            Self::WriterClassification => "writer.classification",
            Self::EvidenceRedaction => "evidence.redaction",
            Self::EvidenceTraceability => "evidence.traceability",
            Self::RuleFamilyUnavailable => "engine.rule_unavailable",
        }
    }
}

impl fmt::Display for FindingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for FindingCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// A half-open byte range in one repository file or retained artifact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ByteSpan {
    start: u64,
    end: u64,
}

impl ByteSpan {
    /// Construct a half-open byte range.
    ///
    /// # Errors
    ///
    /// Returns [`ByteSpanError`] when `end` precedes `start`.
    pub const fn new(start: u64, end: u64) -> Result<Self, ByteSpanError> {
        if end < start {
            return Err(ByteSpanError { start, end });
        }
        Ok(Self { start, end })
    }

    /// Return the inclusive start byte offset.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Return the exclusive end byte offset.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }
}

/// A reversed byte span.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("byte span end {end} precedes start {start}")]
pub struct ByteSpanError {
    start: u64,
    end: u64,
}

/// One stable, deterministic hard policy finding.
///
/// Repository findings disclose a validated repository path. Evidence-redaction
/// findings instead carry an ordinal and, only for preregistered paths, a
/// reviewed domain-separated path digest. The two constructors make that
/// disclosure choice explicit and prevent a redaction issue from accidentally
/// being attached to a rendered path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Finding {
    #[serde(flatten)]
    body: FindingBody,
    location: FindingLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<ByteSpan>,
    algorithm_version: &'static str,
}

impl Finding {
    /// Construct a finding whose repository path is safe to disclose.
    #[must_use]
    pub const fn repository(
        path: RepositoryPath,
        span: Option<ByteSpan>,
        finding: RepositoryFinding,
    ) -> Self {
        Self {
            body: FindingBody::Repository(finding),
            location: FindingLocation::Repository { path },
            span,
            algorithm_version: ANALYZER_VERSION,
        }
    }

    /// Construct a non-disclosing retained-evidence finding.
    #[must_use]
    pub const fn evidence_redaction(
        artifact: ArtifactIdentity,
        span: Option<ByteSpan>,
        issue: EvidenceRedactionIssue,
    ) -> Self {
        Self {
            body: FindingBody::Artifact(ArtifactFinding::EvidenceRedaction { issue }),
            location: FindingLocation::Artifact { artifact },
            span,
            algorithm_version: ANALYZER_VERSION,
        }
    }

    /// Return the stable finding code implied by the typed finding variant.
    #[must_use]
    pub const fn code(&self) -> FindingCode {
        self.body.code()
    }

    /// Return the disclosed or non-disclosing location.
    #[must_use]
    pub const fn location(&self) -> &FindingLocation {
        &self.location
    }

    /// Return the repository path when this finding deliberately discloses one.
    #[must_use]
    pub const fn path(&self) -> Option<&RepositoryPath> {
        self.location.path()
    }

    /// Return the safe artifact identity for a non-disclosing finding.
    #[must_use]
    pub const fn artifact(&self) -> Option<ArtifactIdentity> {
        self.location.artifact()
    }

    /// Return typed repository details, if this finding discloses a path.
    #[must_use]
    pub const fn repository_details(&self) -> Option<&RepositoryFinding> {
        self.body.repository()
    }

    /// Return the closed redaction issue for a non-disclosing artifact finding.
    #[must_use]
    pub const fn evidence_redaction_issue(&self) -> Option<EvidenceRedactionIssue> {
        self.body.evidence_redaction_issue()
    }

    /// Return the optional half-open byte span.
    #[must_use]
    pub const fn span(&self) -> Option<ByteSpan> {
        self.span
    }

    /// Return the frozen analyzer version.
    #[must_use]
    pub const fn algorithm_version(&self) -> &'static str {
        self.algorithm_version
    }
}

impl Ord for Finding {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            &self.location,
            self.span,
            self.code(),
            &self.body,
            self.algorithm_version,
        )
            .cmp(&(
                &other.location,
                other.span,
                other.code(),
                &other.body,
                other.algorithm_version,
            ))
    }
}

impl PartialOrd for Finding {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
