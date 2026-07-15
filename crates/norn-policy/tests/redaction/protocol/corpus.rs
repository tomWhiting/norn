use std::collections::BTreeSet;
use std::error::Error;
use std::io;

use norn_policy::redaction::{RedactionCode, SyntheticPurpose, validate_retained_artifacts};
use serde_json::{Value, json};

mod cases;
mod support;

use cases::CASES;
use support::{
    assert_has, case, codes, fixture, is_sse, mutate_envelope, mutate_json, relocate_case,
    replace_case,
};

#[test]
fn accepts_all_exact_corpus_artifacts() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let violations = validate_retained_artifacts(&fixture.registry, &fixture.snapshot);
    assert!(
        violations.is_empty(),
        "unexpected corpus violations: {violations:?}"
    );
    assert_eq!(fixture.registry.registered_paths().len(), 44);
    assert_eq!(fixture.snapshot.iter().count(), 44);
    Ok(())
}

#[test]
fn sentinel_inventory_is_exact_and_non_vacuous() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let expected = [
        (SyntheticPurpose::AccountId, 6),
        (SyntheticPurpose::CacheKey, 3),
        (SyntheticPurpose::Credential, 9),
        (SyntheticPurpose::PromptContent, 31),
        (SyntheticPurpose::TurnState, 3),
        (SyntheticPurpose::Generic, 211),
    ];
    assert_eq!(fixture.purpose_counts.len(), expected.len());
    for (purpose, count) in expected {
        assert_eq!(fixture.purpose_counts.get(&purpose).copied(), Some(count));
        assert!(count > 0);
    }
    assert_eq!(fixture.purpose_counts.values().sum::<usize>(), 263);
    let call_stream = std::str::from_utf8(
        case("public/streams/responses-interleaved-duplicate-calls.sse")?.bytes,
    )?;
    for delta in [
        "norn-synthetic-prompt-evt-06-function-delta",
        "norn-synthetic-prompt-evt-06-custom-delta",
    ] {
        assert!(call_stream.contains(delta));
    }
    Ok(())
}

#[test]
fn every_artifact_is_bound_to_its_exact_path_and_envelope_context() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    for (ordinal, corpus_case) in CASES.iter().enumerate() {
        let relocated = relocate_case(&fixture.snapshot, corpus_case, ordinal)?;
        let relocated_codes = codes(&fixture.registry, &relocated);
        assert_has(&relocated_codes, RedactionCode::RegisteredArtifactMissing);
        assert_has(&relocated_codes, RedactionCode::UnregisteredArtifact);

        let dialect = if corpus_case.path.contains("/public/") {
            "codex"
        } else {
            "public"
        };
        let changed = mutate_envelope(corpus_case, "dialect", json!(dialect))?;
        let snapshot = replace_case(&fixture.snapshot, corpus_case, changed)?;
        assert_has(
            &codes(&fixture.registry, &snapshot),
            RedactionCode::SchemaMismatch,
        );

        let kind = if is_sse(corpus_case) {
            "request"
        } else {
            "stream"
        };
        let changed = mutate_envelope(corpus_case, "artifact_kind", json!(kind))?;
        let snapshot = replace_case(&fixture.snapshot, corpus_case, changed)?;
        assert_has(
            &codes(&fixture.registry, &snapshot),
            RedactionCode::SchemaMismatch,
        );
    }
    Ok(())
}

#[test]
fn manifest_machine_fields_and_sources_are_closed() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let manifest = case("public/manifest.json")?;
    for (field, value) in [
        ("id", json!("not-a-fixture-row")),
        ("categories", json!(["too/many/segments"])),
        ("finding_ids", json!(["evt-private"])),
    ] {
        let changed = mutate_json(manifest, |document| {
            first_manifest_row(document)?.insert(field.to_owned(), value);
            Ok(())
        })?;
        let snapshot = replace_case(&fixture.snapshot, manifest, changed)?;
        assert_has(
            &codes(&fixture.registry, &snapshot),
            RedactionCode::SchemaMismatch,
        );
    }

    let changed = mutate_json(manifest, |document| {
        first_manifest_row(document)?.insert(
            "source_references".to_owned(),
            json!(["https://developers.openai.com/private/future"]),
        );
        Ok(())
    })?;
    let snapshot = replace_case(&fixture.snapshot, manifest, changed)?;
    assert_has(
        &codes(&fixture.registry, &snapshot),
        RedactionCode::UnregisteredString,
    );
    Ok(())
}

