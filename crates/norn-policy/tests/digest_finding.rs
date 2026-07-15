//! Canonical digest and closed finding identity tests.

use std::error::Error;

use norn_policy::digest::{CanonicalJsonError, Digest, DigestParseError, canonical_json_bytes};
use norn_policy::finding::{
    ArtifactIdentity, ByteSpan, CargoManifestIssue, CargoTargetIssue, CargoTargetKind,
    DebtConstructKind, DebtTargetKind, EvidenceRedactionIssue, EvidenceTraceabilityIssue, Finding,
    FindingCode, FindingPhase, FindingRuleFamily, GeneratedIncludeIssue, LegacyChangeIssue,
    LegacyKind, ModuleResolutionIssue, ModuleShapeIssue, PolicyInput, PolicyInputIssue,
    RepositoryFinding, UnknownWriterIssue, UnsupportedEntryKind, WriterClassificationIssue,
};
use norn_policy::{RepositoryPath, digest_bytes, digest_json};
use serde_json::json;

#[test]
fn byte_digest_uses_complete_lowercase_sha256() -> Result<(), Box<dyn Error>> {
    let digest = digest_bytes(b"abc");
    assert_eq!(
        digest.to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(digest.as_bytes().len(), 32);

    let parsed: Digest = digest.to_string().parse()?;
    assert_eq!(parsed, digest);

    let encoded = serde_json::to_string(&digest)?;
    let decoded: Digest = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, digest);
    Ok(())
}

#[test]
fn digest_parser_rejects_wrong_length_uppercase_and_non_hex() {
    assert_eq!(
        "00".parse::<Digest>(),
        Err(DigestParseError::Length { actual: 2 })
    );

    let uppercase = "A000000000000000000000000000000000000000000000000000000000000000";
    assert_eq!(
        uppercase.parse::<Digest>(),
        Err(DigestParseError::InvalidHex { index: 0 })
    );

    let non_hex = "g000000000000000000000000000000000000000000000000000000000000000";
    assert_eq!(
        non_hex.parse::<Digest>(),
        Err(DigestParseError::InvalidHex { index: 0 })
    );
}

#[test]
fn canonical_json_sorts_objects_and_preserves_array_order() -> Result<(), Box<dyn Error>> {
    let first = json!({"b": 1, "nested": {"z": false, "a": null}, "a": 2});
    let second = json!({"a": 2, "nested": {"a": null, "z": false}, "b": 1});
    let canonical = br#"{"a":2,"b":1,"nested":{"a":null,"z":false}}"#;

    assert_eq!(canonical_json_bytes(&first)?.as_slice(), canonical);
    assert_eq!(digest_json(&first)?, digest_json(&second)?);
    assert_eq!(digest_json(&first)?, digest_bytes(canonical));
    assert_ne!(digest_json(&json!([1, 2]))?, digest_json(&json!([2, 1]))?);
    Ok(())
}

#[test]
fn canonical_json_rejects_floating_point_numbers() {
    let result = digest_json(&json!({"ratio": 1.5}));
    assert!(matches!(
        result,
        Err(CanonicalJsonError::FloatingPointNumber)
    ));
}

#[test]
fn byte_span_enforces_a_half_open_range() -> Result<(), Box<dyn Error>> {
    let span = ByteSpan::new(4, 9)?;
    assert_eq!(span.start(), 4);
    assert_eq!(span.end(), 9);
    assert!(ByteSpan::new(9, 4).is_err());
    Ok(())
}

