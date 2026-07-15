use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry, digest_bytes};
use serde_json::{Value, json};

pub(super) type TestResult<T> = Result<T, Box<dyn Error>>;

pub(super) const CONTRACT_PINS: &str = "crates/norn/testdata/openai_responses/contract-pins.json";
pub(super) const BACKEND_MATRIX: &str =
    "crates/norn/testdata/openai_responses/backend-state-matrix.json";
pub(super) const INDEX: &str = "crates/norn/testdata/openai_responses/index.json";
pub(super) const PUBLIC_MANIFEST: &str =
    "crates/norn/testdata/openai_responses/public/manifest.json";
pub(super) const CODEX_MANIFEST: &str = "crates/norn/testdata/openai_responses/codex/manifest.json";
pub(super) const PUBLIC_REQUEST: &str =
    "crates/norn/testdata/openai_responses/public/requests/responses-role-authority.json";
pub(super) const TRACEABILITY: &str = "docs/reviews/evidence/p1/finding-traceability.jsonl";

const FIXTURE_ROOT: &str = "crates/norn/testdata/openai_responses";
const PUBLIC_CONTRACT_MANIFEST: &str = "policy/contracts/openai-responses-v1/manifest.json";
const PUBLIC_CONTRACT_FILES: [&str; 7] = [
    PUBLIC_CONTRACT_MANIFEST,
    "policy/contracts/openai-responses-v1/contract.schema.json",
    "policy/contracts/openai-responses-v1/inventories.json",
    "policy/contracts/openai-responses-v1/request-graph.json",
    "policy/contracts/openai-responses-v1/response-graph.json",
    "policy/contracts/openai-responses-v1/source-discrepancies.json",
    "policy/contracts/openai-responses-v1/sse-events.json",
];

pub(super) struct Corpus {
    entries: BTreeMap<String, Vec<u8>>,
}

impl Corpus {
    pub(super) fn valid() -> TestResult<Self> {
        let root = repository_root()?;
        let mut entries = BTreeMap::new();
        load_tree(&root, &root.join(FIXTURE_ROOT), &mut entries)?;
        for path in PUBLIC_CONTRACT_FILES {
            entries.insert(path.to_owned(), std::fs::read(root.join(path))?);
        }
        entries.insert(
            TRACEABILITY.to_owned(),
            std::fs::read(root.join(TRACEABILITY))?,
        );
        if entries.len() != 52 {
            return Err(io::Error::other("real Responses test corpus is incomplete").into());
        }
        Ok(Self { entries })
    }

    pub(super) fn snapshot(&self) -> TestResult<OwnedSnapshot> {
        self.snapshot_with_order(false)
    }

    pub(super) fn reversed_snapshot(&self) -> TestResult<OwnedSnapshot> {
        self.snapshot_with_order(true)
    }

    pub(super) fn bytes(&self, path: &str) -> TestResult<&[u8]> {
        self.entries
            .get(path)
            .map(Vec::as_slice)
            .ok_or_else(|| io::Error::other("test corpus path is missing").into())
    }

    pub(super) fn replace(&mut self, path: &str, bytes: Vec<u8>) {
        self.entries.insert(path.to_owned(), bytes);
    }

    pub(super) fn insert(&mut self, path: &str, bytes: Vec<u8>) {
        self.entries.insert(path.to_owned(), bytes);
    }

    pub(super) fn remove(&mut self, path: &str) {
        self.entries.remove(path);
    }

