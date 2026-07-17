use std::io::Cursor;
use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use crate::session::events::{EventBase, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::persistence::SessionStatus;

use super::{
    ResumeFidelity, SessionIndexEntry, SessionRecordOrigin, StrictFormatHeader, StrictStoreError,
    read_strict_event_file, read_strict_index_file,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "gpt-test".to_owned(),
        working_dir: "/workspace".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: 2,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
    }
}

fn event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn file_with_rows<T: Serialize>(rows: &[T]) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(&StrictFormatHeader::current())?;
    bytes.push(b'\n');
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn file_with_raw_row(row: &str) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(&StrictFormatHeader::current())?;
    bytes.push(b'\n');
    bytes.extend_from_slice(row.as_bytes());
    bytes.push(b'\n');
    Ok(bytes)
}

#[test]
fn accepts_exact_index_and_event_files() -> TestResult {
    let index_bytes = file_with_rows(&[entry("session-a")])?;
    let index = read_strict_index_file(Cursor::new(index_bytes), Path::new("index.jsonl"))?;
    assert_eq!(index.entries.len(), 1);

    let event_bytes = file_with_rows(&[event("hello")])?;
    let timeline = read_strict_event_file(Cursor::new(event_bytes), Path::new("session-a.jsonl"))?;
    assert_eq!(timeline.events.len(), 1);
    Ok(())
}

#[test]
fn accepts_provider_epoch_boundary_event() -> TestResult {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    };
    let bytes = file_with_rows(&[boundary])?;
    let timeline = read_strict_event_file(Cursor::new(bytes), Path::new("session-a.jsonl"))?;
    assert!(matches!(
        timeline.events.as_slice(),
        [SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::MigratedLegacy,
            ..
        }]
    ));
    Ok(())
}

#[test]
fn rejects_missing_legacy_and_newer_headers() -> TestResult {
    let event_row = serde_json::to_vec(&event("headerless"))?;
    let missing = read_strict_event_file(Cursor::new(event_row), Path::new("missing.jsonl"));
    assert!(matches!(missing, Err(StrictStoreError::TornTail { .. })));

    for (version, legacy) in [(1, true), (3, false)] {
        let bytes = format!("{{\"norn_session_format\":{version}}}\n");
        let result = read_strict_event_file(Cursor::new(bytes), Path::new("version.jsonl"));
        assert!(matches!(
            (result, legacy),
            (Err(StrictStoreError::LegacyFormat { .. }), true)
                | (Err(StrictStoreError::NewerFormat { .. }), false)
        ));
    }
    Ok(())
}

#[test]
fn rejects_headerless_complete_row_and_extra_header_fields() -> TestResult {
    let mut headerless = serde_json::to_vec(&event("headerless"))?;
    headerless.push(b'\n');
    let missing = read_strict_event_file(Cursor::new(headerless), Path::new("missing.jsonl"));
    assert!(matches!(
        missing,
        Err(StrictStoreError::MissingHeader { .. })
    ));

    let extra = b"{\"norn_session_format\":2,\"future\":true}\n";
    let result = read_strict_event_file(Cursor::new(extra), Path::new("extra.jsonl"));
    assert!(matches!(
        result,
        Err(StrictStoreError::InvalidHeader { .. })
    ));
    Ok(())
}

#[test]
fn rejects_empty_malformed_and_torn_rows() {
    let empty = b"{\"norn_session_format\":2}\n\n";
    assert!(matches!(
        read_strict_event_file(Cursor::new(empty), Path::new("empty.jsonl")),
        Err(StrictStoreError::EmptyRow { line: 2, .. })
    ));

    let malformed = b"{\"norn_session_format\":2}\n{not-json}\n";
    assert!(matches!(
        read_strict_event_file(Cursor::new(malformed), Path::new("malformed.jsonl")),
        Err(StrictStoreError::InvalidJson { line: 2, .. })
    ));

    let torn = b"{\"norn_session_format\":2}\n{\"type\":\"UserMessage\"}";
    assert!(matches!(
        read_strict_event_file(Cursor::new(torn), Path::new("torn.jsonl")),
        Err(StrictStoreError::TornTail { line: 2, .. })
    ));
}

