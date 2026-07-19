use std::error::Error;

use serde_json::{Value, json};

use super::*;
use crate::provider::events::StopReason;
use crate::provider::response_item::{ResponseStreamProvenance, ResponseTranscriptItem};

fn terminal_update(items: Vec<ResponseTranscriptItem>) -> ReconcileUpdate {
    ReconcileUpdate::Terminal {
        items,
        delta_reconciliations: Vec::new(),
    }
}

fn valid_completed_data() -> Value {
    json!({
        "response": {
            "id": "resp_1",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 4,
                "input_tokens_details": {"cached_tokens": 1, "cache_write_tokens": 2},
                "output_tokens": 2,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 6
            }
        }
    })
}

fn completed_event(data: Value) -> SseEvent {
    SseEvent {
        event_type: "response.completed".to_owned(),
        data,
    }
}

fn decode_public_terminal(
    event: &SseEvent,
    update: &ReconcileUpdate,
) -> Result<ProviderEvent, ProviderError> {
    decode_terminal(event, update, ResponsesDialect::Public)
}

fn decode_codex_terminal(
    event: &SseEvent,
    update: &ReconcileUpdate,
) -> Result<ProviderEvent, ProviderError> {
    decode_terminal(event, update, ResponsesDialect::Codex)
}

fn remove_fixture_field(
    data: &mut Value,
    parent_pointer: &str,
    field: &str,
) -> Result<(), Box<dyn Error>> {
    let Some(parent) = data
        .pointer_mut(parent_pointer)
        .and_then(Value::as_object_mut)
    else {
        return Err(format!("fixture omitted object at {parent_pointer}").into());
    };
    if parent.remove(field).is_none() {
        return Err(format!("fixture omitted field at {parent_pointer}/{field}").into());
    }
    Ok(())
}

fn set_fixture_field(
    data: &mut Value,
    parent_pointer: &str,
    field: &str,
    value: Value,
) -> Result<(), Box<dyn Error>> {
    let Some(parent) = data
        .pointer_mut(parent_pointer)
        .and_then(Value::as_object_mut)
    else {
        return Err(format!("fixture omitted object at {parent_pointer}").into());
    };
    let Some(current) = parent.get_mut(field) else {
        return Err(format!("fixture omitted field at {parent_pointer}/{field}").into());
    };
    *current = value;
    Ok(())
}

fn insert_response_field(
    data: &mut Value,
    field: &str,
    value: Value,
) -> Result<(), Box<dyn Error>> {
    let Some(response) = data.pointer_mut("/response").and_then(Value::as_object_mut) else {
        return Err("fixture omitted /response object".into());
    };
    if response.insert(field.to_owned(), value).is_some() {
        return Err(format!("fixture already contained /response/{field}").into());
    }
    Ok(())
}

#[test]
fn completed_projects_identity_and_valid_usage() -> Result<(), Box<dyn Error>> {
    let event = completed_event(valid_completed_data());
    let ProviderEvent::Done {
        stop_reason,
        usage,
        response_id,
    } = decode_public_terminal(&event, &terminal_update(Vec::new()))?
    else {
        return Err("terminal decoder returned a non-Done event".into());
    };
    assert_eq!(stop_reason, StopReason::EndTurn);
    assert_eq!(usage.input_tokens, 4);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(usage.cache_read_tokens, 1);
    assert_eq!(usage.cache_write_tokens, 2);
    assert_eq!(response_id.as_deref(), Some("resp_1"));
    Ok(())
}

#[test]
fn completed_end_turn_directive_has_explicit_semantics() -> Result<(), Box<dyn Error>> {
    let absent = decode_codex_terminal(
        &completed_event(valid_completed_data()),
        &terminal_update(Vec::new()),
    )?;
    assert!(matches!(
        absent,
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));

    for (wire_value, expected) in [
        (Value::Null, StopReason::EndTurn),
        (Value::Bool(true), StopReason::EndTurn),
        (Value::Bool(false), StopReason::ContinueTurn),
    ] {
        let mut data = valid_completed_data();
        insert_response_field(&mut data, "end_turn", wire_value)?;
        let ProviderEvent::Done { stop_reason, .. } =
            decode_codex_terminal(&completed_event(data), &terminal_update(Vec::new()))?
        else {
            return Err("terminal decoder returned a non-Done event".into());
        };
        assert_eq!(stop_reason, expected);
    }
    Ok(())
}

#[test]
fn completed_rejects_malformed_end_turn_directives() -> Result<(), Box<dyn Error>> {
    for wire_value in [
        Value::String("false".to_owned()),
        json!(0),
        json!([]),
        json!({}),
    ] {
        let mut data = valid_completed_data();
        insert_response_field(&mut data, "end_turn", wire_value.clone())?;
        assert!(
            decode_codex_terminal(&completed_event(data), &terminal_update(Vec::new())).is_err(),
            "terminal decoder accepted invalid end_turn value {wire_value}",
        );
    }
    Ok(())
}