    pub(super) fn cross_public_fixture_dialect(&mut self) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let registration = registration_mut(&mut document, PUBLIC_REQUEST)?;
        registration.insert("dialect".to_owned(), Value::String("codex".to_owned()));
        self.replace_public_manifest(&document)
    }

    pub(super) fn replace_public_request_and_repin(&mut self, bytes: Vec<u8>) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let registration = registration_mut(&mut document, PUBLIC_REQUEST)?;
        registration.insert("bytes".to_owned(), Value::from(bytes.len()));
        registration.insert(
            "sha256".to_owned(),
            Value::String(digest_bytes(&bytes).to_string()),
        );
        self.replace(PUBLIC_REQUEST, bytes);
        self.replace_public_manifest(&document)
    }

    pub(super) fn replace_public_finding(&mut self, finding_id: &str) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let findings = registration_mut(&mut document, PUBLIC_REQUEST)?
            .get_mut("finding_ids")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| io::Error::other("test public finding list is missing"))?;
        let Some(finding) = findings.first_mut() else {
            return Err(io::Error::other("test public finding is missing").into());
        };
        *finding = Value::String(finding_id.to_owned());
        self.replace_public_manifest(&document)
    }

    pub(super) fn replace_public_source(&mut self, source: &str) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let sources = registration_mut(&mut document, PUBLIC_REQUEST)?
            .get_mut("source_references")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| io::Error::other("test public source list is missing"))?;
        let Some(first) = sources.first_mut() else {
            return Err(io::Error::other("test public source is missing").into());
        };
        *first = Value::String(source.to_owned());
        self.replace_public_manifest(&document)
    }

    pub(super) fn remove_planned_public_fixture(&mut self) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let fixtures = fixture_array_mut(&mut document)?;
        let position = fixtures
            .iter()
            .position(|fixture| {
                fixture.get("fixture_path").and_then(Value::as_str) == Some(PUBLIC_REQUEST)
            })
            .ok_or_else(|| io::Error::other("planned test fixture is missing"))?;
        fixtures.remove(position);
        self.remove(PUBLIC_REQUEST);
        self.replace_public_manifest(&document)
    }

    pub(super) fn declare_hostile_public_fixture(
        &mut self,
        fixture_id: &str,
        fixture_path: &str,
    ) -> TestResult<()> {
        let mut document = self.public_manifest_value()?;
        let registration = fixture_array_mut(&mut document)?
            .first_mut()
            .and_then(Value::as_object_mut)
            .ok_or_else(|| io::Error::other("test public fixture is missing"))?;
        registration.insert("id".to_owned(), Value::String(fixture_id.to_owned()));
        registration.insert(
            "fixture_path".to_owned(),
            Value::String(fixture_path.to_owned()),
        );
        self.replace_public_manifest(&document)
    }

    pub(super) fn revise_matrix_text(&mut self) -> TestResult<()> {
        let bytes = self.bytes(BACKEND_MATRIX)?;
        let mut document: Value = serde_json::from_slice(bytes)?;
        let treatment = document
            .get_mut("payload")
            .and_then(|payload| payload.get_mut("entries"))
            .and_then(Value::as_array_mut)
            .and_then(|entries| entries.first_mut())
            .and_then(Value::as_object_mut)
            .and_then(|entry| entry.get_mut("p1_treatment"))
            .ok_or_else(|| io::Error::other("test matrix treatment is missing"))?;
        *treatment = Value::String("norn-synthetic-revised-p1-treatment".to_owned());
        self.replace(BACKEND_MATRIX, serde_json::to_vec(&document)?);
        Ok(())
    }

    fn public_manifest_value(&self) -> TestResult<Value> {
        Ok(serde_json::from_slice(self.bytes(PUBLIC_MANIFEST)?)?)
    }

    fn replace_public_manifest(&mut self, document: &Value) -> TestResult<()> {
        self.replace(PUBLIC_MANIFEST, serde_json::to_vec(document)?);
        self.rebuild_index()
    }

    fn rebuild_index(&mut self) -> TestResult<()> {
        let payload = json!({
            "public_manifest": file_reference(PUBLIC_MANIFEST, self.bytes(PUBLIC_MANIFEST)?),
            "codex_manifest": file_reference(CODEX_MANIFEST, self.bytes(CODEX_MANIFEST)?),
        });
        let index = control_envelope("openai-responses-index-v1", "corpus", "index", &payload)?;
        self.replace(INDEX, index);
        Ok(())
    }

    fn snapshot_with_order(&self, reverse: bool) -> TestResult<OwnedSnapshot> {
        let mut rows: Vec<_> = self.entries.iter().collect();
        if reverse {
            rows.reverse();
        }
        let mut entries = Vec::with_capacity(self.entries.len());
        for (path, bytes) in rows {
            entries.push((
                RepositoryPath::parse(path)?,
                SnapshotEntry::regular(bytes.clone()),
            ));
        }
        Ok(OwnedSnapshot::try_from_entries(entries)?)
    }
}

fn fixture_array_mut(document: &mut Value) -> TestResult<&mut Vec<Value>> {
    document
        .get_mut("payload")
        .and_then(|payload| payload.get_mut("fixtures"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| io::Error::other("test public fixture list is missing").into())
}

fn registration_mut<'a>(
    document: &'a mut Value,
    fixture_path: &str,
) -> TestResult<&'a mut serde_json::Map<String, Value>> {
    fixture_array_mut(document)?
        .iter_mut()
        .find(|fixture| fixture.get("fixture_path").and_then(Value::as_str) == Some(fixture_path))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("test public request registration is missing").into())
}

fn load_tree(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<String, Vec<u8>>,
) -> TestResult<()> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("test corpus path is not UTF-8"))?
                    .to_owned();
                entries.insert(relative, std::fs::read(path)?);
            } else {
                return Err(io::Error::other("test corpus contains a special entry").into());
            }
        }
    }
    Ok(())
}

fn repository_root() -> TestResult<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("crate is not beneath a repository root").into())
}

fn control_envelope(id: &str, dialect: &str, kind: &str, payload: &Value) -> TestResult<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "schema_version": 1,
        "artifact_family": "protocol_fixture",
        "fixture_id": id,
        "dialect": dialect,
        "artifact_kind": kind,
        "payload": payload,
    }))?)
}

fn file_reference(path: &str, bytes: &[u8]) -> Value {
    json!({
        "path": path,
        "bytes": bytes.len(),
        "sha256": digest_bytes(bytes).to_string(),
    })
}
