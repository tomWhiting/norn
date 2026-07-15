//! Closed policy, governance, and evidence issue types.

use serde::Serialize;

/// A required policy input with repository-wide authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyInput {
    /// `policy/repository.toml`.
    RepositoryPolicy,
    /// `policy/phase-lock.json`.
    PhaseLock,
    /// Computed immutable origin facts.
    OriginLedger,
    /// Reviewed legacy-exception governance.
    LegacyGovernance,
    /// Reviewed writer-family governance.
    WriterFamilies,
    /// Retained-evidence redaction registrations.
    RedactionRegistry,
    /// Canonical local-gate commands.
    GateCommands,
    /// Finding-to-evidence preregistration.
    FindingTraceability,
    /// Extracted `OpenAI` Responses protocol contract.
    OpenAiResponsesContract,
}

/// A closed structural failure while acquiring or decoding a policy input.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyInputIssue {
    /// The input could not be read from its authority source.
    Read,
    /// The input was not valid UTF-8 where text is required.
    Utf8,
    /// The input syntax was malformed.
    Syntax,
    /// An object or table repeated a field.
    DuplicateField,
    /// An object or table contained an unknown field.
    UnknownField,
    /// A required field was absent.
    MissingField,
    /// A field used an unsupported value.
    InvalidValue,
    /// The input did not match its registered authority.
    AuthorityMismatch,
    /// Related input records were internally inconsistent.
    Inconsistent,
    /// The required analyzer or input adapter was unavailable.
    Unavailable,
}

/// Immutable legacy-exception family.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyKind {
    /// A production file above its applicable LOC limit.
    ProductionLoc,
    /// One prohibited-debt multiset occurrence.
    ProhibitedDebt,
    /// A stable production item group.
    ProductionItem,
}

/// A closed legacy comparison that is not an overdue or item-hiding issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyChangeIssue {
    /// A current over-limit file has no immutable origin exception.
    NewLocException,
    /// An active legacy file changed while remaining over its limit.
    LocChanged,
    /// A persistently resolved LOC exception became active again.
    LocReactivated,
    /// A current debt occurrence has no immutable origin exception.
    NewDebtException,
    /// Production content changed under an active debt exception.
    DebtProductionChanged,
    /// A persistently resolved debt occurrence reappeared.
    DebtReactivated,
    /// Current facts resolved an exception without a reviewed transition.
    ResolutionNotRecorded,
}

/// The remediation phase associated with a finding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FindingPhase {
    /// Contract and enforcement baseline.
    P1,
    /// Authentication and configuration remediation.
    P2,
    /// Transcript and storage remediation.
    P3,
    /// Streaming and turn-signal remediation.
    P4,
    /// Retry and usage remediation.
    P5,
    /// Request-shaping remediation.
    P6,
    /// Tool-protocol remediation.
    P7,
    /// Prompt-caching remediation.
    P8,
    /// Integrated closure.
    P9,
}

/// Closed retained-evidence redaction issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRedactionIssue {
    /// A governed snapshot path has no reviewed registration.
    UnregisteredArtifact,
    /// A registered artifact is absent from the complete snapshot.
    RegisteredArtifactMissing,
    /// A retained entry is not a regular file.
    NonRegularArtifact,
    /// Actual bytes do not match the registered artifact digest.
    ArtifactDigestMismatch,
    /// Bytes are not one complete duplicate-safe JSON document.
    InvalidJson,
    /// Bytes are not complete duplicate-safe JSON Lines.
    InvalidJsonl,
    /// A JSON Lines document repeats a row or row identity.
    DuplicateJsonlRow,
    /// A retained document does not match its closed family schema.
    SchemaMismatch,
    /// A document identity differs from path authority.
    ArtifactIdentityMismatch,
    /// A document family differs from path authority.
    ArtifactFamilyMismatch,
    /// A decoded key names private or reusable material.
    ProhibitedField,
    /// A decoded key or value represents reusable turn or cache state.
    ReusableState,
    /// Raw or decoded content has a credential, identity, or prompt shape.
    DangerousShape,
    /// Raw or decoded content contains a control character.
    ControlCharacter,
    /// Raw or decoded content contains a private absolute path.
    AbsolutePath,
    /// A string is neither a fixed literal nor an authorized sentinel.
    UnregisteredString,
    /// A synthetic value differs from registered metadata.
    SyntheticMetadataMismatch,
    /// A registered synthetic or observation row is missing.
    RegisteredValueMissing,
    /// Rows are duplicated or differ from their required stable order.
    UnstableRowOrder,
    /// An observation tuple differs from its registration.
    ObservationMismatch,
    /// An observation's referenced artifact is absent.
    ReferencedArtifactMissing,
    /// An observation's referenced artifact is not a regular file.
    ReferencedArtifactNonRegular,
    /// Referenced bytes do not match the tuple and artifact digest.
    ReferencedArtifactDigestMismatch,
    /// A raw scanner offset cannot be represented safely.
    SpanUnrepresentable,
}

/// Closed finding-to-evidence traceability issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTraceabilityIssue {
    /// A source finding has no preregistration row.
    FindingMissing,
    /// A preregistration row has no source finding.
    FindingOrphaned,
    /// More than one row names the same finding.
    FindingDuplicated,
    /// A required planned evidence identity is absent.
    EvidenceMissing,
    /// More than one row names the same evidence identity.
    EvidenceDuplicated,
    /// A row differs from its source finding or phase authority.
    SourceMismatch,
}

/// Rule families that must remain available to the policy engine.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingRuleFamily {
    /// Cargo and module production reachability.
    ProductionReachability,
    /// Registered generated includes.
    GeneratedIncludes,
    /// Cfg-aware production LOC ceilings.
    ProductionLoc,
    /// Declaration-only production `mod.rs` shape.
    ModuleShape,
    /// Prohibited source, manifest, and command debt.
    ProhibitedDebt,
    /// Production-item projection and test hiding.
    ProductionProjection,
    /// Immutable origin and reviewed governance.
    OriginGovernance,
    /// Filesystem writer discovery and classification.
    WriterInventory,
    /// Retained-evidence redaction.
    EvidenceRedaction,
    /// Finding-to-evidence traceability.
    EvidenceTraceability,
}