#[test]
fn public_responses_rejects_the_codex_end_turn_overlay() -> Result<(), Box<dyn Error>> {
    for wire_value in [Value::Null, Value::Bool(true), Value::Bool(false)] {
        let mut data = valid_completed_data();
        insert_response_field(&mut data, "end_turn", wire_value)?;
        assert!(
            decode_public_terminal(&completed_event(data), &terminal_update(Vec::new())).is_err(),
            "public terminal decoder accepted a Codex-only end_turn field",
        );
    }
    Ok(())
}

#[test]
fn actionable_calls_continue_for_every_end_turn_value() -> Result<(), Box<dyn Error>> {
    let function_call = json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "structured_output",
        "arguments": "{\"answer\":\"accepted\"}"
    });
    let custom_call = json!({
        "type": "custom_tool_call",
        "id": "ctc_1",
        "call_id": "call_2",
        "name": "apply_patch",
        "input": "patch content"
    });

    for raw_item in [function_call, custom_call] {
        let items = vec![ResponseTranscriptItem {
            item: ResponseItem::from_value(raw_item)?,
            provenance: ResponseStreamProvenance::default(),
        }];
        for wire_value in [Value::Null, Value::Bool(true), Value::Bool(false)] {
            let mut data = valid_completed_data();
            insert_response_field(&mut data, "end_turn", wire_value)?;
            let ProviderEvent::Done { stop_reason, .. } =
                decode_codex_terminal(&completed_event(data), &terminal_update(items.clone()))?
            else {
                return Err("terminal decoder returned a non-Done event".into());
            };
            assert_eq!(stop_reason, StopReason::ToolUse);
        }
    }
    Ok(())
}

#[test]
fn completed_accepts_absent_optional_status() -> Result<(), Box<dyn Error>> {
    let mut data = valid_completed_data();
    remove_fixture_field(&mut data, "/response", "status")?;

    let ProviderEvent::Done { response_id, .. } =
        decode_public_terminal(&completed_event(data), &terminal_update(Vec::new()))?
    else {
        return Err("terminal decoder returned a non-Done event".into());
    };
    assert_eq!(response_id.as_deref(), Some("resp_1"));
    Ok(())
}

#[test]
fn completed_rejects_null_or_inconsistent_status() -> Result<(), Box<dyn Error>> {
    for status in [Value::Null, Value::String("incomplete".to_owned())] {
        let mut data = valid_completed_data();
        set_fixture_field(&mut data, "/response", "status", status.clone())?;
        assert!(
            decode_public_terminal(&completed_event(data), &terminal_update(Vec::new())).is_err(),
            "terminal decoder accepted invalid status {status}"
        );
    }
    Ok(())
}

#[test]
fn missing_response_id_is_not_fabricated() {
    let event = SseEvent {
        event_type: "response.completed".to_owned(),
        data: json!({"response": {"status": "completed", "output": []}}),
    };
    assert!(decode_public_terminal(&event, &terminal_update(Vec::new())).is_err());
}

#[test]
fn absent_usage_remains_distinguishable_on_raw_event() -> Result<(), Box<dyn Error>> {
    let raw = json!({
        "type": "response.completed",
        "sequence_number": 9,
        "response": {"id": "resp_1", "status": "completed", "output": []}
    });
    let event = SseEvent {
        event_type: "response.completed".to_owned(),
        data: raw.clone(),
    };
    let ProviderEvent::Done { usage, .. } =
        decode_public_terminal(&event, &terminal_update(Vec::new()))?
    else {
        return Err("terminal decoder returned a non-Done event".into());
    };
    assert_eq!(usage.input_tokens, 0);
    assert!(raw["response"].get("usage").is_none());
    Ok(())
}

#[test]
fn present_usage_cannot_be_null() -> Result<(), Box<dyn Error>> {
    let mut data = valid_completed_data();
    set_fixture_field(&mut data, "/response", "usage", Value::Null)?;
    assert!(decode_public_terminal(&completed_event(data), &terminal_update(Vec::new())).is_err());
    Ok(())
}

#[test]
fn present_usage_requires_every_field_and_rejects_null_fields() -> Result<(), Box<dyn Error>> {
    let required_fields = [
        ("/response/usage", "input_tokens"),
        ("/response/usage", "input_tokens_details"),
        ("/response/usage/input_tokens_details", "cached_tokens"),
        ("/response/usage/input_tokens_details", "cache_write_tokens"),
        ("/response/usage", "output_tokens"),
        ("/response/usage", "output_tokens_details"),
        ("/response/usage/output_tokens_details", "reasoning_tokens"),
        ("/response/usage", "total_tokens"),
    ];

    for (parent_pointer, field) in required_fields {
        let path = format!("{parent_pointer}/{field}");

        let mut missing = valid_completed_data();
        remove_fixture_field(&mut missing, parent_pointer, field)?;
        assert!(
            decode_public_terminal(&completed_event(missing), &terminal_update(Vec::new()))
                .is_err(),
            "terminal decoder accepted omitted usage field {path}"
        );

        let mut null = valid_completed_data();
        set_fixture_field(&mut null, parent_pointer, field, Value::Null)?;
        assert!(
            decode_public_terminal(&completed_event(null), &terminal_update(Vec::new())).is_err(),
            "terminal decoder accepted null usage field {path}"
        );
    }
    Ok(())
}