#[test]
fn unknown_fields_types_and_fixture_foreign_synthetic_types_are_rejected()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let request = case("public/requests/request-model-profile.json")?;

    let changed = mutate_json(request, |document| {
        payload(document)?.insert("future_private_field".to_owned(), Value::Null);
        Ok(())
    })?;
    assert_changed_has(&fixture, request, changed, RedactionCode::SchemaMismatch)?;

    for unknown_type in [
        "response.future.event",
        "response.metadata",
        "norn-synthetic-generic-evt-05-event",
    ] {
        let changed = mutate_json(request, |document| {
            payload(document)?.insert("type".to_owned(), json!(unknown_type));
            Ok(())
        })?;
        let expected = if unknown_type.starts_with("norn-synthetic-") {
            RedactionCode::SyntheticMetadataMismatch
        } else {
            RedactionCode::SchemaMismatch
        };
        assert_changed_has(&fixture, request, changed, expected)?;
    }
    Ok(())
}

#[test]
fn raw_sensitive_values_remain_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let request = case("public/requests/request-model-profile.json")?;
    let raw = ["bear", "er ", "corpus-private-value"].concat();
    let changed = mutate_json(request, |document| {
        payload(document)?.insert("model".to_owned(), json!(raw));
        Ok(())
    })?;
    assert_changed_has(&fixture, request, changed, RedactionCode::DangerousShape)
}

#[test]
fn assistant_phase_preserves_all_four_states_and_rejects_other_contexts()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let phases =
        assistant_phase_inventory(case("public/streams/responses-messages-phase-order.sse")?)?;
    assert_eq!(
        phases,
        BTreeSet::from([
            "absent".to_owned(),
            "commentary".to_owned(),
            "final_answer".to_owned(),
            "null".to_owned(),
        ])
    );

    let request = case("public/requests/responses-stateless-replay-order.json")?;
    let changed = mutate_json(request, |document| {
        first_phased_message(document)?.insert("phase".to_owned(), json!("analysis"));
        Ok(())
    })?;
    assert_changed_has(&fixture, request, changed, RedactionCode::SchemaMismatch)?;

    let changed = mutate_json(request, |document| {
        first_phased_message(document)?.insert("role".to_owned(), json!("user"));
        Ok(())
    })?;
    assert_changed_has(&fixture, request, changed, RedactionCode::SchemaMismatch)
}

#[test]
fn closed_literals_are_accepted_only_in_their_protocol_context() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let matrix = case("backend-state-matrix.json")?;
    let changed = mutate_json(matrix, |document| {
        first_matrix_entry(document)?.insert("concern".to_owned(), json!("resp_12345678"));
        Ok(())
    })?;
    assert_changed_has(&fixture, matrix, changed, RedactionCode::SchemaMismatch)?;

    let profile = case("public/requests/request-model-profile.json")?;
    let changed = mutate_json(profile, |document| {
        request_reasoning(document)?.insert("summary".to_owned(), json!("future"));
        Ok(())
    })?;
    assert_changed_has(&fixture, profile, changed, RedactionCode::SchemaMismatch)?;

    let changed = mutate_json(profile, |document| {
        first_input_item(document)?.insert("reasoning".to_owned(), json!({"summary": "auto"}));
        Ok(())
    })?;
    assert_changed_has(&fixture, profile, changed, RedactionCode::ProhibitedField)?;

    let changed = mutate_json(profile, |document| {
        payload(document)?.insert("reasoning".to_owned(), json!([{"summary": "auto"}]));
        Ok(())
    })?;
    assert_changed_has(&fixture, profile, changed, RedactionCode::ProhibitedField)?;

    let replay = case("public/requests/responses-stateless-replay-order.json")?;
    let changed = mutate_json(replay, |document| {
        first_reasoning_item(document)?.insert("summary".to_owned(), json!("auto"));
        Ok(())
    })?;
    assert_changed_has(&fixture, replay, changed, RedactionCode::ProhibitedField)?;

    let oauth = case("codex/transport/oauth-failure-state.json")?;
    let changed = mutate_json(oauth, |document| {
        first_detail(document)?.insert("reason".to_owned(), json!("max_output_tokens"));
        Ok(())
    })?;
    assert_changed_has(&fixture, oauth, changed, RedactionCode::ProhibitedField)?;

    let changed = mutate_json(oauth, |document| {
        payload(document)?.insert(
            "incomplete_details".to_owned(),
            json!({"reason": "max_output_tokens"}),
        );
        Ok(())
    })?;
    assert_changed_has(&fixture, oauth, changed, RedactionCode::ProhibitedField)
}

