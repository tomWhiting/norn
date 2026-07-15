use std::error::Error;
use std::io;

use norn_policy::redaction::{
    ArtifactFamily, ArtifactRegistration, ObservationRegistration, ObservationSource,
    RedactionCode, RedactionRegistry, RedactionViolation, SentinelClass, SyntheticPurpose,
    SyntheticRegistration, validate_retained_artifacts,
};
use norn_policy::{Digest, OwnedSnapshot, RepositoryPath, SnapshotEntry, digest_bytes};
use serde_json::{Value, json};

const SYNTHETIC_IDS: [&str; 6] = [
    "account-value",
    "cache-value",
    "credential-value",
    "generic-value",
    "prompt-value",
    "state-value",
];
const TRACEABILITY_BYTES: &[u8] =
    include_bytes!("../../../../docs/reviews/evidence/p1/finding-traceability.jsonl");
const CONTRACT_BYTES: &[u8] =
    include_bytes!("../../../../policy/contracts/openai-responses-v1/manifest.json");

#[derive(Clone)]
pub(super) struct Paths {
    pub(super) protocol: RepositoryPath,
    pub(super) traceability: RepositoryPath,
    pub(super) tool: RepositoryPath,
    pub(super) contract: RepositoryPath,
    pub(super) distribution: RepositoryPath,
    pub(super) log: RepositoryPath,
}

pub(super) struct Fixture {
    pub(super) registry: RedactionRegistry,
    pub(super) snapshot: OwnedSnapshot,
    pub(super) paths: Paths,
}

pub(super) fn fixture() -> Result<Fixture, Box<dyn Error>> {
    let paths = paths()?;
    let synthetics = synthetics()?;
    let log = b"tests=20 passed=20 failed=0\n".to_vec();
    let log_digest = digest_bytes(&log);
    let protocol = protocol_bytes()?;
    let traceability = TRACEABILITY_BYTES.to_vec();
    let tool = b"print('p1 evidence')\n".to_vec();
    let contract = CONTRACT_BYTES.to_vec();

    let distribution_observation = ObservationRegistration::new(
        "distribution-observation",
        paths.log.clone(),
        ArtifactFamily::SanitizedLog,
        ObservationSource::LocalGate,
        synthetic_ids(),
        log_digest,
    )?;
    let distribution = evidence_document(
        "p1-distribution-001",
        ArtifactFamily::Distribution,
        "distribution-observation",
        ObservationSource::LocalGate,
        log_digest,
    )?;

    let artifacts = vec![
        ArtifactRegistration::new(
            "p1-protocol-001",
            paths.protocol.clone(),
            ArtifactFamily::ProtocolFixture,
            digest_bytes(&protocol),
            synthetic_ids(),
            Vec::new(),
        )?,
        ArtifactRegistration::new(
            "p1-traceability-001",
            paths.traceability.clone(),
            ArtifactFamily::TraceabilityJsonl,
            digest_bytes(&traceability),
            Vec::new(),
            Vec::new(),
        )?,
        ArtifactRegistration::new(
            "p1-tool-001",
            paths.tool.clone(),
            ArtifactFamily::EvidenceToolSource,
            digest_bytes(&tool),
            Vec::new(),
            Vec::new(),
        )?,
        ArtifactRegistration::new(
            "p1-contract-001",
            paths.contract.clone(),
            ArtifactFamily::ContractSchema,
            digest_bytes(&contract),
            Vec::new(),
            Vec::new(),
        )?,
        ArtifactRegistration::new(
            "p1-distribution-001",
            paths.distribution.clone(),
            ArtifactFamily::Distribution,
            digest_bytes(&distribution),
            synthetic_ids(),
            vec![distribution_observation],
        )?,
        ArtifactRegistration::new(
            "p1-log-001",
            paths.log.clone(),
            ArtifactFamily::SanitizedLog,
            log_digest,
            Vec::new(),
            Vec::new(),
        )?,
    ];
    let registry = RedactionRegistry::new(artifacts, synthetics)?;
    let snapshot = OwnedSnapshot::try_from_entries([
        (paths.protocol.clone(), SnapshotEntry::regular(protocol)),
        (
            paths.traceability.clone(),
            SnapshotEntry::regular(traceability),
        ),
        (paths.tool.clone(), SnapshotEntry::regular(tool)),
        (paths.contract.clone(), SnapshotEntry::regular(contract)),
        (
            paths.distribution.clone(),
            SnapshotEntry::regular(distribution),
        ),
        (paths.log.clone(), SnapshotEntry::regular(log)),
    ])?;
    Ok(Fixture {
        registry,
        snapshot,
        paths,
    })
}

