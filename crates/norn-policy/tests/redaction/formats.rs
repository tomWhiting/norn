use std::error::Error;

use norn_policy::redaction::{
    ArtifactFamily, ArtifactRegistration, RedactionCode, RedactionRegistry,
};
use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry, digest_bytes};
use serde_json::json;

use super::support::{assert_has, bytes, codes, fixture, replace};

#[test]
fn rejects_duplicate_json_members_and_jsonl_rows() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let duplicate_json = br#"{"type":"response.completed","type":"response.completed"}"#;
    let protocol = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(duplicate_json.to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &protocol),
        RedactionCode::InvalidJson,
    );

    let duplicate_key = br#"{"finding_id":"EVT-001","finding_id":"EVT-001"}"#;
    let jsonl = replace(
        &fixture.snapshot,
        &fixture.paths.traceability,
        SnapshotEntry::regular(duplicate_key.to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &jsonl),
        RedactionCode::InvalidJsonl,
    );

    let row = bytes(&fixture.snapshot, &fixture.paths.traceability)?;
    let mut duplicate_row = row.to_vec();
    duplicate_row.extend_from_slice(row);
    let jsonl = replace(
        &fixture.snapshot,
        &fixture.paths.traceability,
        SnapshotEntry::regular(duplicate_row),
    )?;
    assert_has(
        &codes(&fixture.registry, &jsonl),
        RedactionCode::DuplicateJsonlRow,
    );
    Ok(())
}

#[test]
fn protocol_sse_is_strictly_parsed_and_scanned() -> Result<(), Box<dyn Error>> {
    let path = RepositoryPath::parse(
        "crates/norn/testdata/openai_responses/public/streams/completed.sse",
    )?;
    let bytes = b": norn-fixture-v1 {\"schema_version\":1,\"artifact_family\":\"protocol_fixture\",\"fixture_id\":\"sse-fixture\",\"dialect\":\"public\",\"artifact_kind\":\"stream\"}\nevent: response.completed\ndata: {\"sequence_number\":1,\"type\":\"response.completed\"}\n\n";
    let registration = ArtifactRegistration::new(
        "sse-fixture",
        path.clone(),
        ArtifactFamily::ProtocolFixture,
        digest_bytes(bytes),
        Vec::new(),
        Vec::new(),
    )?;
    let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
    let snapshot =
        OwnedSnapshot::try_from_entries([(path.clone(), SnapshotEntry::regular(bytes.to_vec()))])?;
    assert!(codes(&registry, &snapshot).is_empty());

    let missing_envelope = OwnedSnapshot::try_from_entries([(
        path.clone(),
        SnapshotEntry::regular(
            b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n".to_vec(),
        ),
    )])?;
    assert_has(
        &codes(&registry, &missing_envelope),
        RedactionCode::SchemaMismatch,
    );

    let mismatched_event = b": norn-fixture-v1 {\"schema_version\":1,\"artifact_family\":\"protocol_fixture\",\"fixture_id\":\"sse-fixture\",\"dialect\":\"public\",\"artifact_kind\":\"stream\"}\nevent: response.failed\ndata: {\"type\":\"response.completed\"}\n";
    let mismatched = OwnedSnapshot::try_from_entries([(
        path,
        SnapshotEntry::regular(mismatched_event.to_vec()),
    )])?;
    assert_has(
        &codes(&registry, &mismatched),
        RedactionCode::SchemaMismatch,
    );
    Ok(())
}

#[test]
fn text_families_require_utf8() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.tool,
        SnapshotEntry::regular(vec![0xff, 0xfe]),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );

    let private_log = replace(
        &fixture.snapshot,
        &fixture.paths.log,
        SnapshotEntry::regular(b"message=private-project\n".to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &private_log),
        RedactionCode::SchemaMismatch,
    );
    Ok(())
}

#[test]
fn evidence_tool_source_rejects_raw_windows_home_paths() -> Result<(), Box<dyn Error>> {
    let path = RepositoryPath::parse("scripts/p1-gate")?;
    let private_path = ["C", ":", "\\", "Users", "\\", "private"].concat();
    let bytes = private_path.into_bytes();
    let registration = ArtifactRegistration::new(
        "gate-script",
        path.clone(),
        ArtifactFamily::EvidenceToolSource,
        digest_bytes(&bytes),
        Vec::new(),
        Vec::new(),
    )?;
    let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
    let snapshot = OwnedSnapshot::try_from_entries([(path, SnapshotEntry::regular(bytes))])?;

    assert_has(&codes(&registry, &snapshot), RedactionCode::AbsolutePath);
    Ok(())
}

