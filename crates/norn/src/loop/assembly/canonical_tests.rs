use std::io;

use super::*;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn completed_item(raw: serde_json::Value, output_index: u64) -> TestResultEvent {
    let item_id = raw
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let item = ResponseItem::from_value(raw)?;
    Ok(ProviderEvent::ResponseItemDone {
        item: ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance {
                item_id,
                output_index: Some(output_index),
                content_index: None,
                sequence_number: Some(output_index.saturating_add(1)),
            },
        },
    })
}

type TestResultEvent = Result<ProviderEvent, crate::provider::ResponseItemError>;

#[test]
fn canonical_items_keep_cross_family_order_and_drive_derived_views() -> TestResult {
    let raw_items = vec![
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "first"}],
            "encrypted_content": "cipher-1"
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_commentary",
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
            "type": "message",
            "id": "msg_final",
            "role": "assistant",
            "phase": null,
            "status": "completed",
            "content": [
                {"type": "refusal", "refusal": "partial refusal"},
                {"type": "output_text", "text": "done", "annotations": [], "logprobs": []}
            ]
        }),
    ];
    let mut events = Vec::new();
    events.push(ProviderEvent::TextDelta {
        text: "stale delta".to_owned(),
    });
    for (index, raw) in raw_items.iter().cloned().enumerate() {
        events.push(completed_item(raw, u64::try_from(index)?)?);
    }
    events.push(ProviderEvent::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        response_id: Some("resp_1".to_owned()),
    });

    let Some(response) = assemble_response(&events) else {
        return Err(io::Error::other("canonical response did not assemble").into());
    };
    assert_eq!(response.response_items.len(), raw_items.len());
    let preserved = response
        .response_items
        .iter()
        .map(|item| item.item.raw().clone())
        .collect::<Vec<_>>();
    assert_eq!(preserved, raw_items);
    assert_eq!(response.text, "workingdone");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].call_id, "call_1");
    assert_eq!(response.tool_calls[0].name, "read");
    assert_eq!(response.reasoning.len(), 1);
    assert!(matches!(
        &response.response_items[3].item,
        ResponseItem::Opaque(_)
    ));
    Ok(())
}

#[test]
fn completed_message_repairs_truncated_text_delta() -> TestResult {
    let events = vec![
        ProviderEvent::TextDelta {
            text: "trunc".to_owned(),
        },
        completed_item(
            serde_json::json!({
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "complete",
                    "annotations": [],
                    "logprobs": []
                }]
            }),
            0,
        )?,
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ];
    let Some(response) = assemble_response(&events) else {
        return Err(io::Error::other("response did not assemble").into());
    };
    assert_eq!(response.text, "complete");
    Ok(())
}
