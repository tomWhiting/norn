use std::collections::BTreeSet;

use serde_json::Value;

use crate::digest::digest_bytes;
use crate::snapshot::{EntryKind, OwnedSnapshot};
use crate::strict_json::decode_strict_json;

use super::authority::RedactionRegistry;
use super::contract::validate_contract_document;
use super::evidence_document::{EvidenceDocument, ObservationDocument, SyntheticValueDocument};
use super::gate_evidence;
use super::model::{ArtifactFamily, ArtifactRegistration};
use super::protocol::validate_protocol_artifact;
use super::scan::{decoded_string_violation, evidence_key_violation, raw_violations};
use super::traceability::validate_traceability_document;
use super::validate::{ArtifactIssue, RedactionCode, raw_issue};

pub(crate) fn validate_artifact_content(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    snapshot: &OwnedSnapshot,
    bytes: &[u8],
) -> Vec<ArtifactIssue> {
    let mut issues = raw_violations(bytes)
        .into_iter()
        .map(raw_issue)
        .collect::<Vec<_>>();
    match registration.family() {
        ArtifactFamily::ProtocolFixture => {
            validate_protocol_artifact(registry, registration, bytes, &mut issues);
        }
        ArtifactFamily::TraceabilityJsonl => {
            validate_traceability_document(registration, bytes, &mut issues);
        }
        ArtifactFamily::ContractSchema => {
            validate_contract_document(registration, bytes, &mut issues);
        }
        ArtifactFamily::GateDescriptor => {
            validate_gate_descriptor(registration, snapshot, &mut issues);
        }
        ArtifactFamily::Distribution => {
            validate_evidence_document(registry, registration, snapshot, bytes, &mut issues);
        }
        ArtifactFamily::SanitizedLog => validate_sanitized_log(bytes, &mut issues),
        ArtifactFamily::EvidenceToolSource => {
            if std::str::from_utf8(bytes).is_err() {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        }
    }
    issues
}

fn validate_gate_descriptor(
    registration: &ArtifactRegistration,
    snapshot: &OwnedSnapshot,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Ok(expected) = gate_evidence::expected_descriptor(snapshot, registration.path()) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    if expected != *registration {
        issues.push(issue(RedactionCode::ObservationMismatch));
    }
}

fn validate_sanitized_log(bytes: &[u8], issues: &mut Vec<ArtifactIssue>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    if text.is_empty() || !text.ends_with('\n') {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    }
    for line in text.lines() {
        if line.is_empty() {
            issues.push(issue(RedactionCode::SchemaMismatch));
            continue;
        }
        let mut keys = BTreeSet::new();
        let mut saw_field = false;
        for field in line.split_ascii_whitespace() {
            saw_field = true;
            let Some((key, value)) = field.split_once('=') else {
                issues.push(issue(RedactionCode::SchemaMismatch));
                continue;
            };
            if !keys.insert(key) || !sanitized_log_field(key, value) {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        }
        if !saw_field {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
    }
}

fn sanitized_log_field(key: &str, value: &str) -> bool {
    match key {
        "attempt" | "duration_ms" | "exit_status" | "failed" | "iteration" | "passed" | "tests" => {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        }
        "commit" => {
            value.len() == 40
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }
        "digest" => {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }
        "result" => matches!(value, "fail" | "pass"),
        _ => false,
    }
}

fn validate_evidence_document(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    snapshot: &OwnedSnapshot,
    bytes: &[u8],
    issues: &mut Vec<ArtifactIssue>,
) {
    let Ok(value) = decode_strict_json::<Value>(bytes) else {
        issues.push(issue(RedactionCode::InvalidJson));
        return;
    };
    scan_evidence_value(&value, issues);
    let Ok(document) = serde_json::from_value::<EvidenceDocument>(value) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    if document.schema_version != registration.family().schema_version() {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
    if document.artifact_family != registration.family() {
        issues.push(issue(RedactionCode::ArtifactFamilyMismatch));
    }
    if document.artifact_id != registration.id() {
        issues.push(issue(RedactionCode::ArtifactIdentityMismatch));
    }
    validate_synthetic_rows(registry, registration, &document.synthetic_values, issues);
    validate_observation_rows(registration, snapshot, &document.observations, issues);
}

fn validate_synthetic_rows(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    rows: &[SyntheticValueDocument],
    issues: &mut Vec<ArtifactIssue>,
) {
    if !rows.windows(2).all(|pair| pair[0].id < pair[1].id) {
        issues.push(issue(RedactionCode::UnstableRowOrder));
    }
    if rows.len() != registration.synthetic_ids().len() {
        issues.push(issue(RedactionCode::RegisteredValueMissing));
    }
    for (row, expected_id) in rows.iter().zip(registration.synthetic_ids()) {
        if row.id != *expected_id {
            issues.push(issue(RedactionCode::SyntheticMetadataMismatch));
            continue;
        }
        let Some(expected) = registry.synthetic(expected_id) else {
            issues.push(issue(RedactionCode::SyntheticMetadataMismatch));
            continue;
        };
        if !expected.matches_document(
            &row.value,
            &row.generator,
            &row.provenance,
            row.purpose,
            row.sentinel_class,
        ) {
            issues.push(issue(RedactionCode::SyntheticMetadataMismatch));
        }
    }
}

fn validate_observation_rows(
    registration: &ArtifactRegistration,
    snapshot: &OwnedSnapshot,
    rows: &[ObservationDocument],
    issues: &mut Vec<ArtifactIssue>,
) {
    if !rows.windows(2).all(|pair| pair[0].id < pair[1].id) {
        issues.push(issue(RedactionCode::UnstableRowOrder));
    }
    if rows.len() != registration.observations().len() {
        issues.push(issue(RedactionCode::RegisteredValueMissing));
    }
    for (row, expected) in rows.iter().zip(registration.observations()) {
        if row.id != expected.id()
            || row.referenced_path != *expected.referenced_path()
            || row.referenced_family != expected.referenced_family()
            || row.source != expected.source()
            || row.synthetic_ids != expected.synthetic_ids()
            || row.digest != expected.digest()
        {
            issues.push(issue(RedactionCode::ObservationMismatch));
        }
        let Some(entry) = snapshot.get(expected.referenced_path()) else {
            issues.push(issue(RedactionCode::ReferencedArtifactMissing));
            continue;
        };
        if entry.kind() != EntryKind::Regular {
            issues.push(issue(RedactionCode::ReferencedArtifactNonRegular));
            continue;
        }
        if digest_bytes(entry.bytes()) != expected.digest() {
            issues.push(issue(RedactionCode::ReferencedArtifactDigestMismatch));
        }
    }
}

fn scan_evidence_value(value: &Value, issues: &mut Vec<ArtifactIssue>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key.chars().any(char::is_control) {
                    issues.push(issue(RedactionCode::ControlCharacter));
                } else if let Some(code) = evidence_key_violation(key) {
                    issues.push(issue(code.into()));
                }
                scan_evidence_value(child, issues);
            }
        }
        Value::Array(values) => {
            for child in values {
                scan_evidence_value(child, issues);
            }
        }
        Value::String(string) => {
            if let Some(code) = decoded_string_violation(string) {
                issues.push(issue(code.into()));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
