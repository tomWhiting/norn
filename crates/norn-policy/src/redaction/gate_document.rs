//! Strict decoding of the retained P1 gate descriptor and its checked inputs.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::strict_json::{StrictJsonError, decode_strict_json};
use crate::{Digest, EntryKind, OwnedSnapshot, RepositoryPath, digest_bytes};

pub(super) const GATE_DESCRIPTOR_NAME: &str = "descriptor.json";
pub(super) const GATE_ENTRYPOINT_PATH: &str = "scripts/p1-gate";
pub(super) const GATE_MANIFEST_PATH: &str = "policy/gate-commands.json";
pub(super) const GATE_SCHEMA_PATH: &str = "policy/evidence-schemas/gate-run.schema.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct GatePin {
    pub(super) id: String,
    pub(super) path: RepositoryPath,
    pub(super) sha256: Digest,
    pub(super) status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct GateLogRecord {
    pub(super) path: String,
    pub(super) bytes: usize,
    pub(super) sha256: Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct DistributionCounts {
    pub(super) observations: u64,
    pub(super) passed: u64,
    pub(super) failed: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct GateCommand {
    pub(super) order: usize,
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) argv: Vec<String>,
    pub(super) process_outcome: String,
    pub(super) outcome: String,
    pub(super) exit_code: Option<i64>,
    pub(super) test_executions: u64,
    pub(super) distribution: Option<DistributionCounts>,
    pub(super) stdout: GateLogRecord,
    pub(super) stderr: GateLogRecord,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct GitIdentity {
    commit: String,
    tree: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct GateRecord {
    entrypoint_path: RepositoryPath,
    entrypoint_sha256: Digest,
    command_manifest_path: RepositoryPath,
    command_manifest_sha256: Digest,
    pinned_files: Vec<GatePin>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct GateRunDocument {
    evidence_id: String,
    outcome: String,
    candidate: GitIdentity,
    gate: GateRecord,
    commands: Vec<GateCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ManifestImplementation {
    files: Vec<GatePin>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ManifestCommand {
    id: String,
    kind: String,
    argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct GateManifest {
    implementation: ManifestImplementation,
    commands: Vec<ManifestCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DecodedGateRun {
    pub(super) evidence_id: String,
    pub(super) candidate_commit: String,
    pub(super) candidate_tree: String,
    pub(super) outcome: String,
    pub(super) commands: Vec<GateCommand>,
}

pub(super) fn decode_gate_run(
    checked: &OwnedSnapshot,
    bytes: &[u8],
) -> Result<DecodedGateRun, GateDocumentError> {
    let schema_bytes = required_regular_bytes(checked, GATE_SCHEMA_PATH)?;
    let schema = decode_strict_json::<Value>(schema_bytes).map_err(GateDocumentError::Json)?;
    let document = decode_strict_json::<Value>(bytes).map_err(GateDocumentError::Json)?;
    let validator =
        jsonschema::validator_for(&schema).map_err(|_| GateDocumentError::InvalidCheckedSchema)?;
    if !validator.is_valid(&document) {
        return Err(GateDocumentError::SchemaMismatch);
    }
    let gate = serde_json::from_value::<GateRunDocument>(document)
        .map_err(GateDocumentError::TypedDocument)?;
    validate_checked_bindings(checked, &gate)?;
    Ok(DecodedGateRun {
        evidence_id: gate.evidence_id,
        candidate_commit: gate.candidate.commit,
        candidate_tree: gate.candidate.tree,
        outcome: gate.outcome,
        commands: gate.commands,
    })
}

fn validate_checked_bindings(
    checked: &OwnedSnapshot,
    gate: &GateRunDocument,
) -> Result<(), GateDocumentError> {
    if gate.gate.entrypoint_path.as_str() != GATE_ENTRYPOINT_PATH
        || gate.gate.command_manifest_path.as_str() != GATE_MANIFEST_PATH
    {
        return Err(GateDocumentError::CheckedBindingMismatch);
    }
    let entrypoint = required_regular_bytes(checked, GATE_ENTRYPOINT_PATH)?;
    let manifest_bytes = required_regular_bytes(checked, GATE_MANIFEST_PATH)?;
    if digest_bytes(entrypoint) != gate.gate.entrypoint_sha256
        || digest_bytes(manifest_bytes) != gate.gate.command_manifest_sha256
    {
        return Err(GateDocumentError::CheckedBindingMismatch);
    }
    let manifest =
        decode_strict_json::<GateManifest>(manifest_bytes).map_err(GateDocumentError::Json)?;
    if manifest.implementation.files != gate.gate.pinned_files
        || gate.commands.len() > manifest.commands.len()
    {
        return Err(GateDocumentError::CheckedBindingMismatch);
    }
    for pin in &manifest.implementation.files {
        if super::path_policy::validate_machine_id(&pin.id).is_err()
            || !matches!(pin.status.as_str(), "active" | "pending_fail_closed")
            || digest_bytes(required_regular_bytes(checked, pin.path.as_str())?) != pin.sha256
        {
            return Err(GateDocumentError::CheckedBindingMismatch);
        }
    }
    for (observed, expected) in gate.commands.iter().zip(&manifest.commands) {
        if observed.id != expected.id
            || observed.kind != expected.kind
            || observed.argv != expected.argv
        {
            return Err(GateDocumentError::CheckedBindingMismatch);
        }
    }
    Ok(())
}

fn required_regular_bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    raw_path: &str,
) -> Result<&'a [u8], GateDocumentError> {
    let path =
        RepositoryPath::parse(raw_path).map_err(|_| GateDocumentError::MissingCheckedAuthority)?;
    let Some(entry) = snapshot.get(&path) else {
        return Err(GateDocumentError::MissingCheckedAuthority);
    };
    if entry.kind() != EntryKind::Regular {
        return Err(GateDocumentError::MissingCheckedAuthority);
    }
    Ok(entry.bytes())
}

#[derive(Debug, Error)]
pub(super) enum GateDocumentError {
    #[error("required checked gate authority is unavailable")]
    MissingCheckedAuthority,
    #[error("checked gate JSON is invalid")]
    Json(#[source] StrictJsonError),
    #[error("checked gate schema cannot be compiled")]
    InvalidCheckedSchema,
    #[error("gate descriptor does not satisfy the checked schema")]
    SchemaMismatch,
    #[error("gate descriptor does not satisfy its typed projection")]
    TypedDocument(#[source] serde_json::Error),
    #[error("gate descriptor differs from checked gate authority")]
    CheckedBindingMismatch,
}
