use serde::Serialize;
use thiserror::Error;

use crate::digest::digest_bytes;
use crate::finding::{ArtifactIdentity, ByteSpan, ByteSpanError, EvidenceRedactionIssue, Finding};
use crate::path::RepositoryPath;
use crate::snapshot::{EntryKind, OwnedSnapshot};

use super::authority::{RedactionRegistry, is_governed_path};
use super::json::validate_artifact_content;
use super::scan::{RawMatch, ScanCode, raw_violations};

const PATH_IDENTITY_DOMAIN: &[u8] = b"norn.redaction.path-identity.v1\0";

#[derive(Debug, Error)]
enum SpanError {
    #[error("raw scanner offset cannot be represented as u64")]
    OffsetUnrepresentable,
    #[error(transparent)]
    InvalidSpan(#[from] ByteSpanError),
}

impl SpanError {
    const fn code(&self) -> RedactionCode {
        match self {
            Self::OffsetUnrepresentable | Self::InvalidSpan(..) => {
                RedactionCode::SpanUnrepresentable
            }
        }
    }
}

/// Closed evidence-redaction violation code.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionCode {
    /// A governed snapshot path has no reviewed registration.
    UnregisteredArtifact,
    /// A registered artifact is absent from the complete snapshot.
    RegisteredArtifactMissing,
    /// A retained entry is a link, directory, device, or other non-file.
    NonRegularArtifact,
    /// Actual bytes do not match the registered artifact digest.
    ArtifactDigestMismatch,
    /// Bytes are not one complete duplicate-safe JSON document.
    InvalidJson,
    /// Bytes are not complete duplicate-safe JSON Lines.
    InvalidJsonl,
    /// A JSON Lines document repeats an exact row or row identity.
    DuplicateJsonlRow,
    /// A retained document does not match its closed family schema.
    SchemaMismatch,
    /// A document's artifact identity differs from path authority.
    ArtifactIdentityMismatch,
    /// A document's family differs from path authority.
    ArtifactFamilyMismatch,
    /// A decoded key names private or reusable material in this family.
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
    /// A synthetic value differs from its exact registered metadata.
    SyntheticMetadataMismatch,
    /// A registered synthetic or observation row is missing.
    RegisteredValueMissing,
    /// Rows are duplicated or differ from their required stable order.
    UnstableRowOrder,
    /// An observation tuple differs from its indivisible registration.
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

/// A non-disclosing retained-evidence violation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RedactionViolation {
    artifact: ArtifactIdentity,
    span: Option<ByteSpan>,
    code: RedactionCode,
}

impl RedactionViolation {
    pub(crate) const fn new(
        artifact: ArtifactIdentity,
        span: Option<ByteSpan>,
        code: RedactionCode,
    ) -> Self {
        Self {
            artifact,
            span,
            code,
        }
    }

