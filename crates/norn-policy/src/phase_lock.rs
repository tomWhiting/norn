//! Strict, tamper-evident phase-lock document.

mod authoring;
mod authorities;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::baseline::{
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    P1_GOVERNANCE_ANCHOR_IDENTITY,
};
use crate::digest::Digest;
use crate::path::RepositoryPath;
use crate::strict_json::decode_strict_json;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION, POLICY_SCHEMA_VERSION};

pub use authoring::{
    P1GateByteDigests, P1PhaseLockAuthoringError, P1PhaseLockAuthoringInput,
    P1PhaseLockEncodingError, P1ReviewedAuthorityDigests,
};
pub use authorities::{P1AuthorityError, P1AuthorityKind, ReadyP1Authorities};

/// The active remediation campaign phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum CampaignPhase {
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

/// Git object hash algorithm declared once for a complete source identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormat {
    /// Git's original 160-bit object format.
    Sha1,
    /// Git's 256-bit object format.
    Sha256,
}

impl GitObjectFormat {
    const fn hexadecimal_length(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// A complete Git object identifier in lowercase hexadecimal form.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitObjectId(String);

impl GitObjectId {
    /// Validate a SHA-1 or SHA-256 Git object identifier.
    ///
    /// # Errors
    ///
    /// Returns a structural error for an unsupported length or non-lowercase
    /// hexadecimal byte.
    pub fn parse(value: impl Into<String>) -> Result<Self, GitObjectIdError> {
        let value = value.into();
        if !matches!(value.len(), 40 | 64) {
            return Err(GitObjectIdError::Length {
                actual: value.len(),
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(GitObjectIdError::InvalidHex);
        }
        Ok(Self(value))
    }

    /// Borrow the complete hexadecimal identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the object format implied by this complete identifier's length.
    #[must_use]
    pub fn object_format(&self) -> GitObjectFormat {
        if self.0.len() == GitObjectFormat::Sha1.hexadecimal_length() {
            GitObjectFormat::Sha1
        } else {
            GitObjectFormat::Sha256
        }
    }
}

impl fmt::Display for GitObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for GitObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("GitObjectId").field(&self.0).finish()
    }
}

impl FromStr for GitObjectId {
    type Err = GitObjectIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for GitObjectId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GitObjectId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid Git object identifier structure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GitObjectIdError {
    /// The identifier is neither a complete SHA-1 nor SHA-256 value.
    #[error("Git object identifier length {actual} is not 40 or 64")]
    Length {
        /// Observed byte length.
        actual: usize,
    },
    /// The identifier contains a byte outside lowercase hexadecimal.
    #[error("Git object identifier contains invalid lowercase hexadecimal")]
    InvalidHex,
}

/// Immutable source identity pinned by a phase lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    /// One explicit Git object format shared by commit and tree.
    pub object_format: GitObjectFormat,
    /// Commit object containing the accepted phase base.
    pub commit: GitObjectId,
    /// Exact tree object belonging to the accepted phase base.
    pub tree: GitObjectId,
}

/// Analyzer implementations frozen by a phase lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AlgorithmLock {
    /// Repository analyzer implementation identifier.
    pub analyzer: String,
    /// Canonical digest implementation identifier.
    pub digest: String,
}

/// Digests of every reviewed policy authority outside the lock itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityDigests {
    /// Normalized repository policy.
    pub repository_policy: Digest,
    /// Human-reviewed governance metadata.
    pub governance: Digest,
    /// Last independently reviewed governance used for monotonic comparison.
    pub governance_anchor: Digest,
    /// Reviewed unresolved-writer resolution authority.
    pub writer_resolutions: Digest,
    /// Reviewed writer-family classification authority.
    pub writer_families: Digest,
    /// Exact generated-include technical registry.
    pub generated_include_registry: Digest,
    /// Public and Codex contract manifest.
    pub contract_manifest: Digest,
    /// Complete redaction-registry authority. Its digest includes the closed
    /// evidence-schema digest, registered artifacts, and synthetic sentinels.
    pub evidence_schemas: Digest,
    /// Source-review traceability registry.
    pub source_findings: Digest,
    /// Immutable computed origin ledger.
    pub origin: Digest,
}

/// Checked-in local gate authority pinned by a phase lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GateLock {
    /// Repository-relative gate entrypoint.
    pub entrypoint_path: RepositoryPath,
    /// Hash of the exact gate entrypoint bytes.
    pub entrypoint_sha256: Digest,
    /// Repository-relative fixed command manifest.
    pub command_manifest_path: RepositoryPath,
    /// Hash of the exact command manifest bytes.
    pub command_manifest_sha256: Digest,
}

/// Complete strict P1 phase-lock schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PhaseLock {
    /// Closed lock schema version.
    schema_version: u32,
    /// Active campaign phase.
    active_phase: CampaignPhase,
    /// Accepted immutable base identity.
    base: SourceIdentity,
    /// Frozen algorithm implementations.
    algorithms: AlgorithmLock,
    /// Reviewed authority digests.
    digests: AuthorityDigests,
    /// Local gate authority.
    gate: GateLock,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhaseLockDocument {
    schema_version: u32,
    active_phase: CampaignPhase,
    base: SourceIdentity,
    algorithms: AlgorithmLock,
    digests: AuthorityDigests,
    gate: GateLock,
}