#[test]
fn repository_finding_serialization_has_one_code_implied_shape() -> Result<(), Box<dyn Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    let span = ByteSpan::new(10, 20)?;
    let finding = Finding::repository(
        path.clone(),
        Some(span),
        RepositoryFinding::ProductionLocExceeded {
            actual: 201,
            limit: 200,
        },
    );

    assert_eq!(finding.code(), FindingCode::ProductionLocExceeded);
    assert_eq!(finding.path(), Some(&path));
    assert_eq!(finding.artifact(), None);
    assert!(finding.repository_details().is_some());
    assert_eq!(finding.evidence_redaction_issue(), None);
    assert_eq!(finding.span(), Some(span));
    assert_eq!(finding.algorithm_version(), "norn-policy-1");

    let encoded = serde_json::to_value(&finding)?;
    assert_eq!(encoded["code"], "rust.loc_exceeded");
    assert_eq!(
        encoded["location"],
        json!({"kind": "repository", "path": "src/lib.rs"})
    );
    assert_eq!(encoded["span"], json!({"start": 10, "end": 20}));
    assert_eq!(encoded["fields"], json!({"actual": 201, "limit": 200}));
    assert_eq!(encoded["algorithm_version"], "norn-policy-1");
    assert!(encoded.get("message").is_none());
    assert!(encoded.get("source_snippet").is_none());
    Ok(())
}

#[test]
fn artifact_finding_never_discloses_its_path_or_invents_a_family() -> Result<(), Box<dyn Error>> {
    let private_path = "/Users/example/.config/norn/sk-live-private-credential";
    let identity = ArtifactIdentity::observed(7);
    let finding =
        Finding::evidence_redaction(identity, None, EvidenceRedactionIssue::UnregisteredArtifact);

    assert_eq!(finding.code(), FindingCode::EvidenceRedaction);
    assert_eq!(finding.path(), None);
    assert_eq!(finding.artifact(), Some(identity));
    assert!(finding.repository_details().is_none());
    assert_eq!(
        finding.evidence_redaction_issue(),
        Some(EvidenceRedactionIssue::UnregisteredArtifact)
    );

    let encoded = serde_json::to_string(&finding)?;
    let debug = format!("{finding:?}");
    assert!(!encoded.contains(private_path));
    assert!(!debug.contains(private_path));
    assert!(!encoded.contains("sk-live-private-credential"));
    assert!(!debug.contains("sk-live-private-credential"));
    assert!(!encoded.contains("family"));

    let value = serde_json::to_value(&finding)?;
    assert_eq!(value["code"], "evidence.redaction");
    assert_eq!(value["fields"], json!({"issue": "unregistered_artifact"}));
    assert_eq!(value["location"]["kind"], "artifact");
    assert_eq!(value["location"]["artifact"]["ordinal"], 7);
    assert!(value["location"]["artifact"].get("path_digest").is_none());
    Ok(())
}

#[test]
fn findings_sort_by_location_span_code_and_typed_payload() -> Result<(), Box<dyn Error>> {
    let a: RepositoryPath = "a.rs".parse()?;
    let z: RepositoryPath = "z.rs".parse()?;
    let mut findings = [
        Finding::repository(
            z,
            None,
            RepositoryFinding::RuleFamilyUnavailable {
                rule: FindingRuleFamily::EvidenceRedaction,
            },
        ),
        Finding::repository(
            a.clone(),
            Some(ByteSpan::new(5, 6)?),
            RepositoryFinding::ModuleShape {
                construct_kind: ModuleShapeIssue::OtherItem,
            },
        ),
        Finding::repository(
            a,
            None,
            RepositoryFinding::ProductionLocExceeded {
                actual: 501,
                limit: 500,
            },
        ),
    ];
    findings.sort();

    assert_eq!(findings[0].code(), FindingCode::ProductionLocExceeded);
    assert_eq!(findings[1].code(), FindingCode::ModuleShape);
    assert_eq!(findings[2].code(), FindingCode::RuleFamilyUnavailable);
    Ok(())
}