    /// Return the safe path identity.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactIdentity {
        self.artifact
    }

    /// Return the raw-byte span when one is safely representable.
    #[must_use]
    pub const fn span(&self) -> Option<ByteSpan> {
        self.span
    }

    /// Return the closed failure code.
    #[must_use]
    pub const fn code(&self) -> RedactionCode {
        self.code
    }

    /// Convert this violation into the shared non-disclosing finding shape.
    #[must_use]
    pub const fn into_finding(self) -> Finding {
        Finding::evidence_redaction(self.artifact, self.span, self.code.finding_issue())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ArtifactIssue {
    pub(crate) span: Option<ByteSpan>,
    pub(crate) code: RedactionCode,
}

impl ArtifactIssue {
    pub(crate) const fn new(span: Option<ByteSpan>, code: RedactionCode) -> Self {
        Self { span, code }
    }
}

/// Validate every entry under the fixed governed roots in a complete snapshot.
#[must_use]
pub fn validate_retained_artifacts(
    registry: &RedactionRegistry,
    snapshot: &OwnedSnapshot,
) -> Vec<RedactionViolation> {
    let governed = snapshot
        .iter()
        .filter(|(path, _)| is_governed_path(path))
        .zip(0_u64..);
    let mut violations = Vec::new();
    for ((path, entry), observed_ordinal) in governed {
        let registration = registry.artifact_with_ordinal(path);
        let identity = match registration {
            Some((registry_ordinal, _)) => registered_artifact_identity(registry_ordinal, path),
            None => ArtifactIdentity::observed(observed_ordinal),
        };
        if entry.kind() != EntryKind::Regular {
            violations.push(RedactionViolation::new(
                identity,
                None,
                RedactionCode::NonRegularArtifact,
            ));
            continue;
        }
        let Some((_, registration)) = registration else {
            violations.push(RedactionViolation::new(
                identity,
                None,
                RedactionCode::UnregisteredArtifact,
            ));
            violations.extend(
                raw_violations(entry.bytes())
                    .into_iter()
                    .map(|raw| raw_violation(identity, raw)),
            );
            continue;
        };
        if digest_bytes(entry.bytes()) != registration.digest() {
            violations.push(RedactionViolation::new(
                identity,
                None,
                RedactionCode::ArtifactDigestMismatch,
            ));
        }
        violations.extend(
            validate_artifact_content(registry, registration, snapshot, entry.bytes())
                .into_iter()
                .map(|issue| RedactionViolation::new(identity, issue.span, issue.code)),
        );
    }

    for ((path, _), ordinal) in registry.artifacts().zip(0_u64..) {
        if !snapshot.contains_path(path) {
            violations.push(RedactionViolation::new(
                registered_artifact_identity(ordinal, path),
                None,
                RedactionCode::RegisteredArtifactMissing,
            ));
        }
    }
    violations.sort_unstable();
    violations.dedup();
    violations
}

pub(crate) fn raw_issue(raw: RawMatch) -> ArtifactIssue {
    match span(raw.start, raw.end) {
        Ok(value) => ArtifactIssue::new(Some(value), raw.code.into()),
        Err(error) => ArtifactIssue::new(None, error.code()),
    }
}

fn raw_violation(identity: ArtifactIdentity, raw: RawMatch) -> RedactionViolation {
    let issue = raw_issue(raw);
    RedactionViolation::new(identity, issue.span, issue.code)
}

fn registered_artifact_identity(ordinal: u64, path: &RepositoryPath) -> ArtifactIdentity {
    let mut bytes = Vec::from(PATH_IDENTITY_DOMAIN);
    bytes.extend_from_slice(path.as_str().as_bytes());
    ArtifactIdentity::registered(ordinal, digest_bytes(&bytes))
}

impl RedactionCode {
    const fn finding_issue(self) -> EvidenceRedactionIssue {
        match self {
            Self::UnregisteredArtifact => EvidenceRedactionIssue::UnregisteredArtifact,
            Self::RegisteredArtifactMissing => EvidenceRedactionIssue::RegisteredArtifactMissing,
            Self::NonRegularArtifact => EvidenceRedactionIssue::NonRegularArtifact,
            Self::ArtifactDigestMismatch => EvidenceRedactionIssue::ArtifactDigestMismatch,
            Self::InvalidJson => EvidenceRedactionIssue::InvalidJson,
            Self::InvalidJsonl => EvidenceRedactionIssue::InvalidJsonl,
            Self::DuplicateJsonlRow => EvidenceRedactionIssue::DuplicateJsonlRow,
            Self::SchemaMismatch => EvidenceRedactionIssue::SchemaMismatch,
            Self::ArtifactIdentityMismatch => EvidenceRedactionIssue::ArtifactIdentityMismatch,
            Self::ArtifactFamilyMismatch => EvidenceRedactionIssue::ArtifactFamilyMismatch,
            Self::ProhibitedField => EvidenceRedactionIssue::ProhibitedField,
            Self::ReusableState => EvidenceRedactionIssue::ReusableState,
            Self::DangerousShape => EvidenceRedactionIssue::DangerousShape,
            Self::ControlCharacter => EvidenceRedactionIssue::ControlCharacter,
            Self::AbsolutePath => EvidenceRedactionIssue::AbsolutePath,
            Self::UnregisteredString => EvidenceRedactionIssue::UnregisteredString,
            Self::SyntheticMetadataMismatch => EvidenceRedactionIssue::SyntheticMetadataMismatch,
            Self::RegisteredValueMissing => EvidenceRedactionIssue::RegisteredValueMissing,
            Self::UnstableRowOrder => EvidenceRedactionIssue::UnstableRowOrder,
            Self::ObservationMismatch => EvidenceRedactionIssue::ObservationMismatch,
            Self::ReferencedArtifactMissing => EvidenceRedactionIssue::ReferencedArtifactMissing,
            Self::ReferencedArtifactNonRegular => {
                EvidenceRedactionIssue::ReferencedArtifactNonRegular
            }
            Self::ReferencedArtifactDigestMismatch => {
                EvidenceRedactionIssue::ReferencedArtifactDigestMismatch
            }
            Self::SpanUnrepresentable => EvidenceRedactionIssue::SpanUnrepresentable,
        }
    }
}

fn span(start: usize, end: usize) -> Result<ByteSpan, SpanError> {
    let Ok(start) = u64::try_from(start) else {
        return Err(SpanError::OffsetUnrepresentable);
    };
    let Ok(end) = u64::try_from(end) else {
        return Err(SpanError::OffsetUnrepresentable);
    };
    ByteSpan::new(start, end).map_err(SpanError::from)
}

impl From<ScanCode> for RedactionCode {
    fn from(value: ScanCode) -> Self {
        match value {
            ScanCode::AbsolutePath => Self::AbsolutePath,
            ScanCode::ControlCharacter => Self::ControlCharacter,
            ScanCode::DangerousShape => Self::DangerousShape,
            ScanCode::ProhibitedField => Self::ProhibitedField,
            ScanCode::ReusableState => Self::ReusableState,
        }
    }
}
