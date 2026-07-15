use std::error::Error;
use std::io;

use norn_policy::SnapshotEntry;
use norn_policy::redaction::RedactionCode;
use serde_json::{Map, Value, json};

use super::support::{assert_has, bytes, codes, fixture, replace};

mod corpus;

#[test]
fn accepts_typed_content_cache_controls_and_nullable_fields() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let observed = codes(&fixture.registry, &fixture.snapshot);
    assert!(observed.is_empty(), "unexpected codes: {observed:?}");
    Ok(())
}

#[test]
fn sensitive_scalar_fields_require_exact_purpose_sentinels() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let cases = [
        (
            "access_token",
            json!("norn-synthetic-prompt-001"),
            RedactionCode::SyntheticMetadataMismatch,
        ),
        (
            "access_token",
            json!("synthetic-literal"),
            RedactionCode::ProhibitedField,
        ),
        (
            "previous_response_id",
            json!("synthetic-literal"),
            RedactionCode::ReusableState,
        ),
        ("access_token", Value::Null, RedactionCode::ProhibitedField),
    ];
    for (key, value, expected) in cases {
        let changed = change_payload_field(
            bytes(&fixture.snapshot, &fixture.paths.protocol)?,
            key,
            value,
        )?;
        let snapshot = replace(
            &fixture.snapshot,
            &fixture.paths.protocol,
            SnapshotEntry::regular(changed),
        )?;
        assert_has(&codes(&fixture.registry, &snapshot), expected);
    }
    Ok(())
}

#[test]
fn rejects_dangerous_values_without_embedding_test_secrets() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let cases = [
        (
            ["bear", "er ", "synthetic-value"].concat(),
            RedactionCode::DangerousShape,
        ),
        (
            ["s", "k-", "synthetic-value"].concat(),
            RedactionCode::DangerousShape,
        ),
        (
            ["ey", "Jsynthe", ".", "ijklmnop", ".", "qrstuvwx"].concat(),
            RedactionCode::DangerousShape,
        ),
        (
            ["fixture", "@", "private", ".", "invalid"].concat(),
            RedactionCode::DangerousShape,
        ),
        (
            ["/", "Users", "/", "fixture", "/", "private"].concat(),
            RedactionCode::AbsolutePath,
        ),
        (
            ["norn", "-private-", "prompt-value"].concat(),
            RedactionCode::DangerousShape,
        ),
        (
            ["resp", "_", "synthetic-state-value"].concat(),
            RedactionCode::ReusableState,
        ),
    ];
    for (value, expected) in cases {
        let changed = change_payload_field(
            bytes(&fixture.snapshot, &fixture.paths.protocol)?,
            "model",
            Value::String(value),
        )?;
        let snapshot = replace(
            &fixture.snapshot,
            &fixture.paths.protocol,
            SnapshotEntry::regular(changed),
        )?;
        assert_has(&codes(&fixture.registry, &snapshot), expected);
    }
    Ok(())
}

#[test]
fn rejects_unknown_envelope_and_payload_fields() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let protocol = bytes(&fixture.snapshot, &fixture.paths.protocol)?;
    let mut unknown_envelope = serde_json::from_slice::<Value>(protocol)?;
    unknown_envelope
        .as_object_mut()
        .ok_or_else(|| io::Error::other("protocol envelope missing"))?
        .insert("surprise".to_owned(), Value::Null);
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(serde_json::to_vec(&unknown_envelope)?),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );

    let unknown_payload = change_payload_field(protocol, "surprise", json!(1))?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(unknown_payload),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );
    Ok(())
}

