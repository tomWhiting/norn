//! Deterministic checked-tree redaction-authority authoring.

use std::collections::{BTreeMap, BTreeSet};
use std::num::ParseIntError;
use std::str::Utf8Error;

use serde_json::Value;
use thiserror::Error;

use crate::digest::{Digest, digest_bytes};
use crate::path::{RepositoryPath, RepositoryPathError};
use crate::snapshot::{EntryKind, OwnedSnapshot};
use crate::strict_json::{StrictJsonError, decode_strict_json};

use super::authority::{RedactionRegistry, is_governed_path};
use super::gate_evidence;
use super::model::{
    ArtifactFamily, ArtifactRegistration, RegistrationError, SentinelClass, SyntheticPurpose,
    SyntheticRegistration,
};
use super::path_policy::validate_artifact_path;
use super::validate_retained_artifacts;

const ARTIFACT_ID_DOMAIN: &[u8] = b"norn.redaction.checked-tree-artifact-id.v1\0";
const SSE_METADATA_PREFIX: &str = ": norn-fixture-v1 ";
const SYNTHETIC_PREFIX: &str = "norn-synthetic-";
const CORPUS_GENERATOR_ID: &str = "p1-responses-fixture-corpus-v1";
const CORPUS_GENERATOR_PATH: &str = "docs/reviews/evidence/p1/responses_fixture_generate.py";

const CHECKED_TREE_FAMILIES: [ArtifactFamily; 4] = [
    ArtifactFamily::ProtocolFixture,
    ArtifactFamily::TraceabilityJsonl,
    ArtifactFamily::ContractSchema,
    ArtifactFamily::EvidenceToolSource,
];

const RUN_LOCAL_FAMILIES: [ArtifactFamily; 3] = [
    ArtifactFamily::GateDescriptor,
    ArtifactFamily::Distribution,
    ArtifactFamily::SanitizedLog,
];

struct ArtifactSeed {
    id: String,
    path: RepositoryPath,
    family: ArtifactFamily,
    digest: Digest,
    synthetic_values: BTreeSet<String>,
}

pub(super) fn author_checked_tree(
    snapshot: &OwnedSnapshot,
) -> Result<RedactionRegistry, RedactionAuthoringError> {
    super::tool_inventory::validate(snapshot)?;
    let packaged = gate_evidence::derive_packaged(snapshot)?;
    let mut seeds = Vec::new();
    let mut all_synthetic_values = BTreeSet::new();
    for (path, entry) in snapshot
        .iter()
        .filter(|(path, _)| is_governed_path(path) && !gate_evidence::is_packaged_gate_path(path))
    {
        if entry.kind() != EntryKind::Regular {
            return Err(RedactionAuthoringError::NonRegularCheckedTreeArtifact);
        }
        let family = checked_tree_family(path)?;
        let (id, synthetic_values) = if family == ArtifactFamily::ProtocolFixture {
            protocol_identity_and_values(path, entry.bytes())?
        } else {
            (artifact_id(path, family), BTreeSet::new())
        };
        all_synthetic_values.extend(synthetic_values.iter().cloned());
        seeds.push(ArtifactSeed {
            id,
            path: path.clone(),
            family,
            digest: digest_bytes(entry.bytes()),
            synthetic_values,
        });
    }

    if seeds.is_empty() || all_synthetic_values.is_empty() {
        return Err(RedactionAuthoringError::EmptyCheckedTreeAuthority);
    }

    let provenance = RepositoryPath::parse(CORPUS_GENERATOR_PATH)
        .map_err(RedactionAuthoringError::InvalidCompiledProvenance)?;
    let (synthetics, synthetic_ids) = author_synthetics(all_synthetic_values, &provenance)?;
    let artifacts = seeds
        .into_iter()
        .map(|seed| author_artifact(seed, &synthetic_ids))
        .collect::<Result<Vec<_>, _>>()?;
    let mut registry = RedactionRegistry::new(artifacts, synthetics)?;
    if let Some(packaged) = packaged {
        registry = registry.combined(&packaged)?;
    }
    if !validate_retained_artifacts(&registry, snapshot).is_empty() {
        return Err(RedactionAuthoringError::CheckedTreeValidation);
    }
    Ok(registry)
}

