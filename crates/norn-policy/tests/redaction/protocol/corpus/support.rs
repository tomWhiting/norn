use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io;
use std::path::Path;

use norn_policy::redaction::{
    ArtifactFamily, ArtifactRegistration, RedactionCode, RedactionRegistry, RedactionViolation,
    SentinelClass, SyntheticPurpose, SyntheticRegistration, validate_retained_artifacts,
};
use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry, digest_bytes};
use serde_json::Value;

use super::cases::{CASES, CorpusCase};

const SSE_METADATA_PREFIX: &str = ": norn-fixture-v1 ";
const GENERATOR_ID: &str = "p1-responses-fixture-corpus-v1";
const GENERATOR_PATH: &str = "docs/reviews/evidence/p1/responses_fixture_generate.py";

pub(super) struct CorpusFixture {
    pub(super) registry: RedactionRegistry,
    pub(super) snapshot: OwnedSnapshot,
    pub(super) purpose_counts: BTreeMap<SyntheticPurpose, usize>,
}

pub(super) fn fixture() -> Result<CorpusFixture, Box<dyn Error>> {
    let mut values_by_path = BTreeMap::new();
    let mut all_values = BTreeSet::new();
    for case in CASES {
        let values = synthetic_values(case)?;
        all_values.extend(values.iter().cloned());
        values_by_path.insert(case.path, values);
    }

    let mut value_ids = BTreeMap::new();
    let mut synthetics = Vec::new();
    let mut purpose_counts = BTreeMap::new();
    let provenance = RepositoryPath::parse(GENERATOR_PATH)?;
    for (offset, value) in all_values.into_iter().enumerate() {
        let ordinal = offset
            .checked_add(1)
            .ok_or_else(|| io::Error::other("synthetic ordinal overflow"))?;
        let id = format!("corpus-synthetic-{ordinal:03}");
        let purpose = synthetic_purpose(&value);
        *purpose_counts.entry(purpose).or_insert(0) += 1;
        synthetics.push(SyntheticRegistration::new(
            &id,
            &value,
            GENERATOR_ID,
            provenance.clone(),
            purpose,
            SentinelClass::NonReusableFixtureV1,
        )?);
        value_ids.insert(value, id);
    }

    let mut rows = Vec::new();
    for case in CASES {
        let path = RepositoryPath::parse(case.path)?;
        let fixture_id = fixture_id(case)?;
        let ids = values_by_path
            .get(case.path)
            .ok_or_else(|| io::Error::other("corpus path has no synthetic inventory"))?
            .iter()
            .map(|value| {
                value_ids
                    .get(value)
                    .cloned()
                    .ok_or_else(|| io::Error::other("synthetic value has no registry ID"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let artifact = ArtifactRegistration::new(
            fixture_id,
            path.clone(),
            ArtifactFamily::ProtocolFixture,
            digest_bytes(case.bytes),
            ids,
            Vec::new(),
        )?;
        rows.push((path, artifact, SnapshotEntry::regular(case.bytes.to_vec())));
    }
    rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let artifacts = rows
        .iter()
        .map(|(_, artifact, _)| artifact.clone())
        .collect();
    let registry = RedactionRegistry::new(artifacts, synthetics)?;
    let entries = rows.into_iter().map(|(path, _, entry)| (path, entry));
    Ok(CorpusFixture {
        registry,
        snapshot: OwnedSnapshot::try_from_entries(entries)?,
        purpose_counts,
    })
}

pub(super) fn codes(registry: &RedactionRegistry, snapshot: &OwnedSnapshot) -> Vec<RedactionCode> {
    validate_retained_artifacts(registry, snapshot)
        .iter()
        .map(RedactionViolation::code)
        .collect()
}

pub(super) fn assert_has(codes: &[RedactionCode], expected: RedactionCode) {
    assert!(
        codes.contains(&expected),
        "missing {expected:?} in {codes:?}"
    );
}

pub(super) fn replace_case(
    snapshot: &OwnedSnapshot,
    case: &CorpusCase,
    bytes: Vec<u8>,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let path = RepositoryPath::parse(case.path)?;
    replace_entry(snapshot, &path, Some(SnapshotEntry::regular(bytes)))
}

pub(super) fn relocate_case(
    snapshot: &OwnedSnapshot,
    case: &CorpusCase,
    ordinal: usize,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let path = RepositoryPath::parse(case.path)?;
    let extension = if is_sse(case) { "sse" } else { "json" };
    let relocated = RepositoryPath::parse(format!(
        "crates/norn/testdata/openai_responses/relocated/artifact-{ordinal:02}.{extension}"
    ))?;
    let without = replace_entry(snapshot, &path, None)?;
    let entries = without
        .iter()
        .map(|(candidate, entry)| (candidate.clone(), entry.clone()))
        .chain([(relocated, SnapshotEntry::regular(case.bytes.to_vec()))]);
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn mutate_envelope(
    case: &CorpusCase,
    field: &str,
    value: Value,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if is_sse(case) {
        mutate_sse_metadata(case.bytes, field, value)
    } else {
        let mut document = serde_json::from_slice::<Value>(case.bytes)?;
        let object = document
            .as_object_mut()
            .ok_or_else(|| io::Error::other("corpus envelope is not an object"))?;
        object.insert(field.to_owned(), value);
        Ok(serde_json::to_vec(&document)?)
    }
}

pub(super) fn mutate_json(
    case: &CorpusCase,
    mutate: impl FnOnce(&mut Value) -> Result<(), io::Error>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if is_sse(case) {
        return Err(io::Error::other("JSON mutation requested for SSE fixture").into());
    }
    let mut document = serde_json::from_slice::<Value>(case.bytes)?;
    mutate(&mut document)?;
    Ok(serde_json::to_vec(&document)?)
}

pub(super) fn case(path_suffix: &str) -> Result<&'static CorpusCase, io::Error> {
    CASES
        .iter()
        .find(|case| case.path.ends_with(path_suffix))
        .ok_or_else(|| io::Error::other("corpus case is missing"))
}

fn synthetic_values(case: &CorpusCase) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut values = BTreeSet::new();
    for document in documents(case)? {
        collect_synthetic_values(&document, &mut values);
    }
    Ok(values)
}

fn fixture_id(case: &CorpusCase) -> Result<String, Box<dyn Error>> {
    documents(case)?
        .first()
        .and_then(|document| document.get("fixture_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("corpus envelope has no fixture ID").into())
}

fn documents(case: &CorpusCase) -> Result<Vec<Value>, Box<dyn Error>> {
    if !is_sse(case) {
        return Ok(vec![serde_json::from_slice(case.bytes)?]);
    }
    let text = std::str::from_utf8(case.bytes)?;
    let mut documents = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix(SSE_METADATA_PREFIX) {
            documents.push(serde_json::from_str(value)?);
        } else if let Some(value) = line.strip_prefix("data:") {
            documents.push(serde_json::from_str(value.trim())?);
        }
    }
    Ok(documents)
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
            for value in values {
                collect_synthetic_values(value, output);
            }
        }
        Value::String(value) => insert_synthetic(value, output),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

pub(super) fn is_sse(case: &CorpusCase) -> bool {
    Path::new(case.path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sse"))
}

fn insert_synthetic(value: &str, output: &mut BTreeSet<String>) {
    if value.starts_with("norn-synthetic-") {
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

fn replace_entry(
    snapshot: &OwnedSnapshot,
    path: &RepositoryPath,
    replacement: Option<SnapshotEntry>,
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let mut replacement = replacement;
    let entries = snapshot.iter().filter_map(|(candidate, entry)| {
        if candidate == path {
            replacement.take().map(|entry| (candidate.clone(), entry))
        } else {
            Some((candidate.clone(), entry.clone()))
        }
    });
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn mutate_sse_metadata(bytes: &[u8], field: &str, value: Value) -> Result<Vec<u8>, Box<dyn Error>> {
    let text = std::str::from_utf8(bytes)?;
    let mut lines = text.lines();
    let first = lines
        .next()
        .and_then(|line| line.strip_prefix(SSE_METADATA_PREFIX))
        .ok_or_else(|| io::Error::other("SSE metadata is missing"))?;
    let mut metadata = serde_json::from_str::<Value>(first)?;
    metadata
        .as_object_mut()
        .ok_or_else(|| io::Error::other("SSE metadata is not an object"))?
        .insert(field.to_owned(), value);
    let mut output = format!(
        "{SSE_METADATA_PREFIX}{}\n",
        serde_json::to_string(&metadata)?
    );
    for line in lines {
        output.push_str(line);
        output.push('\n');
    }
    Ok(output.into_bytes())
}