#[test]
fn rejects_duplicate_keys_nested_in_index_and_event_rows() -> TestResult {
    let index_row = serde_json::to_string(&entry("session-a"))?;
    let duplicate_index = index_row.replacen(
        "\"kind\":\"native\"",
        "\"kind\":\"native\",\"kind\":\"native\"",
        1,
    );
    assert_ne!(duplicate_index, index_row);
    let index_result = read_strict_index_file(
        Cursor::new(file_with_raw_row(&duplicate_index)?),
        Path::new("index.jsonl"),
    );
    assert!(matches!(
        index_result,
        Err(StrictStoreError::InvalidJson { line: 2, .. })
    ));

    let event_row = serde_json::to_string(&event("hello"))?;
    let duplicate_event = event_row.replacen(
        "\"parent_id\":null",
        "\"parent_id\":null,\"parent_id\":null",
        1,
    );
    assert_ne!(duplicate_event, event_row);
    let event_result = read_strict_event_file(
        Cursor::new(file_with_raw_row(&duplicate_event)?),
        Path::new("session-a.jsonl"),
    );
    assert!(matches!(
        event_result,
        Err(StrictStoreError::InvalidJson { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn rejects_unknown_and_duplicate_index_rows() -> TestResult {
    let mut unknown = serde_json::to_value(entry("session-a"))?;
    if let Some(object) = unknown.as_object_mut() {
        object.insert("future".to_owned(), Value::Bool(true));
    }
    let unknown_bytes = file_with_rows(&[unknown])?;
    let unknown_result =
        read_strict_index_file(Cursor::new(unknown_bytes), Path::new("index.jsonl"));
    assert!(matches!(
        unknown_result,
        Err(StrictStoreError::UnknownField { line: 2, .. })
    ));

    let duplicate = entry("session-a");
    let duplicate_bytes = file_with_rows(&[duplicate.clone(), duplicate])?;
    let duplicate_result =
        read_strict_index_file(Cursor::new(duplicate_bytes), Path::new("index.jsonl"));
    assert!(matches!(
        duplicate_result,
        Err(StrictStoreError::DuplicateSessionId { line: 3, .. })
    ));
    Ok(())
}

#[test]
fn requires_a_canonical_random_generation_uuid() -> TestResult {
    let canonical = entry("session-a");

    let mut missing = serde_json::to_value(&canonical)?;
    let object = missing
        .as_object_mut()
        .ok_or("serialized index row was not an object")?;
    object.remove("generation");
    let missing_result = read_strict_index_file(
        Cursor::new(file_with_rows(&[missing])?),
        Path::new("index.jsonl"),
    );
    assert!(matches!(
        missing_result,
        Err(StrictStoreError::InvalidJson { line: 2, .. })
    ));

    let mut wrong_version = canonical.clone();
    wrong_version.generation = uuid::Uuid::now_v7();
    let version_result = read_strict_index_file(
        Cursor::new(file_with_rows(&[wrong_version])?),
        Path::new("index.jsonl"),
    );
    assert!(matches!(
        version_result,
        Err(StrictStoreError::InvalidIndexEntry { line: 2, .. })
    ));

    let mut non_canonical = serde_json::to_value(&canonical)?;
    non_canonical["generation"] = Value::String(canonical.generation.to_string().to_uppercase());
    let canonical_result = read_strict_index_file(
        Cursor::new(file_with_rows(&[non_canonical])?),
        Path::new("index.jsonl"),
    );
    assert!(matches!(
        canonical_result,
        Err(StrictStoreError::NonCanonicalRow { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn rejects_non_normalized_working_directories_and_partial_child_shapes() -> TestResult {
    let mut relative = entry("relative");
    relative.working_dir = "workspace".to_owned();
    let relative_bytes = file_with_rows(&[relative])?;
    assert!(matches!(
        read_strict_index_file(Cursor::new(relative_bytes), Path::new("index.jsonl")),
        Err(StrictStoreError::InvalidIndexEntry { line: 2, .. })
    ));

    let mut partial_child = entry("partial-child");
    partial_child.parent_id = Some("parent".to_owned());
    let partial_bytes = file_with_rows(&[partial_child])?;
    assert!(matches!(
        read_strict_index_file(Cursor::new(partial_bytes), Path::new("index.jsonl")),
        Err(StrictStoreError::InvalidIndexEntry { line: 2, .. })
    ));

    let mut inspect_only = entry("inspect-only");
    inspect_only.fidelity = ResumeFidelity::InspectOnly;
    let inspect_bytes = file_with_rows(&[inspect_only])?;
    assert!(matches!(
        read_strict_index_file(Cursor::new(inspect_bytes), Path::new("index.jsonl")),
        Err(StrictStoreError::InvalidIndexEntry { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn rejects_unknown_event_types_and_fields() -> TestResult {
    let unknown_type = file_with_rows(&[json!({
        "type": "FutureEvent",
        "base": {"id": "event-a", "parent_id": null, "timestamp": Utc::now()},
    })])?;
    let type_result =
        read_strict_event_file(Cursor::new(unknown_type), Path::new("unknown-type.jsonl"));
    assert!(matches!(
        type_result,
        Err(StrictStoreError::UnknownEventType { line: 2, .. })
    ));

    let mut unknown_field = serde_json::to_value(event("hello"))?;
    if let Some(object) = unknown_field.as_object_mut() {
        object.insert("future".to_owned(), Value::Bool(true));
    }
    let field_bytes = file_with_rows(&[unknown_field])?;
    let field_result =
        read_strict_event_file(Cursor::new(field_bytes), Path::new("unknown-field.jsonl"));
    assert!(matches!(
        field_result,
        Err(StrictStoreError::UnknownField { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn rejects_unknown_nested_fields_without_serde_loss() -> TestResult {
    let mut raw = serde_json::to_value(event("hello"))?;
    if let Some(base) = raw.get_mut("base").and_then(Value::as_object_mut) {
        base.insert("future".to_owned(), Value::Bool(true));
    }
    let bytes = file_with_rows(&[raw])?;
    let result = read_strict_event_file(Cursor::new(bytes), Path::new("nested.jsonl"));
    assert!(matches!(
        result,
        Err(StrictStoreError::UnknownField { line: 2, .. }
            | StrictStoreError::NonCanonicalRow { line: 2, .. })
    ));
    Ok(())
}

#[test]
fn rejects_duplicate_event_ids() -> TestResult {
    let duplicate = event("same-id");
    let bytes = file_with_rows(&[duplicate.clone(), duplicate])?;
    let result = read_strict_event_file(Cursor::new(bytes), Path::new("duplicate.jsonl"));
    assert!(matches!(
        result,
        Err(StrictStoreError::DuplicateEventId { line: 3, .. })
    ));
    Ok(())
}

#[test]
fn migrated_canonical_records_still_require_a_fresh_epoch() {
    let mut migrated = entry("migrated");
    migrated.origin = SessionRecordOrigin::MigratedLegacy {
        source_format: 1,
        source_sha256: "a".repeat(64),
    };
    assert!(migrated.permits_resume());
    assert!(migrated.requires_fresh_provider_epoch());

    let native = entry("native");
    assert!(native.permits_resume());
    assert!(!native.requires_fresh_provider_epoch());
}