impl PhaseLock {
    /// Decode and validate the P1 phase lock.
    ///
    /// # Errors
    ///
    /// Rejects duplicate or unknown fields, unsupported versions/phases, and
    /// algorithm identifiers that do not match this evaluator.
    pub fn decode_p1(bytes: &[u8]) -> Result<Self, PhaseLockError> {
        let Ok(document): Result<PhaseLockDocument, _> = decode_strict_json(bytes) else {
            return Err(PhaseLockError::Json);
        };
        let lock = Self {
            schema_version: document.schema_version,
            active_phase: document.active_phase,
            base: document.base,
            algorithms: document.algorithms,
            digests: document.digests,
            gate: document.gate,
        };
        lock.validate_p1()?;
        Ok(lock)
    }

    /// Return the closed lock schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the active campaign phase.
    #[must_use]
    pub const fn active_phase(&self) -> CampaignPhase {
        self.active_phase
    }

    /// Borrow the format-bound base commit and tree.
    #[must_use]
    pub const fn base(&self) -> &SourceIdentity {
        &self.base
    }

    /// Borrow the frozen algorithm identities.
    #[must_use]
    pub const fn algorithms(&self) -> &AlgorithmLock {
        &self.algorithms
    }

    /// Borrow every reviewed authority digest.
    #[must_use]
    pub const fn digests(&self) -> &AuthorityDigests {
        &self.digests
    }

    /// Borrow the checked-in local gate authority.
    #[must_use]
    pub const fn gate(&self) -> &GateLock {
        &self.gate
    }

    fn validate_p1(&self) -> Result<(), PhaseLockError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PhaseLockError::SchemaVersion {
                actual: self.schema_version,
            });
        }
        if self.active_phase != CampaignPhase::P1 {
            return Err(PhaseLockError::ActivePhase {
                actual: self.active_phase,
            });
        }
        if self.algorithms.analyzer != ANALYZER_VERSION {
            return Err(PhaseLockError::AnalyzerVersion);
        }
        if self.algorithms.digest != DIGEST_VERSION {
            return Err(PhaseLockError::DigestVersion);
        }
        if self.base.commit.object_format() != self.base.object_format
            || self.base.tree.object_format() != self.base.object_format
        {
            return Err(PhaseLockError::GitObjectFormat);
        }
        if self.base.object_format != GitObjectFormat::Sha1 {
            return Err(PhaseLockError::P1BaseObjectFormat);
        }
        if self.base.commit.as_str() != P1_BASE_COMMIT {
            return Err(PhaseLockError::P1BaseCommit);
        }
        if self.base.tree.as_str() != P1_BASE_TREE {
            return Err(PhaseLockError::P1BaseTree);
        }
        if self.digests.generated_include_registry != P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY {
            return Err(PhaseLockError::P1GeneratedIncludeRegistry);
        }
        if self.digests.governance_anchor != P1_GOVERNANCE_ANCHOR_IDENTITY {
            return Err(PhaseLockError::P1GovernanceAnchor);
        }
        if self.gate.entrypoint_path.as_str() != "scripts/p1-gate" {
            return Err(PhaseLockError::P1GateEntrypointPath);
        }
        if self.gate.command_manifest_path.as_str() != "policy/gate-commands.json" {
            return Err(PhaseLockError::P1GateCommandManifestPath);
        }
        Ok(())
    }
}

/// Strict P1 phase-lock failures.
#[derive(Debug, Error)]
pub enum PhaseLockError {
    /// JSON was ambiguous, malformed, or outside the closed schema.
    #[error("phase lock is not valid strict JSON")]
    Json,
    /// The lock schema is not implemented by this evaluator.
    #[error("phase lock schema version {actual} is unsupported")]
    SchemaVersion {
        /// Observed schema version.
        actual: u32,
    },
    /// The P1 evaluator was asked to accept another phase.
    #[error("phase lock active phase {actual:?} is not P1")]
    ActivePhase {
        /// Observed campaign phase.
        actual: CampaignPhase,
    },
    /// The analyzer implementation does not match the compiled evaluator.
    #[error("phase lock analyzer version does not match this evaluator")]
    AnalyzerVersion,
    /// The digest implementation does not match the compiled evaluator.
    #[error("phase lock digest version does not match this evaluator")]
    DigestVersion,
    /// The commit or tree does not match the one declared Git object format.
    #[error("phase lock commit and tree must match the declared Git object format")]
    GitObjectFormat,
    /// P1 was ratified against a SHA-1 repository and accepts no other format.
    #[error("P1 phase lock base must use the ratified SHA-1 object format")]
    P1BaseObjectFormat,
    /// The lock names a commit other than the ratified P1 base commit.
    #[error("phase lock commit does not match the ratified P1 base commit")]
    P1BaseCommit,
    /// The lock names a tree other than the ratified P1 base tree.
    #[error("phase lock tree does not match the ratified P1 base tree")]
    P1BaseTree,
    /// The lock names a generated-include registry other than the exact P1 authority.
    #[error("phase lock generated-include registry does not match the exact P1 authority")]
    P1GeneratedIncludeRegistry,
    /// The lock names an anchor other than the compiled reviewed governance.
    #[error("phase lock governance anchor does not match the reviewed P1 authority")]
    P1GovernanceAnchor,
    /// The P1 gate entrypoint path differs from the ratified normalized path.
    #[error("phase lock P1 gate entrypoint path is not scripts/p1-gate")]
    P1GateEntrypointPath,
    /// The P1 command manifest path differs from the ratified normalized path.
    #[error("phase lock P1 command manifest path is not policy/gate-commands.json")]
    P1GateCommandManifestPath,
}