fn checked_tree_family(path: &RepositoryPath) -> Result<ArtifactFamily, RedactionAuthoringError> {
    if RUN_LOCAL_FAMILIES
        .iter()
        .any(|family| validate_artifact_path(path, *family).is_ok())
    {
        return Err(RedactionAuthoringError::RunLocalAuthorityRequired);
    }

    let mut selected = None;
    for family in CHECKED_TREE_FAMILIES {
        if validate_artifact_path(path, family).is_ok() && selected.replace(family).is_some() {
            return Err(RedactionAuthoringError::AmbiguousCheckedTreeArtifact);
        }
    }
    selected.ok_or(RedactionAuthoringError::UnclassifiedGovernedArtifact)
}

fn author_synthetics(
    values: BTreeSet<String>,
    provenance: &RepositoryPath,
) -> Result<(Vec<SyntheticRegistration>, BTreeMap<String, String>), RedactionAuthoringError> {
    let mut registrations = Vec::with_capacity(values.len());
    let mut ids = BTreeMap::new();
    for (offset, value) in values.into_iter().enumerate() {
        let ordinal = offset
            .checked_add(1)
            .ok_or(RedactionAuthoringError::SyntheticOrdinalOverflow)?;
        let id = format!("corpus-synthetic-{ordinal:03}");
        registrations.push(SyntheticRegistration::new(
            &id,
            &value,
            CORPUS_GENERATOR_ID,
            provenance.clone(),
            synthetic_purpose(&value),
            SentinelClass::NonReusableFixtureV1,
        )?);
        if ids.insert(value, id).is_some() {
            return Err(RedactionAuthoringError::SyntheticIdentityInvariant);
        }
    }
    Ok((registrations, ids))
}

fn author_artifact(
    seed: ArtifactSeed,
    synthetic_ids: &BTreeMap<String, String>,
) -> Result<ArtifactRegistration, RedactionAuthoringError> {
    let mut ids = Vec::with_capacity(seed.synthetic_values.len());
    for value in seed.synthetic_values {
        let Some(id) = synthetic_ids.get(&value) else {
            return Err(RedactionAuthoringError::SyntheticIdentityInvariant);
        };
        ids.push(id.clone());
    }
    ids.sort_unstable();
    ArtifactRegistration::new(
        seed.id,
        seed.path,
        seed.family,
        seed.digest,
        ids,
        Vec::new(),
    )
    .map_err(Into::into)
}

fn protocol_identity_and_values(
    path: &RepositoryPath,
    bytes: &[u8],
) -> Result<(String, BTreeSet<String>), RedactionAuthoringError> {
    if std::path::Path::new(path.as_str()).extension() == Some(std::ffi::OsStr::new("sse")) {
        sse_identity_and_values(bytes)
    } else {
        json_identity_and_values(bytes)
    }
}

fn json_identity_and_values(
    bytes: &[u8],
) -> Result<(String, BTreeSet<String>), RedactionAuthoringError> {
    let value =
        decode_strict_json::<Value>(bytes).map_err(RedactionAuthoringError::InvalidProtocolJson)?;
    let id = fixture_id(&value)?;
    let mut values = BTreeSet::new();
    collect_synthetic_values(&value, &mut values);
    Ok((id, values))
}