pub(super) fn codes(registry: &RedactionRegistry, snapshot: &OwnedSnapshot) -> Vec<RedactionCode> {
    let violations = validate_retained_artifacts(registry, snapshot);
    violations.iter().map(RedactionViolation::code).collect()
}

pub(super) fn assert_has(codes: &[RedactionCode], expected: RedactionCode) {
    assert!(
        codes.contains(&expected),
        "missing {expected:?} in {codes:?}"
    );
}

pub(super) fn bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    path: &RepositoryPath,
) -> Result<&'a [u8], io::Error> {
    snapshot
        .get(path)
        .map(SnapshotEntry::bytes)
        .ok_or_else(|| io::Error::other("fixture entry missing"))
}

pub(super) fn replace(
    snapshot: &OwnedSnapshot,
    path: &RepositoryPath,
    replacement: SnapshotEntry,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let mut replacement = Some(replacement);
    let entries = snapshot.iter().map(|(candidate, entry)| {
        let selected = if candidate == path {
            match replacement.take() {
                Some(replacement) => replacement,
                None => entry.clone(),
            }
        } else {
            entry.clone()
        };
        (candidate.clone(), selected)
    });
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn remove(
    snapshot: &OwnedSnapshot,
    path: &RepositoryPath,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = snapshot
        .iter()
        .filter(|(candidate, _)| *candidate != path)
        .map(|(candidate, entry)| (candidate.clone(), entry.clone()));
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn add(
    snapshot: &OwnedSnapshot,
    path: RepositoryPath,
    entry: SnapshotEntry,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = snapshot
        .iter()
        .map(|(candidate, current)| (candidate.clone(), current.clone()))
        .chain([(path, entry)]);
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn synthetic_ids() -> Vec<String> {
    SYNTHETIC_IDS.iter().map(|id| (*id).to_owned()).collect()
}

pub(super) fn protocol_bytes() -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&json!({
        "artifact_family": ArtifactFamily::ProtocolFixture,
        "artifact_kind": "request",
        "dialect": "public",
        "fixture_id": "p1-protocol-001",
        "payload": {
            "access_token": "norn-synthetic-credential-001",
            "account_id": "norn-synthetic-account-001",
            "content": [{
                "text": "norn-synthetic-prompt-001",
                "type": "input_text"
            }],
            "include": [
                "file_search_call.results",
                "web_search_call.results",
                "web_search_call.action.sources",
                "message.input_image.image_url",
                "computer_call_output.output.image_url",
                "code_interpreter_call.outputs",
                "reasoning.encrypted_content",
                "message.output_text.logprobs"
            ],
            "output": "norn-synthetic-generic-001",
            "previous_response_id": "norn-synthetic-state-001",
            "prompt_cache_key": "norn-synthetic-cache-001",
            "prompt_cache_options": {
                "retention": "in_memory",
                "ttl": 300
            },
            "refusal": null,
            "tools": [{
                "name": "norn-synthetic-generic-001",
                "parameters": {
                    "$defs": {
                        "norn-synthetic-generic-001": {
                            "const": "norn-synthetic-generic-001",
                            "default": 1,
                            "type": "string"
                        }
                    },
                    "additionalProperties": false,
                    "properties": {
                        "norn-synthetic-generic-001": {
                            "$ref": "#/$defs/norn-synthetic-generic-001"
                        }
                    },
                    "required": ["norn-synthetic-generic-001"],
                    "type": "object"
                },
                "type": "function"
            }],
            "type": "response.completed",
            "url": "https://citation.example.invalid/source"
        },
        "schema_version": 1
    }))
}

fn paths() -> Result<Paths, Box<dyn Error>> {
    Ok(Paths {
        protocol: RepositoryPath::parse(
            "crates/norn/testdata/openai_responses/public/requests/request.json",
        )?,
        traceability: RepositoryPath::parse("docs/reviews/evidence/p1/finding-traceability.jsonl")?,
        tool: RepositoryPath::parse("docs/reviews/evidence/p1/openai_contract_constants.py")?,
        contract: RepositoryPath::parse("policy/contracts/openai-responses-v1/manifest.json")?,
        distribution: RepositoryPath::parse("target/p1-gate/evidence/distribution.json")?,
        log: RepositoryPath::parse("target/p1-gate/evidence/run.log")?,
    })
}

fn synthetics() -> Result<Vec<SyntheticRegistration>, Box<dyn Error>> {
    let provenance = RepositoryPath::parse("crates/norn-policy/tests/redaction/support.rs")?;
    let rows = [
        (
            "account-value",
            "norn-synthetic-account-001",
            SyntheticPurpose::AccountId,
        ),
        (
            "cache-value",
            "norn-synthetic-cache-001",
            SyntheticPurpose::CacheKey,
        ),
        (
            "credential-value",
            "norn-synthetic-credential-001",
            SyntheticPurpose::Credential,
        ),
        (
            "generic-value",
            "norn-synthetic-generic-001",
            SyntheticPurpose::Generic,
        ),
        (
            "prompt-value",
            "norn-synthetic-prompt-001",
            SyntheticPurpose::PromptContent,
        ),
        (
            "state-value",
            "norn-synthetic-state-001",
            SyntheticPurpose::TurnState,
        ),
    ];
    rows.into_iter()
        .map(|(id, value, purpose)| {
            SyntheticRegistration::new(
                id,
                value,
                "p1-fixture-generator-v1",
                provenance.clone(),
                purpose,
                SentinelClass::NonReusableFixtureV1,
            )
            .map_err(Into::into)
        })
        .collect()
}

fn evidence_document(
    artifact_id: &str,
    family: ArtifactFamily,
    observation_id: &str,
    source: ObservationSource,
    log_digest: Digest,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&json!({
        "artifact_family": family,
        "artifact_id": artifact_id,
        "observations": [{
            "digest": log_digest,
            "id": observation_id,
            "source": source,
            "referenced_family": ArtifactFamily::SanitizedLog,
            "referenced_path": "target/p1-gate/evidence/run.log",
            "synthetic_ids": synthetic_ids()
        }],
        "schema_version": 1,
        "synthetic_values": [
            synthetic_document("account-value", "norn-synthetic-account-001", SyntheticPurpose::AccountId),
            synthetic_document("cache-value", "norn-synthetic-cache-001", SyntheticPurpose::CacheKey),
            synthetic_document("credential-value", "norn-synthetic-credential-001", SyntheticPurpose::Credential),
            synthetic_document("generic-value", "norn-synthetic-generic-001", SyntheticPurpose::Generic),
            synthetic_document("prompt-value", "norn-synthetic-prompt-001", SyntheticPurpose::PromptContent),
            synthetic_document("state-value", "norn-synthetic-state-001", SyntheticPurpose::TurnState)
        ]
    }))
}

fn synthetic_document(id: &str, value: &str, purpose: SyntheticPurpose) -> Value {
    json!({
        "generator": "p1-fixture-generator-v1",
        "id": id,
        "provenance": "crates/norn-policy/tests/redaction/support.rs",
        "purpose": purpose,
        "sentinel_class": SentinelClass::NonReusableFixtureV1,
        "value": value
    })
}