#[test]
fn every_finding_code_is_implied_by_one_typed_variant() -> Result<(), Box<dyn Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    let mut codes = Vec::new();
    for details in repository_findings() {
        let finding = Finding::repository(path.clone(), None, details);
        let encoded = serde_json::to_value(&finding)?;
        assert_eq!(encoded["code"], finding.code().as_str());
        codes.push(finding.code());
    }
    let redaction = Finding::evidence_redaction(
        ArtifactIdentity::registered(0, digest_bytes(b"artifact")),
        None,
        EvidenceRedactionIssue::SchemaMismatch,
    );
    let encoded = serde_json::to_value(&redaction)?;
    assert_eq!(encoded["code"], redaction.code().as_str());
    codes.push(redaction.code());
    codes.sort_unstable();

    assert_eq!(
        codes,
        vec![
            FindingCode::PolicyInputMissing,
            FindingCode::PolicyInputUnreadable,
            FindingCode::PolicyInputInvalid,
            FindingCode::UnknownSchemaVersion,
            FindingCode::DigestMismatch,
            FindingCode::SymlinkEntry,
            FindingCode::UnsupportedEntry,
            FindingCode::InvalidCargoManifest,
            FindingCode::InvalidCargoTarget,
            FindingCode::UnclassifiedRustSource,
            FindingCode::ModuleResolution,
            FindingCode::GeneratedInclude,
            FindingCode::ProductionLocExceeded,
            FindingCode::ModuleShape,
            FindingCode::ProductionHiddenAsTest,
            FindingCode::ProhibitedDebt,
            FindingCode::LegacyExceptionChanged,
            FindingCode::LegacyExceptionOverdue,
            FindingCode::UnknownWriterSink,
            FindingCode::WriterClassification,
            FindingCode::EvidenceRedaction,
            FindingCode::EvidenceTraceability,
            FindingCode::RuleFamilyUnavailable,
        ]
    );
    Ok(())
}

#[test]
fn writer_classification_variants_match_the_classifier_payload_exactly()
-> Result<(), Box<dyn Error>> {
    let operation = digest_bytes(b"writer-operation");
    let issues = [
        WriterClassificationIssue::Missing { operation },
        WriterClassificationIssue::Duplicate { operation },
        WriterClassificationIssue::Stale { operation },
        WriterClassificationIssue::SharedEdges { operation },
    ];
    let expected = ["missing", "duplicate", "stale", "shared_edges"];
    let path: RepositoryPath = "policy/writer-families.toml".parse()?;

    for (issue, expected_issue) in issues.into_iter().zip(expected) {
        let finding = Finding::repository(
            path.clone(),
            None,
            RepositoryFinding::WriterClassification { issue },
        );
        let encoded = serde_json::to_value(&finding)?;
        assert_eq!(encoded["fields"]["issue"]["issue"], expected_issue);
        assert_eq!(encoded["fields"]["issue"]["operation"], operation.to_hex());
        assert!(encoded["fields"].get("operation_kind").is_none());
        assert!(encoded["fields"].get("family").is_none());
    }
    Ok(())
}

#[test]
fn every_redaction_issue_is_closed_machine_text() -> Result<(), Box<dyn Error>> {
    let issues = [
        EvidenceRedactionIssue::UnregisteredArtifact,
        EvidenceRedactionIssue::RegisteredArtifactMissing,
        EvidenceRedactionIssue::NonRegularArtifact,
        EvidenceRedactionIssue::ArtifactDigestMismatch,
        EvidenceRedactionIssue::InvalidJson,
        EvidenceRedactionIssue::InvalidJsonl,
        EvidenceRedactionIssue::DuplicateJsonlRow,
        EvidenceRedactionIssue::SchemaMismatch,
        EvidenceRedactionIssue::ArtifactIdentityMismatch,
        EvidenceRedactionIssue::ArtifactFamilyMismatch,
        EvidenceRedactionIssue::ProhibitedField,
        EvidenceRedactionIssue::ReusableState,
        EvidenceRedactionIssue::DangerousShape,
        EvidenceRedactionIssue::ControlCharacter,
        EvidenceRedactionIssue::AbsolutePath,
        EvidenceRedactionIssue::UnregisteredString,
        EvidenceRedactionIssue::SyntheticMetadataMismatch,
        EvidenceRedactionIssue::RegisteredValueMissing,
        EvidenceRedactionIssue::UnstableRowOrder,
        EvidenceRedactionIssue::ObservationMismatch,
        EvidenceRedactionIssue::ReferencedArtifactMissing,
        EvidenceRedactionIssue::ReferencedArtifactNonRegular,
        EvidenceRedactionIssue::ReferencedArtifactDigestMismatch,
        EvidenceRedactionIssue::SpanUnrepresentable,
    ];
    assert_eq!(issues.len(), 24);
    for issue in issues {
        assert!(serde_json::to_value(issue)?.is_string());
    }
    Ok(())
}