fn sse_identity_and_values(
    bytes: &[u8],
) -> Result<(String, BTreeSet<String>), RedactionAuthoringError> {
    let text = std::str::from_utf8(bytes).map_err(RedactionAuthoringError::ProtocolUtf8)?;
    let mut lines = text.lines();
    let metadata = lines
        .next()
        .and_then(|line| line.strip_prefix(SSE_METADATA_PREFIX))
        .ok_or(RedactionAuthoringError::InvalidProtocolEnvelope)?;
    let metadata = decode_strict_json::<Value>(metadata.as_bytes())
        .map_err(RedactionAuthoringError::InvalidProtocolJson)?;
    let id = fixture_id(&metadata)?;
    let mut values = BTreeSet::new();
    collect_synthetic_values(&metadata, &mut values);
    let mut saw_data = false;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix(':') {
            insert_synthetic(comment.trim(), &mut values);
        } else if let Some(value) = line.strip_prefix("event:") {
            insert_nonempty_sse_value(value, &mut values)?;
        } else if let Some(value) = line.strip_prefix("id:") {
            insert_nonempty_sse_value(value, &mut values)?;
        } else if let Some(value) = line.strip_prefix("retry:") {
            value
                .trim()
                .parse::<u64>()
                .map_err(RedactionAuthoringError::InvalidProtocolRetry)?;
        } else if let Some(value) = line.strip_prefix("data:") {
            let document = decode_strict_json::<Value>(value.trim().as_bytes())
                .map_err(RedactionAuthoringError::InvalidProtocolJson)?;
            collect_synthetic_values(&document, &mut values);
            saw_data = true;
        } else {
            return Err(RedactionAuthoringError::InvalidProtocolEnvelope);
        }
    }
    if !saw_data {
        return Err(RedactionAuthoringError::InvalidProtocolEnvelope);
    }
    Ok((id, values))
}

fn insert_nonempty_sse_value(
    value: &str,
    output: &mut BTreeSet<String>,
) -> Result<(), RedactionAuthoringError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RedactionAuthoringError::InvalidProtocolEnvelope);
    }
    insert_synthetic(value, output);
    Ok(())
}

