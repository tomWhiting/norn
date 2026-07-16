use std::io;

use super::*;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::conversion::events_to_messages;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::{EventStore, JsonlSink, read_session_events};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn transcript_item(raw: serde_json::Value, output_index: u64) -> TestItemResult {
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
            sequence_number: Some(output_index.saturating_add(10)),
        },
    })
}

type TestItemResult = Result<ResponseTranscriptItem, crate::provider::ResponseItemError>;

#[test]
fn canonical_assistant_items_replay_in_exact_order_without_stream_coordinates() -> TestResult {
    let raw_items = [
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "first"}],
            "encrypted_content": "cipher"
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "working",
                "annotations": [{"type": "url_citation", "url": "https://example.com"}],
                "logprobs": []
            }]
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "read",
            "arguments": "{\"path\":\"README.md\"}",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "future_hosted_call",
            "id": "future_1",
            "payload": {"kept": true}
        }),
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_2",
            "summary": [{"type": "summary_text", "text": "after the call"}],
            "encrypted_content": "cipher-2"
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_2",
            "role": "assistant",
            "phase": null,
            "status": "completed",
            "content": [{"type": "refusal", "refusal": "cannot do one part"}]
        }),
    ];
    let response_items = raw_items
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(transcript_item(raw, u64::try_from(index)?)?))
        .collect::<TestItemsResult>()?;
    let request = ProviderRequest {
        messages: vec![Message {
            response_items,
            role: MessageRole::Assistant,
            content: Some("lossy projection must not be serialized".to_owned()),
            thinking: "lossy reasoning projection".to_owned(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        }],
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    let payload = build_payload(&request, CATALOG_BACKEND_CODEX_SUBSCRIPTION)?;
    let Some(input) = payload.get("input").and_then(serde_json::Value::as_array) else {
        return Err(io::Error::other("request input was not an array").into());
    };
    assert_eq!(input, &raw_items);
    assert!(input.iter().all(|item| item.get("output_index").is_none()));
    assert!(
        input
            .iter()
            .all(|item| item.get("sequence_number").is_none())
    );
    Ok(())
}

#[test]
fn persisted_hosted_search_turn_replays_exactly_into_stateless_continuation() -> TestResult {
    let raw_items = vec![
        serde_json::json!({
            "type": "web_search_call",
            "id": "ws_persisted",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["canonical transcript persistence"],
                "sources": [{
                    "type": "url",
                    "url": "https://example.test/source",
                    "title": "Canonical transcript source"
                }]
            }
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_persisted",
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "I found the relevant source.",
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": 31,
                    "url": "https://example.test/source",
                    "title": "Canonical transcript source"
                }],
                "logprobs": []
            }]
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc_persisted",
            "call_id": "call_persisted",
            "name": "read",
            "arguments": "{\"path\":\"README.md\"}",
            "caller": {"type": "program", "caller_id": "call_program_persisted"},
            "status": "completed"
        }),
    ];
    let response_items = raw_items
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(transcript_item(raw, u64::try_from(index)?)?))
        .collect::<TestItemsResult>()?;

    let temp = tempfile::tempdir()?;
    let session_id = "canonical-hosted-search";
    let session_path = temp.path().join(format!("{session_id}.jsonl"));
    let store = EventStore::with_sink(Box::new(JsonlSink::open(&session_path)?));
    let assistant_id = store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: "lossy flat text must not be replayed".to_owned(),
        thinking: "lossy flat reasoning must not be replayed".to_owned(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "call_stale_flat_projection".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({"path": "wrong.txt"}),
            kind: ToolCallKind::Function,
            caller: crate::provider::request::ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: Some("resp_persisted".to_owned()),
    })?;
    store.append(SessionEvent::ToolResult {
        base: EventBase::new(Some(assistant_id)),
        tool_call_id: "call_persisted".to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::Value::String("tool result".to_owned()),
        spool_ref: None,
        duration_ms: 1,
    })?;
    store.checkpoint()?;
    drop(store);

    let replay = read_session_events(temp.path(), session_id)?;
    assert_eq!(replay.skipped_lines, 0);
    let request = ProviderRequest {
        messages: events_to_messages(&replay.events),
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    let payload = build_payload(&request, CATALOG_BACKEND_CODEX_SUBSCRIPTION)?;
    let Some(input) = payload.get("input").and_then(serde_json::Value::as_array) else {
        return Err(io::Error::other("request input was not an array").into());
    };

    let mut expected_input = raw_items;
    expected_input.push(serde_json::json!({
        "type": "function_call_output",
        "call_id": "call_persisted",
        "output": "tool result",
        "caller": {"type": "program", "caller_id": "call_program_persisted"}
    }));
    assert_eq!(input, &expected_input);
    assert!(input.iter().all(|item| item.get("output_index").is_none()));
    assert!(
        input
            .iter()
            .all(|item| item.get("sequence_number").is_none())
    );
    let serialized = payload.to_string();
    assert!(!serialized.contains("lossy flat"));
    assert!(!serialized.contains("call_stale_flat_projection"));
    Ok(())
}

#[test]
fn caller_projection_is_ordered_family_aware_and_presence_preserving() -> TestResult {
    let raw_items = [
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "first_function",
            "arguments": "{}",
            "caller": {
                "type": "program",
                "caller_id": "program_first",
                "provider_extension": {"kept": true}
            }
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "second_function",
            "arguments": "{}",
            "caller": null
        }),
        serde_json::json!({
            "type": "custom_tool_call",
            "call_id": "call_reused",
            "name": "custom",
            "input": "opaque",
            "caller": {"type": "program", "caller_id": "program_custom"}
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_absent",
            "name": "without_caller",
            "arguments": "{}"
        }),
    ];
    let response_items = raw_items
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(transcript_item(raw, u64::try_from(index)?)?))
        .collect::<TestItemsResult>()?;
    let mut messages = vec![Message {
        response_items,
        role: MessageRole::Assistant,
        content: None,
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
    }];
    messages.extend([
        tool_result_message("call_reused", ToolCallKind::Function, "first"),
        tool_result_message("call_reused", ToolCallKind::Custom, "custom"),
        tool_result_message("call_reused", ToolCallKind::Function, "second"),
        tool_result_message("call_absent", ToolCallKind::Function, "absent"),
    ]);
    let request = ProviderRequest {
        messages,
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    let payload = build_payload(&request, CATALOG_BACKEND_CODEX_SUBSCRIPTION)?;
    let Some(input) = payload.get("input").and_then(serde_json::Value::as_array) else {
        return Err(io::Error::other("request input was not an array").into());
    };
    let outputs = &input[raw_items.len()..];
    assert_eq!(outputs[0]["caller"], raw_items[0]["caller"]);
    assert_eq!(outputs[1]["caller"]["caller_id"], "program_custom");
    assert_eq!(outputs[2]["caller"], serde_json::Value::Null);
    assert!(outputs[3].get("caller").is_none());
    Ok(())
}

fn tool_result_message(call_id: &str, kind: ToolCallKind, content: &str) -> Message {
    Message {
        response_items: Vec::new(),
        role: MessageRole::ToolResult,
        content: Some(content.to_owned()),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(call_id.to_owned()),
        tool_name: Some("fixture".to_owned()),
        tool_call_kind: Some(kind),
        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
    }
}

type TestItemsResult = Result<Vec<ResponseTranscriptItem>, Box<dyn std::error::Error>>;
