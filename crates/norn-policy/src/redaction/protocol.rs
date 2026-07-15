use serde::Deserialize;
use serde_json::Value;

use crate::strict_json::decode_strict_json;

use super::authority::RedactionRegistry;
use super::model::{ArtifactFamily, ArtifactRegistration, SyntheticPurpose};
use super::protocol_schema::{
    ProtocolArtifactKind, ProtocolDialect, ProtocolObjectRole, RuleContext, StringRule,
    expected_shape,
};
use super::validate::{ArtifactIssue, RedactionCode};

mod value;

pub(super) use value::validate_string_rule;
use value::{scan_value, validate_protocol_type};

const SSE_METADATA_PREFIX: &str = ": norn-fixture-v1 ";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolEnvelope {
    schema_version: u32,
    artifact_family: ArtifactFamily,
    fixture_id: String,
    dialect: ProtocolDialect,
    artifact_kind: ProtocolArtifactKind,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamEnvelope {
    schema_version: u32,
    artifact_family: ArtifactFamily,
    fixture_id: String,
    dialect: ProtocolDialect,
    artifact_kind: ProtocolArtifactKind,
}

struct EnvelopeIdentity<'a> {
    schema_version: u32,
    family: ArtifactFamily,
    fixture_id: &'a str,
    dialect: ProtocolDialect,
    kind: ProtocolArtifactKind,
}

pub(crate) fn validate_protocol_artifact(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    bytes: &[u8],
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some((dialect, kind)) = expected_shape(registration.path().as_str()) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    if kind == ProtocolArtifactKind::Stream {
        validate_sse(registry, registration, bytes, dialect, issues);
    } else {
        validate_json(registry, registration, bytes, dialect, kind, issues);
    }
}

fn validate_json(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    bytes: &[u8],
    dialect: ProtocolDialect,
    kind: ProtocolArtifactKind,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Ok(value) = decode_strict_json::<Value>(bytes) else {
        issues.push(issue(RedactionCode::InvalidJson));
        return;
    };
    let Ok(document) = serde_json::from_value::<ProtocolEnvelope>(value) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    validate_envelope(
        registration,
        &EnvelopeIdentity {
            schema_version: document.schema_version,
            family: document.artifact_family,
            fixture_id: &document.fixture_id,
            dialect: document.dialect,
            kind: document.artifact_kind,
        },
        (dialect, kind),
        issues,
    );
    if !document.payload.is_object() {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    }
    scan_value(
        registry,
        registration,
        &document.payload,
        RuleContext {
            dialect,
            kind,
            assistant_message: false,
            role: if kind == ProtocolArtifactKind::Request {
                ProtocolObjectRole::RequestPayload
            } else {
                ProtocolObjectRole::Other
            },
        },
        None,
        issues,
    );
}

fn validate_sse(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    bytes: &[u8],
    dialect: ProtocolDialect,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    let mut lines = text.lines();
    let Some(metadata_line) = lines.next() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    let Some(metadata_json) = metadata_line.strip_prefix(SSE_METADATA_PREFIX) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    let Ok(metadata_value) = decode_strict_json::<Value>(metadata_json.as_bytes()) else {
        issues.push(issue(RedactionCode::InvalidJson));
        return;
    };
    let Ok(metadata) = serde_json::from_value::<StreamEnvelope>(metadata_value) else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    validate_envelope(
        registration,
        &EnvelopeIdentity {
            schema_version: metadata.schema_version,
            family: metadata.artifact_family,
            fixture_id: &metadata.fixture_id,
            dialect: metadata.dialect,
            kind: metadata.artifact_kind,
        },
        (dialect, ProtocolArtifactKind::Stream),
        issues,
    );

    let mut event_name: Option<String> = None;
    let mut event_count = 0_u64;
    for line in lines {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            let value = value.trim();
            validate_protocol_type(registry, registration, value, dialect, issues);
            event_name = Some(value.to_owned());
            continue;
        }
        if let Some(value) = line.strip_prefix("id:") {
            validate_string_rule(
                registry,
                registration,
                value.trim(),
                StringRule::Synthetic(SyntheticPurpose::Generic),
                issues,
            );
            continue;
        }
        if let Some(value) = line.strip_prefix("retry:") {
            if value.trim().parse::<u64>().is_err() {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            issues.push(issue(RedactionCode::SchemaMismatch));
            continue;
        };
        let Ok(value) = decode_strict_json::<Value>(data.trim().as_bytes()) else {
            issues.push(issue(RedactionCode::InvalidJson));
            continue;
        };
        let Some(event_type) = value
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str)
        else {
            issues.push(issue(RedactionCode::SchemaMismatch));
            continue;
        };
        if event_name.as_deref() != Some(event_type) {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
        scan_value(
            registry,
            registration,
            &value,
            RuleContext {
                dialect,
                kind: ProtocolArtifactKind::Stream,
                assistant_message: false,
                role: ProtocolObjectRole::Event,
            },
            None,
            issues,
        );
        event_count += 1;
        event_name = None;
    }
    if event_count == 0 || event_name.is_some() {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

fn validate_envelope(
    registration: &ArtifactRegistration,
    identity: &EnvelopeIdentity<'_>,
    expected: (ProtocolDialect, ProtocolArtifactKind),
    issues: &mut Vec<ArtifactIssue>,
) {
    if identity.schema_version != ArtifactFamily::ProtocolFixture.schema_version() {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
    if identity.family != ArtifactFamily::ProtocolFixture {
        issues.push(issue(RedactionCode::ArtifactFamilyMismatch));
    }
    if identity.fixture_id != registration.id() {
        issues.push(issue(RedactionCode::ArtifactIdentityMismatch));
    }
    if (identity.dialect, identity.kind) != expected {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