#[test]
fn pinned_discriminators_are_closed_without_truncating_the_contract() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture()?;
    let uncommon = change_payload_field(
        bytes(&fixture.snapshot, &fixture.paths.protocol)?,
        "type",
        json!("response.audio.delta"),
    )?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(uncommon),
    )?;
    let observed = codes(&fixture.registry, &snapshot);
    assert!(!observed.contains(&RedactionCode::SchemaMismatch));

    let unknown = change_payload_field(
        bytes(&fixture.snapshot, &fixture.paths.protocol)?,
        "type",
        json!("response.future.event"),
    )?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(unknown),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );

    let unknown_include = change_payload_field(
        bytes(&fixture.snapshot, &fixture.paths.protocol)?,
        "include",
        json!(["future.private.include"]),
    )?;
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(unknown_include),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );
    Ok(())
}

#[test]
fn json_schema_rejects_external_references_and_private_defaults() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let protocol = bytes(&fixture.snapshot, &fixture.paths.protocol)?;

    let mut external_reference = serde_json::from_slice::<Value>(protocol)?;
    let parameters = tool_parameters_object(&mut external_reference)?;
    let property = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut("norn-synthetic-generic-001"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("fixture schema property missing"))?;
    property.insert(
        "$ref".to_owned(),
        json!("https://schemas.example.invalid/private"),
    );
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(serde_json::to_vec(&external_reference)?),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::SchemaMismatch,
    );

    let mut private_default = serde_json::from_slice::<Value>(protocol)?;
    let parameters = tool_parameters_object(&mut private_default)?;
    let definition = parameters
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .and_then(|definitions| definitions.get_mut("norn-synthetic-generic-001"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("fixture schema definition missing"))?;
    definition.insert("default".to_owned(), json!("private-project"));
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::regular(serde_json::to_vec(&private_default)?),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::ProhibitedField,
    );
    Ok(())
}

#[test]
fn machine_shaped_private_strings_and_local_urls_are_not_safe_literals()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let cases = [
        (
            "model",
            json!("private-project"),
            RedactionCode::ProhibitedField,
        ),
        (
            "filename",
            json!("private-report.txt"),
            RedactionCode::ProhibitedField,
        ),
        (
            "url",
            json!("http://127.0.0.1/private"),
            RedactionCode::UnregisteredString,
        ),
    ];
    for (key, value, expected) in cases {
        let changed = change_payload_field(
            bytes(&fixture.snapshot, &fixture.paths.protocol)?,
            key,
            value,
        )?;
        let snapshot = replace(
            &fixture.snapshot,
            &fixture.paths.protocol,
            SnapshotEntry::regular(changed),
        )?;
        assert_has(&codes(&fixture.registry, &snapshot), expected);
    }
    Ok(())
}

#[test]
fn evidence_summaries_reject_fields_contract_schemas_may_name() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let observed = codes(&fixture.registry, &fixture.snapshot);
    assert!(observed.is_empty(), "unexpected codes: {observed:?}");

    let mut distribution =
        serde_json::from_slice::<Value>(bytes(&fixture.snapshot, &fixture.paths.distribution)?)?;
    distribution
        .as_object_mut()
        .ok_or_else(|| io::Error::other("distribution envelope missing"))?
        .insert(
            "access_token".to_owned(),
            json!("norn-synthetic-credential-001"),
        );
    let snapshot = replace(
        &fixture.snapshot,
        &fixture.paths.distribution,
        SnapshotEntry::regular(serde_json::to_vec(&distribution)?),
    )?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::ProhibitedField,
    );
    Ok(())
}

fn change_payload_field(bytes: &[u8], key: &str, value: Value) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut document = serde_json::from_slice::<Value>(bytes)?;
    let payload = payload_object(&mut document)?;
    payload.insert(key.to_owned(), value);
    Ok(serde_json::to_vec(&document)?)
}

fn payload_object(document: &mut Value) -> Result<&mut Map<String, Value>, io::Error> {
    document
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("protocol payload missing"))
}

fn tool_parameters_object(document: &mut Value) -> Result<&mut Map<String, Value>, io::Error> {
    payload_object(document)?
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.first_mut())
        .and_then(|tool| tool.get_mut("parameters"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("protocol tool parameters missing"))
}