#[test]
fn exact_gate_scripts_and_manifest_are_valid_evidence_tools() -> Result<(), Box<dyn Error>> {
    let script_path = RepositoryPath::parse("scripts/p1-gate")?;
    let script = b"{}";
    let script_registration = ArtifactRegistration::new(
        "gate-script",
        script_path.clone(),
        ArtifactFamily::EvidenceToolSource,
        digest_bytes(script),
        Vec::new(),
        Vec::new(),
    )?;
    let script_registry = RedactionRegistry::new(vec![script_registration], Vec::new())?;
    let script_snapshot =
        OwnedSnapshot::try_from_entries([(script_path, SnapshotEntry::regular(script.to_vec()))])?;
    assert!(codes(&script_registry, &script_snapshot).is_empty());

    let manifest_path = RepositoryPath::parse("policy/gate-commands.json")?;
    let manifest = include_bytes!("../../../../policy/gate-commands.json");
    let manifest_registration = ArtifactRegistration::new(
        "gate-manifest",
        manifest_path.clone(),
        ArtifactFamily::ContractSchema,
        digest_bytes(manifest),
        Vec::new(),
        Vec::new(),
    )?;
    let manifest_registry = RedactionRegistry::new(vec![manifest_registration], Vec::new())?;
    let manifest_snapshot = OwnedSnapshot::try_from_entries([(
        manifest_path,
        SnapshotEntry::regular(manifest.to_vec()),
    )])?;
    let manifest_codes = codes(&manifest_registry, &manifest_snapshot);
    assert!(
        manifest_codes.is_empty(),
        "unexpected manifest violations: {manifest_codes:?}"
    );

    let authority_path =
        RepositoryPath::parse("crates/norn-policy/tests/evidence/p1_base_authority.json")?;
    let authority = include_bytes!("../evidence/p1_base_authority.json");
    let authority_registration = ArtifactRegistration::new(
        "base-authority",
        authority_path.clone(),
        ArtifactFamily::ContractSchema,
        digest_bytes(authority),
        Vec::new(),
        Vec::new(),
    )?;
    let authority_registry = RedactionRegistry::new(vec![authority_registration], Vec::new())?;
    let authority_snapshot = OwnedSnapshot::try_from_entries([(
        authority_path,
        SnapshotEntry::regular(authority.to_vec()),
    )])?;
    let authority_codes = codes(&authority_registry, &authority_snapshot);
    assert!(
        authority_codes.is_empty(),
        "unexpected base-authority redaction codes: {authority_codes:?}"
    );
    Ok(())
}

#[test]
fn base_authority_rejects_duplicate_json_keys() -> Result<(), Box<dyn Error>> {
    let path = RepositoryPath::parse("crates/norn-policy/tests/evidence/p1_base_authority.json")?;
    let bytes = br#"{"schema_version":1,"schema_version":1}"#;
    let registration = ArtifactRegistration::new(
        "base-authority",
        path.clone(),
        ArtifactFamily::ContractSchema,
        digest_bytes(bytes),
        Vec::new(),
        Vec::new(),
    )?;
    let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
    let snapshot =
        OwnedSnapshot::try_from_entries([(path, SnapshotEntry::regular(bytes.to_vec()))])?;

    assert_has(&codes(&registry, &snapshot), RedactionCode::InvalidJson);
    Ok(())
}

#[test]
fn contract_registration_cannot_redefine_a_reviewed_document() -> Result<(), Box<dyn Error>> {
    let path = RepositoryPath::parse("policy/gate-commands.json")?;
    let bytes = b"{}";
    let registration = ArtifactRegistration::new(
        "gate-manifest",
        path.clone(),
        ArtifactFamily::ContractSchema,
        digest_bytes(bytes),
        Vec::new(),
        Vec::new(),
    )?;
    let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
    let snapshot =
        OwnedSnapshot::try_from_entries([(path, SnapshotEntry::regular(bytes.to_vec()))])?;

    assert_has(&codes(&registry, &snapshot), RedactionCode::SchemaMismatch);
    Ok(())
}

#[test]
fn one_traceability_row_cannot_impersonate_the_reviewed_registry() -> Result<(), Box<dyn Error>> {
    let path = RepositoryPath::parse("docs/reviews/evidence/p1/finding-traceability.jsonl")?;
    let mut bytes = serde_json::to_vec(&json!({
        "closure_status": "open",
        "current_seams": ["crates/norn/src/provider/openai/sse.rs"],
        "evidence_class": "confirmed_defect",
        "evidence_method": "defect_regression",
        "expectation_class": "baseline_red",
        "finding_id": "EVT-01",
        "fixture_applicability": "planned",
        "fixture_category": "responses/events",
        "owner_phase": "P4",
        "planned_evidence_id": "p4-evt-01-defect-regression",
        "planned_fixture_ids": ["fixture-responses-events"],
        "source_evidence": "docs/reviews/source.md",
        "source_severity": "high",
        "target_assertion": "Terminal events remain typed."
    }))?;
    bytes.push(b'\n');
    let registration = ArtifactRegistration::new(
        "traceability",
        path.clone(),
        ArtifactFamily::TraceabilityJsonl,
        digest_bytes(&bytes),
        Vec::new(),
        Vec::new(),
    )?;
    let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
    let snapshot = OwnedSnapshot::try_from_entries([(path, SnapshotEntry::regular(bytes))])?;

    assert_has(&codes(&registry, &snapshot), RedactionCode::SchemaMismatch);
    Ok(())
}

#[test]
fn decoded_unicode_paths_are_rejected() -> Result<(), Box<dyn Error>> {
    for encoded in [
        br#"{"value":"\u002froot\u002fprivate"}"#.as_slice(),
        br#"{"value":"prefix C\u003a\u005cUsers\u005cprivate"}"#.as_slice(),
    ] {
        let path = RepositoryPath::parse("policy/gate-commands.json")?;
        let registration = ArtifactRegistration::new(
            "gate-manifest",
            path.clone(),
            ArtifactFamily::ContractSchema,
            digest_bytes(encoded),
            Vec::new(),
            Vec::new(),
        )?;
        let registry = RedactionRegistry::new(vec![registration], Vec::new())?;
        let snapshot =
            OwnedSnapshot::try_from_entries([(path, SnapshotEntry::regular(encoded.to_vec()))])?;
        assert_has(&codes(&registry, &snapshot), RedactionCode::AbsolutePath);
    }
    Ok(())
}