fn fixture_id(value: &Value) -> Result<String, RedactionAuthoringError> {
    value
        .as_object()
        .and_then(|object| object.get("fixture_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(RedactionAuthoringError::InvalidProtocolEnvelope)
}

fn collect_synthetic_values(value: &Value, output: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                insert_synthetic(key, output);
                collect_synthetic_values(child, output);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_synthetic_values(child, output);
            }
        }
        Value::String(value) => insert_synthetic(value, output),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn insert_synthetic(value: &str, output: &mut BTreeSet<String>) {
    if value.starts_with(SYNTHETIC_PREFIX) {
        output.insert(value.to_owned());
    }
}

fn synthetic_purpose(value: &str) -> SyntheticPurpose {
    if value.starts_with("norn-synthetic-account-") {
        SyntheticPurpose::AccountId
    } else if value.starts_with("norn-synthetic-cache-") {
        SyntheticPurpose::CacheKey
    } else if value.starts_with("norn-synthetic-credential-") {
        SyntheticPurpose::Credential
    } else if value.starts_with("norn-synthetic-prompt-") {
        SyntheticPurpose::PromptContent
    } else if value.starts_with("norn-synthetic-state-") {
        SyntheticPurpose::TurnState
    } else {
        SyntheticPurpose::Generic
    }
}

pub(super) fn artifact_id(path: &RepositoryPath, family: ArtifactFamily) -> String {
    let mut framed = Vec::from(ARTIFACT_ID_DOMAIN);
    framed.extend_from_slice(path.as_str().as_bytes());
    format!("p1-{}-{}", family.as_str(), digest_bytes(&framed))
}

/// Failure to derive reviewed checked-tree redaction authority.
#[derive(Debug, Error)]
pub enum RedactionAuthoringError {
    /// A governed checked-tree entry was a link or another non-file type.
    #[error("checked-tree redaction artifact is not a regular file")]
    NonRegularCheckedTreeArtifact,
    /// A gate-time artifact requires a separately reviewed observation tuple.
    #[error("run-local redaction artifact requires reviewed observation authority")]
    RunLocalAuthorityRequired,
    /// A governed path did not belong to one checked-tree artifact family.
    #[error("governed checked-tree redaction artifact is unclassified")]
    UnclassifiedGovernedArtifact,
    /// A governed path matched multiple checked-tree families.
    #[error("governed checked-tree redaction artifact has ambiguous family authority")]
    AmbiguousCheckedTreeArtifact,
    /// A protocol fixture was not duplicate-safe strict JSON.
    #[error("protocol fixture JSON is invalid")]
    InvalidProtocolJson(#[source] StrictJsonError),
    /// An SSE protocol fixture was not UTF-8.
    #[error("protocol fixture stream is not UTF-8")]
    ProtocolUtf8(#[source] Utf8Error),
    /// A protocol fixture omitted its closed envelope or stream structure.
    #[error("protocol fixture envelope is invalid")]
    InvalidProtocolEnvelope,
    /// An SSE retry field was not an unsigned integer.
    #[error("protocol fixture retry field is invalid")]
    InvalidProtocolRetry(#[source] ParseIntError),
    /// Synthetic registry ordinal exceeded the representable authority shape.
    #[error("synthetic registry ordinal is not representable")]
    SyntheticOrdinalOverflow,
    /// The final checked tree omitted all artifacts or all corpus sentinels.
    #[error("checked-tree redaction authority is empty")]
    EmptyCheckedTreeAuthority,
    /// The compiled corpus-generator provenance path is invalid.
    #[error("compiled corpus-generator provenance is invalid")]
    InvalidCompiledProvenance(#[source] RepositoryPathError),
    /// One compiled evidence-tool path did not satisfy repository path rules.
    #[error("compiled evidence-tool path is invalid")]
    InvalidCompiledToolPath(#[source] RepositoryPathError),
    /// An exact preregistered evidence tool was absent from the checked tree.
    #[error("preregistered evidence-tool source is missing")]
    MissingEvidenceToolSource,
    /// The compiled inventory and discovered evidence-tool candidates differ.
    #[error("evidence-tool source inventory differs from compiled authority")]
    EvidenceToolInventoryMismatch,
    /// Run-local authoring received no retained artifacts.
    #[error("run-local redaction authority is empty")]
    EmptyRunLocalAuthority,
    /// A run-local retained entry was a link or another non-file type.
    #[error("run-local redaction artifact is not a regular file")]
    NonRegularRunLocalArtifact,
    /// A local or packaged gate run did not have one exact canonical layout.
    #[error("gate evidence layout is invalid")]
    InvalidRunLocalLayout,
    /// A gate descriptor failed checked schema or authority binding.
    #[error("gate descriptor authority is invalid")]
    InvalidGateDescriptor,
    /// A local-gate envelope was not duplicate-safe strict JSON.
    #[error("run-local redaction document is invalid")]
    InvalidRunLocalDocument(#[source] StrictJsonError),
    /// A local-gate tuple referenced a non-local or non-log observation.
    #[error("run-local observation authority is invalid")]
    InvalidRunLocalObservation,
    /// The authored local-gate registry did not validate its source snapshot.
    #[error("run-local redaction authority failed self-validation")]
    RunLocalValidation,
    /// The supplied checked registry did not validate its complete snapshot.
    #[error("checked redaction authority failed validation")]
    CheckedAuthorityValidation,
    /// Checked and run-local snapshots could not form one disjoint snapshot.
    #[error("checked and run-local snapshots overlap")]
    CombinedSnapshot,
    /// The authored checked-tree registry did not validate its source snapshot.
    #[error("checked-tree redaction authority failed self-validation")]
    CheckedTreeValidation,
    /// Internal synthetic identity construction did not preserve one-to-one rows.
    #[error("derived synthetic identity invariant failed")]
    SyntheticIdentityInvariant,
    /// Derived rows violated the closed registration contract.
    #[error("derived checked-tree redaction authority is invalid")]
    Registration(#[from] RegistrationError),
}
