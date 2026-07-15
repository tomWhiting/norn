//! Independent structural checks for the checked-in Responses fixture corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};

const METADATA_PREFIX: &str = ": norn-fixture-v1 ";
const CODEX_SOURCE_PREFIX: &str = "https://github.com/openai/codex/blob/\
    0396f99cf1a27fc87dd12d23403b25e840b6ecbd/";

type TestResult<T> = Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    schema_version: u32,
    artifact_family: String,
    fixture_id: String,
    dialect: String,
    artifact_kind: String,
    payload: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPayload {
    fixtures: Vec<Registration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    id: String,
    dialect: String,
    artifact_kind: String,
    fixture_path: String,
    bytes: u64,
    sha256: String,
    source_references: Vec<String>,
    categories: Vec<String>,
    finding_ids: Vec<String>,
    owner_phase: String,
    expectation_class: String,
    current_observation: String,
    target_assertions: Vec<String>,
    secret_profile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamIdentity {
    schema_version: u32,
    artifact_family: String,
    fixture_id: String,
    dialect: String,
    artifact_kind: String,
}

#[test]
fn manifests_bind_the_complete_two_dialect_corpus() -> TestResult<()> {
    let root = repository_root()?;
    let fixture_root = root.join("crates/norn/testdata/openai_responses");
    let mut registered_paths = BTreeSet::new();
    let mut registered_ids = BTreeSet::new();
    let mut counts = Vec::new();

    for dialect in ["public", "codex"] {
        let manifest_path = fixture_root.join(dialect).join("manifest.json");
        let manifest: Envelope<ManifestPayload> = decode(&std::fs::read(manifest_path)?)?;
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.artifact_family, "protocol_fixture");
        assert_eq!(manifest.dialect, dialect);
        assert_eq!(manifest.artifact_kind, "manifest");
        assert_eq!(
            manifest.fixture_id,
            format!("openai-responses-{dialect}-manifest-v1")
        );
        assert!(
            manifest
                .payload
                .fixtures
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
        );
        counts.push(manifest.payload.fixtures.len());
        for registration in manifest.payload.fixtures {
            verify_registration(&root, dialect, &registration)?;
            assert!(registered_paths.insert(registration.fixture_path));
            assert!(registered_ids.insert(registration.id));
        }
    }

    assert_eq!(counts, [26, 13]);
    assert_eq!(registered_ids.len(), 39);
    assert_eq!(
        registered_paths,
        concrete_fixture_paths(&root, &fixture_root)?
    );
    assert_eq!(all_files(&root, &fixture_root)?.len(), 44);
    Ok(())
}

#[test]
fn structural_decoder_rejects_duplicate_object_members() {
    let result = decode::<Value>(br#"{"schema_version":1,"schema_version":1}"#);
    assert!(result.is_err());
}

fn verify_registration(root: &Path, dialect: &str, registration: &Registration) -> TestResult<()> {
    assert_eq!(registration.dialect, dialect);
    assert!(matches!(
        registration.artifact_kind.as_str(),
        "request" | "stream" | "transport"
    ));
    assert!(registration.fixture_path.contains(&format!("/{dialect}/")));
    assert_eq!(registration.finding_ids.len(), 1);
    assert_eq!(
        registration
            .categories
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        registration.categories.len()
    );
    assert!(registration.owner_phase.starts_with('P'));
    assert!(matches!(
        registration.expectation_class.as_str(),
        "baseline_red" | "contract_target"
    ));
    assert!(
        registration
            .current_observation
            .starts_with("norn-synthetic-")
    );
    assert!(
        registration
            .target_assertions
            .iter()
            .all(|value| value.starts_with("norn-synthetic-"))
    );
    assert_eq!(registration.secret_profile, "registered_synthetic");
    assert!(
        registration
            .source_references
            .iter()
            .all(|source| official_source(source))
    );

    let value = std::fs::read(root.join(&registration.fixture_path))?;
    assert_eq!(u64::try_from(value.len())?, registration.bytes);
    assert_eq!(hex_digest(&value), registration.sha256);
    if registration.artifact_kind == "stream" {
        verify_stream_identity(registration, &value)
    } else {
        verify_json_identity(registration, &value)
    }
}

fn verify_json_identity(registration: &Registration, value: &[u8]) -> TestResult<()> {
    let fixture: Envelope<Value> = decode(value)?;
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.artifact_family, "protocol_fixture");
    assert_eq!(fixture.fixture_id, registration.id);
    assert_eq!(fixture.dialect, registration.dialect);
    assert_eq!(fixture.artifact_kind, registration.artifact_kind);
    assert!(fixture.payload.is_object());
    Ok(())
}

