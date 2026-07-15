//! Deterministic construction of the closed P1 phase-lock authority.

use thiserror::Error;

use crate::baseline::{
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    P1_GOVERNANCE_ANCHOR_IDENTITY,
};
use crate::path::RepositoryPathError;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION, POLICY_SCHEMA_VERSION};
use crate::{Digest, RepositoryPath};

use super::{
    AlgorithmLock, AuthorityDigests, CampaignPhase, GateLock, GitObjectFormat, GitObjectId,
    GitObjectIdError, PhaseLock, PhaseLockError, SourceIdentity,
};

const P1_GATE_ENTRYPOINT_PATH: &str = "scripts/p1-gate";
const P1_GATE_COMMAND_MANIFEST_PATH: &str = "policy/gate-commands.json";

/// Reviewed P1 authorities whose identities are derived outside the phase lock.
///
/// The compiled generated-include and governance-anchor identities are
/// deliberately absent. A caller cannot replace either ratified identity while
/// authoring a P1 lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P1ReviewedAuthorityDigests {
    /// Normalized repository policy.
    pub repository_policy: Digest,
    /// Current human-reviewed governance metadata.
    pub governance: Digest,
    /// Current reviewed writer-resolution authority.
    pub writer_resolutions: Digest,
    /// Current reviewed writer-family authority.
    pub writer_families: Digest,
    /// Public and Codex contract manifest.
    pub contract_manifest: Digest,
    /// Complete redaction-registry authority.
    pub evidence_schemas: Digest,
    /// Source-review traceability registry.
    pub source_findings: Digest,
    /// Immutable computed origin ledger.
    pub origin: Digest,
}

/// SHA-256 identities of the exact checked-in P1 gate bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P1GateByteDigests {
    /// Hash of the exact `scripts/p1-gate` bytes.
    pub entrypoint_sha256: Digest,
    /// Hash of the exact `policy/gate-commands.json` bytes.
    pub command_manifest_sha256: Digest,
}

/// Complete caller-supplied input for deterministic P1 phase-lock authoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P1PhaseLockAuthoringInput {
    /// Reviewed authority identities that are not compiled ratifications.
    pub authorities: P1ReviewedAuthorityDigests,
    /// Exact byte hashes for the two fixed local gate paths.
    pub gate: P1GateByteDigests,
}

impl PhaseLock {
    /// Construct a P1 lock from reviewed digests and compiled ratifications.
    ///
    /// The schema, phase, base commit and tree, algorithms, generated-include
    /// identity, governance-anchor identity, and gate paths are not caller
    /// inputs. They always come from this evaluator's compiled P1 contract.
    ///
    /// # Errors
    ///
    /// Returns an error if a compiled identity or path is structurally invalid,
    /// or if the resulting document fails the same validation used by decoding.
    pub fn author_p1(input: P1PhaseLockAuthoringInput) -> Result<Self, P1PhaseLockAuthoringError> {
        let base = SourceIdentity {
            object_format: GitObjectFormat::Sha1,
            commit: GitObjectId::parse(P1_BASE_COMMIT)
                .map_err(P1PhaseLockAuthoringError::BaseCommit)?,
            tree: GitObjectId::parse(P1_BASE_TREE).map_err(P1PhaseLockAuthoringError::BaseTree)?,
        };
        let digests = AuthorityDigests {
            repository_policy: input.authorities.repository_policy,
            governance: input.authorities.governance,
            governance_anchor: P1_GOVERNANCE_ANCHOR_IDENTITY,
            writer_resolutions: input.authorities.writer_resolutions,
            writer_families: input.authorities.writer_families,
            generated_include_registry: P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
            contract_manifest: input.authorities.contract_manifest,
            evidence_schemas: input.authorities.evidence_schemas,
            source_findings: input.authorities.source_findings,
            origin: input.authorities.origin,
        };
        let gate = GateLock {
            entrypoint_path: RepositoryPath::parse(P1_GATE_ENTRYPOINT_PATH)
                .map_err(P1PhaseLockAuthoringError::GateEntrypointPath)?,
            entrypoint_sha256: input.gate.entrypoint_sha256,
            command_manifest_path: RepositoryPath::parse(P1_GATE_COMMAND_MANIFEST_PATH)
                .map_err(P1PhaseLockAuthoringError::GateCommandManifestPath)?,
            command_manifest_sha256: input.gate.command_manifest_sha256,
        };
        let lock = Self {
            schema_version: POLICY_SCHEMA_VERSION,
            active_phase: CampaignPhase::P1,
            base,
            algorithms: AlgorithmLock {
                analyzer: ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            digests,
            gate,
        };
        lock.validate_p1()
            .map_err(P1PhaseLockAuthoringError::Validation)?;
        Ok(lock)
    }

    /// Encode a validated P1 lock as deterministic pretty JSON.
    ///
    /// The result has one newline after the closing brace. Before returning,
    /// the encoder strictly decodes its bytes and verifies structural equality
    /// with the source lock.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON encoding, strict decoding, or structural
    /// round-trip verification fails.
    pub fn encode_p1_pretty(&self) -> Result<Vec<u8>, P1PhaseLockEncodingError> {
        self.validate_p1()
            .map_err(P1PhaseLockEncodingError::Validation)?;
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(P1PhaseLockEncodingError::Serialization)?;
        bytes.push(b'\n');
        let decoded = Self::decode_p1(&bytes).map_err(P1PhaseLockEncodingError::Decode)?;
        if decoded != *self {
            return Err(P1PhaseLockEncodingError::RoundTrip);
        }
        Ok(bytes)
    }
}

/// Failures constructing a P1 phase lock from compiled ratifications.
#[derive(Debug, Error)]
pub enum P1PhaseLockAuthoringError {
    /// The compiled ratified base commit is structurally invalid.
    #[error("compiled P1 base commit is invalid")]
    BaseCommit(#[source] GitObjectIdError),
    /// The compiled ratified base tree is structurally invalid.
    #[error("compiled P1 base tree is invalid")]
    BaseTree(#[source] GitObjectIdError),
    /// The compiled gate entrypoint path is structurally invalid.
    #[error("compiled P1 gate entrypoint path is invalid")]
    GateEntrypointPath(#[source] RepositoryPathError),
    /// The compiled gate command-manifest path is structurally invalid.
    #[error("compiled P1 gate command-manifest path is invalid")]
    GateCommandManifestPath(#[source] RepositoryPathError),
    /// The constructed lock does not satisfy the compiled P1 contract.
    #[error("authored P1 phase lock does not satisfy the compiled contract")]
    Validation(#[source] PhaseLockError),
}

/// Failures encoding or round-tripping a P1 phase lock.
#[derive(Debug, Error)]
pub enum P1PhaseLockEncodingError {
    /// The source lock does not satisfy the compiled P1 contract.
    #[error("P1 phase lock does not satisfy the compiled contract")]
    Validation(#[source] PhaseLockError),
    /// JSON serialization failed.
    #[error("P1 phase lock JSON serialization failed")]
    Serialization(#[source] serde_json::Error),
    /// Strict decoding of the encoded bytes failed.
    #[error("encoded P1 phase lock failed strict decoding")]
    Decode(#[source] PhaseLockError),
    /// Strict decoding produced a structurally different lock.
    #[error("encoded P1 phase lock failed structural round-trip verification")]
    RoundTrip,
}
