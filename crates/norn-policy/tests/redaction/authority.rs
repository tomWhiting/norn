use std::error::Error;

use norn_policy::redaction::{
    ArtifactFamily, ArtifactRegistration, ObservationRegistration, ObservationSource, PublicUrl,
    RedactionRegistry, RegistrationError, SentinelClass, SyntheticPurpose, SyntheticRegistration,
    redaction_schema_digest,
};
use norn_policy::{RepositoryPath, digest_bytes};
use serde_json::json;

use super::support::fixture;

#[test]
fn checked_in_registry_document_is_strict_and_non_vacuous() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let document = serde_json::to_vec(&json!({
        "schema_version": 1,
        "artifacts": [{
            "id": "p1-protocol-001",
            "path": fixture.paths.protocol.as_str(),
            "family": "protocol_fixture",
            "sha256": digest_bytes(b"{}"),
            "synthetic_ids": ["generic-value"],
            "observations": []
        }],
        "synthetics": [{
            "id": "generic-value",
            "value": "norn-synthetic-generic-001",
            "generator": "p1-fixture-generator-v1",
            "provenance": "crates/norn-policy/tests/redaction/support.rs",
            "purpose": "generic",
            "sentinel_class": "non_reusable_fixture_v1"
        }]
    }))?;
    let registry = RedactionRegistry::decode_p1(&document)?;
    assert_eq!(registry.registered_paths().len(), 1);

    let duplicate = br#"{"schema_version":1,"schema_version":1,"artifacts":[],"synthetics":[]}"#;
    assert!(RedactionRegistry::decode_p1(duplicate).is_err());
    let unknown = br#"{"schema_version":1,"artifacts":[],"synthetics":[],"extra":null}"#;
    assert!(RedactionRegistry::decode_p1(unknown).is_err());
    let empty = br#"{"schema_version":1,"artifacts":[],"synthetics":[]}"#;
    assert!(RedactionRegistry::decode_p1(empty).is_err());
    Ok(())
}

#[test]
fn registry_and_schema_digests_are_deterministic() -> Result<(), Box<dyn Error>> {
    let first = fixture()?;
    let second = fixture()?;
    assert_eq!(first.registry.digest(), second.registry.digest());
    assert_eq!(
        RedactionRegistry::schema_digest(),
        redaction_schema_digest()
    );
    assert_ne!(first.registry.digest(), redaction_schema_digest());
    Ok(())
}

#[test]
fn authority_requires_stable_order_and_family_roots() -> Result<(), Box<dyn Error>> {
    let first_path =
        RepositoryPath::parse("crates/norn/testdata/openai_responses/public/first.json")?;
    let second_path =
        RepositoryPath::parse("crates/norn/testdata/openai_responses/public/second.json")?;
    let first = ArtifactRegistration::new(
        "first-artifact",
        first_path,
        ArtifactFamily::ProtocolFixture,
        digest_bytes(b"{}"),
        Vec::new(),
        Vec::new(),
    )?;
    let second = ArtifactRegistration::new(
        "second-artifact",
        second_path,
        ArtifactFamily::ProtocolFixture,
        digest_bytes(b"{}"),
        Vec::new(),
        Vec::new(),
    )?;
    assert_eq!(
        RedactionRegistry::new(vec![second, first], Vec::new()),
        Err(RegistrationError::UnstableAuthorityOrder)
    );

    let wrong_root = ArtifactRegistration::new(
        "wrong-root",
        RepositoryPath::parse("policy/contracts/openai-responses-v1/wrong.json")?,
        ArtifactFamily::ProtocolFixture,
        digest_bytes(b"{}"),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(wrong_root, Err(RegistrationError::InvalidArtifactPath));
    Ok(())
}

#[test]
fn registry_rejects_tuple_digest_or_family_swaps() -> Result<(), Box<dyn Error>> {
    let gate_path = RepositoryPath::parse("target/p1-gate/evidence/gate.json")?;
    let log_path = RepositoryPath::parse("target/p1-gate/evidence/run.log")?;
    let observation = ObservationRegistration::new(
        "gate-observation",
        log_path.clone(),
        ArtifactFamily::SanitizedLog,
        ObservationSource::LocalGate,
        Vec::new(),
        digest_bytes(b"different"),
    )?;
    let gate = ArtifactRegistration::new(
        "gate-artifact",
        gate_path,
        ArtifactFamily::GateDescriptor,
        digest_bytes(b"{}"),
        Vec::new(),
        vec![observation],
    )?;
    let log = ArtifactRegistration::new(
        "log-artifact",
        log_path,
        ArtifactFamily::SanitizedLog,
        digest_bytes(b"log"),
        Vec::new(),
        Vec::new(),
    )?;
    assert_eq!(
        RedactionRegistry::new(vec![gate, log], Vec::new()),
        Err(RegistrationError::ObservationBindingMismatch)
    );
    Ok(())
}

#[test]
fn authority_debug_and_errors_do_not_render_values_or_paths() -> Result<(), Box<dyn Error>> {
    let value = "norn-synthetic-private-value-001";
    let provenance = RepositoryPath::parse("crates/norn-policy/tests/redaction/support.rs")?;
    let synthetic = SyntheticRegistration::new(
        "private-value",
        value,
        "p1-fixture-generator-v1",
        provenance,
        SyntheticPurpose::Generic,
        SentinelClass::NonReusableFixtureV1,
    )?;
    let rendered = format!("{synthetic:?}");
    assert!(!rendered.contains(value));
    assert!(rendered.contains("[REDACTED]"));

    let private_path = RepositoryPath::parse("private/prompt/location.rs")?;
    let result = SyntheticRegistration::new(
        "private-path",
        "norn-synthetic-private-path-001",
        "p1-fixture-generator-v1",
        private_path.clone(),
        SyntheticPurpose::Generic,
        SentinelClass::NonReusableFixtureV1,
    );
    let error = result.err().ok_or("expected rejected provenance")?;
    let rendered = error.to_string();
    assert!(!rendered.contains(private_path.as_str()));
    Ok(())
}

#[test]
fn observation_sources_use_exact_ratified_urls_or_non_url_provenance() -> Result<(), Box<dyn Error>>
{
    let urls = [
        PublicUrl::OpenAiResponsesEndpoint,
        PublicUrl::OpenAiCompactEndpoint,
        PublicUrl::OpenAiStreamingEvents,
        PublicUrl::OpenAiWebsocketEvents,
    ];
    for url in urls {
        let encoded = serde_json::to_string(&url)?;
        let decoded = serde_json::from_str::<PublicUrl>(&encoded)?;
        assert_eq!(decoded, url);
    }
    assert_eq!(
        serde_json::to_string(&ObservationSource::LocalGate)?,
        "\"local_gate\""
    );
    assert_eq!(
        serde_json::to_string(&ObservationSource::CodexSourcePin)?,
        "\"codex_source_pin\""
    );
    let obsolete = "\"https://developers.openai.com/api/reference/resources/responses\"";
    assert!(serde_json::from_str::<PublicUrl>(obsolete).is_err());
    Ok(())
}
