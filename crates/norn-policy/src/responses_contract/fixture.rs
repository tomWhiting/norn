use serde::Deserialize;
use serde_json::Value;

use crate::strict_json::decode_strict_json;

use super::model::{ArtifactFamily, ArtifactKind, Dialect, FixtureRegistration};

const SSE_METADATA_PREFIX: &str = ": norn-fixture-v1 ";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonFixtureEnvelope {
    schema_version: u32,
    artifact_family: ArtifactFamily,
    fixture_id: String,
    dialect: Dialect,
    artifact_kind: ArtifactKind,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamFixtureEnvelope {
    schema_version: u32,
    artifact_family: ArtifactFamily,
    fixture_id: String,
    dialect: Dialect,
    artifact_kind: ArtifactKind,
}

pub(super) fn fixture_matches(registration: &FixtureRegistration, bytes: &[u8]) -> bool {
    match registration.artifact_kind {
        ArtifactKind::Request | ArtifactKind::Transport => json_matches(registration, bytes),
        ArtifactKind::Stream => stream_matches(registration, bytes),
        ArtifactKind::BackendStateMatrix
        | ArtifactKind::ContractPins
        | ArtifactKind::Index
        | ArtifactKind::Manifest => false,
    }
}

fn json_matches(registration: &FixtureRegistration, bytes: &[u8]) -> bool {
    let Ok(envelope) = decode_strict_json::<JsonFixtureEnvelope>(bytes) else {
        return false;
    };
    envelope.schema_version == 1
        && envelope.artifact_family == ArtifactFamily::ProtocolFixture
        && envelope.fixture_id == registration.id
        && envelope.dialect == registration.dialect
        && envelope.artifact_kind == registration.artifact_kind
        && envelope.payload.is_object()
}

fn stream_matches(registration: &FixtureRegistration, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut lines = text.lines();
    let Some(metadata) = lines
        .next()
        .and_then(|line| line.strip_prefix(SSE_METADATA_PREFIX))
    else {
        return false;
    };
    let Ok(envelope) = decode_strict_json::<StreamFixtureEnvelope>(metadata.as_bytes()) else {
        return false;
    };
    if envelope.schema_version != 1
        || envelope.artifact_family != ArtifactFamily::ProtocolFixture
        || envelope.fixture_id != registration.id
        || envelope.dialect != registration.dialect
        || envelope.artifact_kind != ArtifactKind::Stream
    {
        return false;
    }

    let mut data_rows = 0_usize;
    for line in lines {
        if line.is_empty()
            || line.starts_with(':')
            || line.starts_with("event:")
            || line.starts_with("id:")
            || line.starts_with("retry:")
        {
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return false;
        };
        if decode_strict_json::<Value>(data.trim().as_bytes()).is_err() {
            return false;
        }
        data_rows += 1;
    }
    data_rows != 0
}
