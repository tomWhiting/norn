use std::io;

use super::conversion::events_to_messages;
use super::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::provider::request::ToolCallKind;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn item(raw: serde_json::Value, output_index: u64) -> TestItemResult {
    let item_id = raw
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Ok(ResponseTranscriptItem {
        item: ResponseItem::from_value(raw)?,
        provenance: ResponseStreamProvenance {
            item_id,
            output_index: Some(output_index),
            content_index: None,
            sequence_number: Some(output_index.saturating_add(1)),
        },
    })
}

type TestItemResult = Result<ResponseTranscriptItem, crate::provider::ResponseItemError>;

#[test]
fn assistant_event_round_trip_and_resume_keep_canonical_items() -> TestResult {
    let raws = [
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "future_summary", "payload": true}],
            "encrypted_content": "cipher"
        }),
        serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {"type": "search", "queries": ["norn"]}
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "phase": "final_answer",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "answer",
                "annotations": [],
                "logprobs": []
            }]
        }),
    ];
    let response_items = raws
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(item(raw, u64::try_from(index)?)?))
        .collect::<TestItemsResult>()?;
    let event = SessionEvent::AssistantMessage {
        response_items,
        base: EventBase::new(None),
        content: "stale flat answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_1".to_owned()),
    };
    let encoded = serde_json::to_string(&event)?;
    let decoded: SessionEvent = serde_json::from_str(&encoded)?;
    let messages = events_to_messages(std::slice::from_ref(&decoded));
    let Some(message) = messages.first() else {
        return Err(io::Error::other("assistant event did not resume as a message").into());
    };
    assert_eq!(message.content.as_deref(), Some("answer"));
    let preserved = message
        .response_items
        .iter()
        .map(|entry| entry.item.raw().clone())
        .collect::<Vec<_>>();
    assert_eq!(preserved, raws);
    Ok(())
}

#[test]
fn legacy_assistant_event_without_items_remains_readable() -> TestResult {
    let event: SessionEvent = serde_json::from_value(serde_json::json!({
        "type": "AssistantMessage",
        "base": {
            "id": "event-1",
            "parent_id": null,
            "timestamp": "2026-07-16T00:00:00Z"
        },
        "content": "legacy",
        "thinking": "",
        "tool_calls": [],
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "stop_reason": "end_turn"
    }))?;
    let SessionEvent::AssistantMessage { response_items, .. } = event else {
        return Err(io::Error::other("legacy event changed variant").into());
    };
    assert!(response_items.is_empty());
    Ok(())
}

#[test]
fn canonical_calls_override_conflicting_flat_projection() -> TestResult {
    let canonical = item(
        serde_json::json!({
            "type": "function_call",
            "id": "fc_actual",
            "call_id": "call_actual",
            "name": "read",
            "arguments": "{\"path\":\"README.md\"}",
            "status": "completed"
        }),
        0,
    )?;
    let event = SessionEvent::AssistantMessage {
        response_items: vec![canonical],
        base: EventBase::new(None),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "call_forged".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({"path": "wrong"}),
            kind: ToolCallKind::Function,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    };

    let authoritative = event
        .assistant_tool_calls()
        .ok_or_else(|| io::Error::other("assistant call projection was unavailable"))?;
    assert_eq!(authoritative.len(), 1);
    assert_eq!(authoritative[0].call_id, "call_actual");
    assert_eq!(authoritative[0].name, "read");

    let resumed = events_to_messages(&[event]);
    assert_eq!(resumed[0].tool_calls.len(), 1);
    assert_eq!(resumed[0].tool_calls[0].call_id, "call_actual");
    assert_eq!(resumed[0].tool_calls[0].name, "read");
    Ok(())
}

type TestItemsResult = Result<Vec<ResponseTranscriptItem>, Box<dyn std::error::Error>>;