#[test]
fn complete_contextual_literal_sets_are_accepted() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let profile = case("public/requests/request-model-profile.json")?;
    for summary in [
        json!("auto"),
        json!("concise"),
        json!("detailed"),
        Value::Null,
    ] {
        let changed = mutate_json(profile, |document| {
            request_reasoning(document)?.insert("summary".to_owned(), summary);
            Ok(())
        })?;
        assert_changed_lacks_semantic_codes(&fixture, profile, changed)?;
    }
    let changed = mutate_json(profile, |document| {
        request_reasoning(document)?.insert("generate_summary".to_owned(), json!("concise"));
        Ok(())
    })?;
    assert_changed_lacks_semantic_codes(&fixture, profile, changed)?;

    let usage = case("public/streams/usage-attempts-and-absence.sse")?;
    let content_filter = replace_text(usage.bytes, "max_output_tokens", "content_filter")?;
    assert_changed_lacks_semantic_codes(&fixture, usage, content_filter)?;
    let invalid = replace_text(usage.bytes, "max_output_tokens", "future_reason")?;
    assert_changed_has(&fixture, usage, invalid, RedactionCode::SchemaMismatch)
}

fn assert_changed_has(
    fixture: &support::CorpusFixture,
    corpus_case: &cases::CorpusCase,
    changed: Vec<u8>,
    expected: RedactionCode,
) -> Result<(), Box<dyn Error>> {
    let snapshot = replace_case(&fixture.snapshot, corpus_case, changed)?;
    assert_has(&codes(&fixture.registry, &snapshot), expected);
    Ok(())
}

fn assert_changed_lacks_semantic_codes(
    fixture: &support::CorpusFixture,
    corpus_case: &cases::CorpusCase,
    changed: Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    let snapshot = replace_case(&fixture.snapshot, corpus_case, changed)?;
    let observed = codes(&fixture.registry, &snapshot);
    assert_has(&observed, RedactionCode::ArtifactDigestMismatch);
    for forbidden in [
        RedactionCode::SchemaMismatch,
        RedactionCode::ProhibitedField,
        RedactionCode::ReusableState,
        RedactionCode::DangerousShape,
        RedactionCode::UnregisteredString,
        RedactionCode::SyntheticMetadataMismatch,
    ] {
        assert!(!observed.contains(&forbidden), "unexpected {forbidden:?}");
    }
    Ok(())
}

fn replace_text(bytes: &[u8], from: &str, to: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let original = std::str::from_utf8(bytes)?;
    let changed = original.replacen(from, to, 1);
    assert_ne!(changed, original);
    Ok(changed.into_bytes())
}

fn payload(document: &mut Value) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    document
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("fixture payload is missing"))
}

fn first_manifest_row(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("fixtures")
        .and_then(Value::as_array_mut)
        .and_then(|fixtures| fixtures.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("manifest fixture row is missing"))
}

fn first_matrix_entry(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .and_then(|entries| entries.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("backend matrix entry is missing"))
}

fn request_reasoning(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("reasoning")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("request reasoning is missing"))
}

fn first_input_item(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .and_then(|items| items.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("request input item is missing"))
}

fn first_reasoning_item(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .and_then(|items| {
            items
                .iter_mut()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("reasoning item is missing"))
}

fn first_detail(document: &mut Value) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("details")
        .and_then(Value::as_array_mut)
        .and_then(|details| details.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("transport detail is missing"))
}

fn first_phased_message(
    document: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>, io::Error> {
    payload(document)?
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .and_then(|items| {
            items.iter_mut().find(|item| {
                item.get("role").and_then(Value::as_str) == Some("assistant")
                    && item.get("phase").is_some()
            })
        })
        .and_then(Value::as_object_mut)
        .ok_or_else(|| io::Error::other("phased assistant message is missing"))
}

fn assistant_phase_inventory(
    corpus_case: &cases::CorpusCase,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let text = std::str::from_utf8(corpus_case.bytes)?;
    let mut phases = BTreeSet::new();
    for line in text.lines().filter_map(|line| line.strip_prefix("data:")) {
        let event = serde_json::from_str::<Value>(line.trim())?;
        let Some(item) = event.get("item") else {
            continue;
        };
        if item.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let phase = match item.get("phase") {
            None => "absent",
            Some(Value::Null) => "null",
            Some(Value::String(value)) => value,
            Some(Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
                return Err(io::Error::other("assistant phase has an invalid shape").into());
            }
        };
        phases.insert(phase.to_owned());
    }
    Ok(phases)
}
