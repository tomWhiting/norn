use std::io;

use serde_json::Value;

use crate::provider::events::ProviderEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::conversion::events_to_messages;
use crate::session::events::SessionEvent;

pub(super) fn supported_non_audio_items(id_suffix: &str, text: &str) -> Vec<Value> {
    vec![
        serde_json::json!({
            "type": "reasoning",
            "id": format!("rs_{id_suffix}"),
            "summary": [{"type": "summary_text", "text": "preserve canonical order"}],
            "content": [{"type": "reasoning_text", "text": "detail"}],
            "encrypted_content": "opaque-reasoning",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "web_search_call",
            "id": format!("ws_{id_suffix}"),
            "status": "completed",
            "action": {
                "type": "search",
                "query": "canonical lifecycle",
                "sources": [{"type": "url", "url": "https://example.test/lifecycle"}]
            }
        }),
        serde_json::json!({
            "type": "message",
            "id": format!("msg_{id_suffix}"),
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": text.len(),
                    "url": "https://example.test/lifecycle",
                    "title": "Lifecycle source"
                }],
                "logprobs": [{
                    "token": text,
                    "bytes": text.as_bytes(),
                    "logprob": -0.1,
                    "top_logprobs": []
                }]
            }]
        }),
        serde_json::json!({
            "type": "image_generation_call",
            "id": format!("ig_{id_suffix}"),
            "status": "completed",
            "result": "ZmluYWwtaW1hZ2U="
        }),
        serde_json::json!({
            "type": "mcp_call",
            "id": format!("mcp_{id_suffix}"),
            "status": "completed",
            "arguments": "{\"query\":\"canonical lifecycle\"}",
            "name": "lookup",
            "server_label": "docs",
            "output": "structured result",
            "error": null
        }),
        serde_json::json!({
            "type": "code_interpreter_call",
            "id": format!("ci_{id_suffix}"),
            "container_id": format!("container_{id_suffix}"),
            "status": "completed",
            "code": "print('ok')",
            "outputs": [{
                "type": "image",
                "url": "https://example.test/generated.png"
            }]
        }),
    ]
}

pub(super) fn transcript_item(
    raw: Value,
    output_index: u64,
) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
    let item_id = raw.get("id").and_then(Value::as_str).map(str::to_owned);
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

pub(super) fn completed_item_event(
    raw: Value,
    output_index: u64,
) -> Result<ProviderEvent, crate::provider::ResponseItemError> {
    Ok(ProviderEvent::ResponseItemDone {
        item: transcript_item(raw, output_index)?,
    })
}

pub(super) fn canonical_item_values(events: &[SessionEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { response_items, .. } => Some(response_items),
            _ => None,
        })
        .flatten()
        .map(|entry| entry.item.raw().clone())
        .collect()
}

pub(super) fn stateless_payload_input(
    events: &[SessionEvent],
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let request = ProviderRequest {
        messages: events_to_messages(events),
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
    let payload = crate::provider::openai::request::build_payload(&request, "codex_subscription")?;
    let input = payload
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("Responses payload had no input array"))?;
    Ok(input.clone())
}

pub(super) fn contains_contiguous_items(input: &[Value], expected: &[Value]) -> bool {
    !expected.is_empty()
        && input
            .windows(expected.len())
            .any(|window| window == expected)
}
