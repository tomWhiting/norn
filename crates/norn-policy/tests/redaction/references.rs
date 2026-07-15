use std::error::Error;
use std::io;

use norn_policy::redaction::{ObservationSource, RedactionCode};
use norn_policy::{SnapshotEntry, digest_bytes};
use serde_json::{Map, Value, json};

use super::support::{assert_has, bytes, codes, fixture, remove, replace};

#[test]
fn observation_rows_cannot_be_omitted_or_reidentified() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let distribution_bytes = bytes(&fixture.snapshot, &fixture.paths.distribution)?;

    let missing = replace_observations(distribution_bytes, json!([]))?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.distribution,
        SnapshotEntry::regular(missing),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::RegisteredValueMissing,
    );

    let changed = change_observation_field(distribution_bytes, "id", json!("changed-row"))?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.distribution,
        SnapshotEntry::regular(changed),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::ObservationMismatch,
    );
    Ok(())
}

#[test]
fn each_observation_tuple_member_is_bound_together() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let mutations = [
        (
            "referenced_path",
            json!("target/p1-gate/evidence/distribution.json"),
        ),
        ("referenced_family", json!("distribution")),
        ("source", json!(ObservationSource::CodexSourcePin)),
        ("synthetic_ids", json!(["account-value"])),
        ("digest", json!(digest_bytes(b"different artifact"))),
    ];
    for (key, value) in mutations {
        let changed = change_observation_field(
            bytes(&fixture.snapshot, &fixture.paths.distribution)?,
            key,
            value,
        )?;
        let snapshot = replace(
            &fixture.snapshot,
            &fixture.paths.distribution,
            SnapshotEntry::regular(changed),
        )?;
        assert_has(
            &codes(&fixture.registry, &snapshot),
            RedactionCode::ObservationMismatch,
        );
    }
    Ok(())
}

#[test]
fn referenced_artifact_must_be_present_in_same_snapshot() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let snapshot = remove(&fixture.snapshot, &fixture.paths.log)?;
    let observed = codes(&fixture.registry, &snapshot);
    assert_has(&observed, RedactionCode::RegisteredArtifactMissing);
    assert_has(&observed, RedactionCode::ReferencedArtifactMissing);
    Ok(())
}

fn replace_observations(bytes: &[u8], value: Value) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut document = serde_json::from_slice::<Value>(bytes)?;
    let object = document
        .as_object_mut()
        .ok_or_else(|| io::Error::other("gate document is not an object"))?;
    object.insert("observations".to_owned(), value);
    Ok(serde_json::to_vec(&document)?)
}

fn change_observation_field(
    bytes: &[u8],
    key: &str,
    value: Value,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut document = serde_json::from_slice::<Value>(bytes)?;
    let row = first_observation(&mut document)?;
    row.insert(key.to_owned(), value);
    Ok(serde_json::to_vec(&document)?)
}

fn first_observation(document: &mut Value) -> Result<&mut Map<String, Value>, io::Error> {
    document
        .get_mut("observations")
        .and_then(Value::as_array_mut)
        .and_then(|rows| rows.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("gate observation missing"))
}