fn verify_stream_identity(registration: &Registration, value: &[u8]) -> TestResult<()> {
    let text = std::str::from_utf8(value)?;
    let mut lines = text.lines();
    let metadata = lines
        .next()
        .and_then(|line| line.strip_prefix(METADATA_PREFIX))
        .ok_or_else(|| io::Error::other("stream metadata is missing"))?;
    let identity: StreamIdentity = decode(metadata.as_bytes())?;
    assert_eq!(identity.schema_version, 1);
    assert_eq!(identity.artifact_family, "protocol_fixture");
    assert_eq!(identity.fixture_id, registration.id);
    assert_eq!(identity.dialect, registration.dialect);
    assert_eq!(identity.artifact_kind, "stream");

    let mut pending_event = None;
    let mut event_count = 0_usize;
    for line in lines {
        if let Some(event_name) = line.strip_prefix("event: ") {
            assert!(pending_event.replace(event_name).is_none());
        } else if let Some(data) = line.strip_prefix("data: ") {
            let event: Value = decode(data.as_bytes())?;
            assert_eq!(
                event.get("type").and_then(Value::as_str),
                pending_event.take()
            );
            event_count += 1;
        } else {
            assert!(line.is_empty());
        }
    }
    assert!(pending_event.is_none());
    assert!(event_count != 0);
    Ok(())
}

fn official_source(source: &str) -> bool {
    matches!(
        source,
        "https://api.openai.com/v1/responses"
            | "https://api.openai.com/v1/responses/compact"
            | "https://developers.openai.com/api/reference/resources/responses/streaming-events"
            | "https://developers.openai.com/api/docs/guides/text"
            | "https://developers.openai.com/api/docs/guides/reasoning"
            | "https://developers.openai.com/api/docs/guides/conversation-state"
            | "https://developers.openai.com/api/docs/guides/compaction"
            | "https://developers.openai.com/api/docs/guides/prompt-caching"
            | "https://developers.openai.com/api/docs/guides/tools"
            | "https://developers.openai.com/api/docs/guides/tools-web-search"
            | "https://developers.openai.com/api/docs/guides/function-calling"
    ) || matches!(
        source.strip_prefix(CODEX_SOURCE_PREFIX),
        Some(
            "codex-rs/core/src/client.rs"
                | "codex-rs/codex-api/src/sse/responses.rs"
                | "codex-rs/codex-api/src/common.rs"
                | "codex-rs/protocol/src/models.rs"
                | "codex-rs/login/src/server.rs"
        )
    )
}

fn concrete_fixture_paths(root: &Path, fixture_root: &Path) -> TestResult<BTreeSet<String>> {
    Ok(all_files(root, fixture_root)?
        .into_iter()
        .filter(|path| {
            path.contains("/requests/")
                || path.contains("/streams/")
                || path.contains("/transport/")
        })
        .collect())
}

fn all_files(root: &Path, fixture_root: &Path) -> TestResult<BTreeSet<String>> {
    let mut pending = vec![fixture_root.to_path_buf()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("fixture path is not UTF-8"))?
                    .to_owned();
                files.insert(relative);
            } else {
                return Err(io::Error::other("fixture tree contains a special entry").into());
            }
        }
    }
    Ok(files)
}

fn repository_root() -> TestResult<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("crate is not beneath a repository root").into())
}

fn decode<T: DeserializeOwned>(value: &[u8]) -> TestResult<T> {
    let mut deserializer = serde_json::Deserializer::from_slice(value);
    let strict = StrictValue::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(serde_json::from_value(strict.0)?)
}

fn hex_digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object member names")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom("floating-point JSON numbers are not admitted"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
            if values.insert(key, value.0).is_some() {
                return Err(serde::de::Error::custom("duplicate JSON object member"));
            }
        }
        Ok(StrictValue(Value::Object(values.into_iter().collect())))
    }
}