#[test]
fn finding_code_is_stable_machine_text() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        FindingCode::UnknownWriterSink.as_str(),
        "writer.unknown_sink"
    );
    assert_eq!(
        serde_json::to_string(&FindingCode::EvidenceRedaction)?,
        "\"evidence.redaction\""
    );
    Ok(())
}

fn repository_findings() -> Vec<RepositoryFinding> {
    let digest = digest_bytes(b"typed-finding");
    vec![
        RepositoryFinding::PolicyInputMissing {
            input: PolicyInput::RepositoryPolicy,
        },
        RepositoryFinding::PolicyInputUnreadable {
            input: PolicyInput::PhaseLock,
            issue: PolicyInputIssue::Read,
        },
        RepositoryFinding::PolicyInputInvalid {
            input: PolicyInput::OriginLedger,
            issue: PolicyInputIssue::AuthorityMismatch,
        },
        RepositoryFinding::UnknownSchemaVersion {
            input: PolicyInput::LegacyGovernance,
            schema_version: 2,
        },
        RepositoryFinding::DigestMismatch {
            input: PolicyInput::WriterFamilies,
            expected: digest,
            actual: digest_bytes(b"observed"),
        },
        RepositoryFinding::SymlinkEntry,
        RepositoryFinding::UnsupportedEntry {
            actual: UnsupportedEntryKind::Special,
        },
        RepositoryFinding::InvalidCargoManifest {
            issue: CargoManifestIssue::ManifestMalformed,
        },
        RepositoryFinding::InvalidCargoTarget {
            target_kind: Some(CargoTargetKind::Binary),
            issue: CargoTargetIssue::TargetMissing,
        },
        RepositoryFinding::UnclassifiedRustSource,
        RepositoryFinding::ModuleResolution {
            issue: ModuleResolutionIssue::ModuleAmbiguous,
        },
        RepositoryFinding::GeneratedInclude {
            issue: GeneratedIncludeIssue::RegistryDrift,
        },
        RepositoryFinding::ProductionLocExceeded {
            actual: 501,
            limit: 500,
        },
        RepositoryFinding::ModuleShape {
            construct_kind: ModuleShapeIssue::PrivateUse,
        },
        RepositoryFinding::ProductionHiddenAsTest {
            fingerprint: digest,
            count: 1,
        },
        RepositoryFinding::ProhibitedDebt {
            target_kind: DebtTargetKind::Library,
            construct_kind: DebtConstructKind::UnwrapCall,
            fingerprint: digest,
        },
        RepositoryFinding::LegacyExceptionChanged {
            origin: digest,
            kind: LegacyKind::ProductionLoc,
            issue: LegacyChangeIssue::LocChanged,
        },
        RepositoryFinding::LegacyExceptionOverdue {
            origin: digest,
            kind: LegacyKind::ProhibitedDebt,
            due_phase: FindingPhase::P2,
        },
        RepositoryFinding::UnknownWriterSink {
            fingerprint: digest,
            issue: UnknownWriterIssue::DynamicReceiver,
        },
        RepositoryFinding::WriterClassification {
            issue: WriterClassificationIssue::Missing { operation: digest },
        },
        RepositoryFinding::EvidenceTraceability {
            issue: EvidenceTraceabilityIssue::EvidenceMissing,
            count: 1,
        },
        RepositoryFinding::RuleFamilyUnavailable {
            rule: FindingRuleFamily::WriterInventory,
        },
    ]
}
