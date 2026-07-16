use std::io;

use super::*;
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

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
    let raw_items = vec![
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

type TestItemsResult = Result<Vec<ResponseTranscriptItem>, Box<dyn std::error::Error>>;
