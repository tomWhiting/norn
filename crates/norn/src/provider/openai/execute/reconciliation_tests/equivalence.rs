use std::io;

use super::*;

fn added(sequence: u64, output_index: u64, item: Value) -> SseEvent {
    let mut data = serde_json::Map::new();
    data.insert(
        "type".to_owned(),
        Value::String("response.output_item.added".to_owned()),
    );
    data.insert("sequence_number".to_owned(), Value::from(sequence));
    data.insert("output_index".to_owned(), Value::from(output_index));
    data.insert("item".to_owned(), item);
    SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: Value::Object(data),
    }
}

fn indexed_event(
    event_type: &str,
    sequence: u64,
    item_id: &str,
    output_index: u64,
    fields: Value,
) -> Result<SseEvent, io::Error> {
    let mut data = json!({
        "type": event_type,
        "sequence_number": sequence,
        "item_id": item_id,
        "output_index": output_index,
    });
    let object = data
        .as_object_mut()
        .ok_or_else(|| io::Error::other("event envelope was not an object"))?;
    let Value::Object(additions) = fields else {
        return Err(io::Error::other("event fields were not an object"));
    };
    object.extend(additions);
    Ok(SseEvent {
        event_type: event_type.to_owned(),
        data,
    })
}

fn map_frames(frames: &[SseEvent]) -> Result<Vec<ProviderEvent>, ProviderError> {
    let mut mapper = ResponsesMapper::default();
    let mut events = Vec::new();
    for frame in frames {
        for event in mapper.map_event(frame) {
            events.push(event?);
        }
    }
    Ok(events)
}

#[derive(Clone, Copy)]
enum LiveProjection {
    Text,
    Refusal,
    Thinking,
}
use LiveProjection::{Refusal, Text, Thinking};

fn projected_text(events: &[ProviderEvent], projection: LiveProjection) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match (projection, event) {
            (LiveProjection::Text, ProviderEvent::TextDelta { text })
            | (LiveProjection::Thinking, ProviderEvent::ThinkingDelta { text }) => {
                Some(text.as_str())
            }
            (LiveProjection::Refusal, ProviderEvent::RefusalDelta { refusal, .. }) => {
                Some(refusal.as_str())
            }
            _ => None,
        })
        .collect()
}

fn canonical_output() -> Vec<Value> {
    let annotation = json!({
        "type": "url_citation",
        "start_index": 0,
        "end_index": 6,
        "title": "Source",
        "url": "https://example.test/source"
    });
    vec![
        json!({
            "type": "reasoning",
            "id": "rs_equivalent",
            "summary": [{"type": "summary_text", "text": "plan"}],
            "content": [{"type": "reasoning_text", "text": "detail"}],
            "encrypted_content": "ciphertext",
            "status": "completed"
        }),
        message(
            "msg_equivalent",
            &json!([
                {
                    "type": "output_text",
                    "text": "answer",
                    "annotations": [annotation],
                    "logprobs": [{
                        "token": "answer",
                        "bytes": [97, 110, 115, 119, 101, 114],
                        "logprob": -0.1,
                        "top_logprobs": []
                    }]
                },
                {"type": "refusal", "refusal": "cannot"}
            ]),
        ),
        json!({
            "type": "web_search_call",
            "id": "ws_equivalent",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "norn",
                "sources": [{"type": "url", "url": "https://example.test/source"}]
            }
        }),
        json!({
            "type": "image_generation_call",
            "id": "ig_equivalent",
            "status": "completed",
            "result": "ZmluYWwtaW1hZ2U="
        }),
        json!({
            "type": "mcp_call",
            "id": "mcp_equivalent",
            "status": "completed",
            "arguments": "{\"query\":\"norn\"}",
            "name": "lookup",
            "server_label": "docs",
            "output": "result",
            "error": null
        }),
        json!({
            "type": "code_interpreter_call",
            "id": "ci_equivalent",
            "container_id": "container_equivalent",
            "status": "completed",
            "code": "print('ok')",
            "outputs": [{
                "type": "image",
                "url": "https://example.test/generated.png"
            }]
        }),
    ]
}

fn streamed_frames(output: &[Value]) -> Result<Vec<SseEvent>, io::Error> {
    let annotation = output[1]["content"][0]["annotations"][0].clone();
    let message_done = done_item(35, 1, &output[1]);
    Ok(vec![
        added(
            1,
            0,
            json!({
                "type": "reasoning",
                "id": "rs_equivalent",
                "summary": [],
                "content": [],
                "encrypted_content": null,
                "status": "in_progress"
            }),
        ),
        added(
            2,
            1,
            json!({
                "type": "message",
                "id": "msg_equivalent",
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }),
        ),
        added(
            3,
            2,
            json!({
                "type": "web_search_call",
                "id": "ws_equivalent",
                "status": "in_progress",
                "action": {"type": "search", "query": "norn"}
            }),
        ),
        added(
            4,
            3,
            json!({
                "type": "image_generation_call",
                "id": "ig_equivalent",
                "status": "in_progress",
                "result": null
            }),
        ),
        added(
            5,
            4,
            json!({
                "type": "mcp_call",
                "id": "mcp_equivalent",
                "status": "in_progress",
                "arguments": "",
                "name": "lookup",
                "server_label": "docs",
                "output": null,
                "error": null
            }),
        ),
        added(
            6,
            5,
            json!({
                "type": "code_interpreter_call",
                "id": "ci_equivalent",
                "container_id": "container_equivalent",
                "status": "in_progress",
                "code": "",
                "outputs": []
            }),
        ),
        indexed_event(
            "response.reasoning_summary_text.delta",
            7,
            "rs_equivalent",
            0,
            json!({"summary_index": 0, "delta": "pl"}),
        )?,
        indexed_event(
            "response.reasoning_summary_text.done",
            8,
            "rs_equivalent",
            0,
            json!({"summary_index": 0, "text": "plan"}),
        )?,
        indexed_event(
            "response.content_part.added",
            9,
            "msg_equivalent",
            1,
            json!({
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": [],
                    "logprobs": []
                }
            }),
        )?,
        indexed_event(
            "response.output_text.delta",
            10,
            "msg_equivalent",
            1,
            json!({
                "content_index": 0,
                "delta": "ans",
                "logprobs": [{
                    "token": "ans",
                    "logprob": -0.2,
                    "top_logprobs": []
                }]
            }),
        )?,
        indexed_event(
            "response.output_text.annotation.added",
            11,
            "msg_equivalent",
            1,
            json!({
                "content_index": 0,
                "annotation_index": 0,
                "annotation": annotation
            }),
        )?,
        indexed_event(
            "response.output_text.done",
            12,
            "msg_equivalent",
            1,
            json!({"content_index": 0, "text": "answer"}),
        )?,
        indexed_event(
            "response.content_part.done",
            13,
            "msg_equivalent",
            1,
            json!({"content_index": 0, "part": output[1]["content"][0].clone()}),
        )?,
        indexed_event(
            "response.content_part.added",
            14,
            "msg_equivalent",
            1,
            json!({
                "content_index": 1,
                "part": {"type": "refusal", "refusal": ""}
            }),
        )?,
        indexed_event(
            "response.refusal.delta",
            15,
            "msg_equivalent",
            1,
            json!({"content_index": 1, "delta": "can"}),
        )?,
        indexed_event(
            "response.refusal.done",
            16,
            "msg_equivalent",
            1,
            json!({"content_index": 1, "refusal": "cannot"}),
        )?,
        indexed_event(
            "response.content_part.done",
            17,
            "msg_equivalent",
            1,
            json!({"content_index": 1, "part": output[1]["content"][1].clone()}),
        )?,
        indexed_event(
            "response.web_search_call.in_progress",
            18,
            "ws_equivalent",
            2,
            json!({}),
        )?,
        indexed_event(
            "response.web_search_call.searching",
            19,
            "ws_equivalent",
            2,
            json!({}),
        )?,
        indexed_event(
            "response.web_search_call.completed",
            20,
            "ws_equivalent",
            2,
            json!({}),
        )?,
        indexed_event(
            "response.image_generation_call.in_progress",
            21,
            "ig_equivalent",
            3,
            json!({}),
        )?,
        indexed_event(
            "response.image_generation_call.generating",
            22,
            "ig_equivalent",
            3,
            json!({}),
        )?,
        indexed_event(
            "response.image_generation_call.partial_image",
            23,
            "ig_equivalent",
            3,
            json!({"partial_image_index": 0, "partial_image_b64": "cHJldmlldw=="}),
        )?,
        indexed_event(
            "response.image_generation_call.completed",
            24,
            "ig_equivalent",
            3,
            json!({}),
        )?,
        indexed_event(
            "response.mcp_call.in_progress",
            25,
            "mcp_equivalent",
            4,
            json!({}),
        )?,
        indexed_event(
            "response.mcp_call_arguments.delta",
            26,
            "mcp_equivalent",
            4,
            json!({"delta": "{\"query\":"}),
        )?,
        indexed_event(
            "response.mcp_call_arguments.done",
            27,
            "mcp_equivalent",
            4,
            json!({"arguments": "{\"query\":\"norn\"}"}),
        )?,
        indexed_event(
            "response.mcp_call.completed",
            28,
            "mcp_equivalent",
            4,
            json!({}),
        )?,
        indexed_event(
            "response.code_interpreter_call.in_progress",
            29,
            "ci_equivalent",
            5,
            json!({}),
        )?,
        indexed_event(
            "response.code_interpreter_call_code.delta",
            30,
            "ci_equivalent",
            5,
            json!({"delta": "print"}),
        )?,
        indexed_event(
            "response.code_interpreter_call_code.done",
            31,
            "ci_equivalent",
            5,
            json!({"code": "print('ok')"}),
        )?,
        indexed_event(
            "response.code_interpreter_call.interpreting",
            32,
            "ci_equivalent",
            5,
            json!({}),
        )?,
        indexed_event(
            "response.code_interpreter_call.completed",
            33,
            "ci_equivalent",
            5,
            json!({}),
        )?,
        done_item(34, 5, &output[5]),
        message_done.clone(),
        message_done,
        done_item(36, 3, &output[3]),
        done_item(37, 0, &output[0]),
        done_item(38, 4, &output[4]),
        done_item(39, 2, &output[2]),
        completed(40, output),
    ])
}

#[test]
fn streamed_and_terminal_only_paths_produce_identical_canonical_output() -> TestResult {
    let output = canonical_output();
    let streamed_events = map_frames(&streamed_frames(&output)?)?;
    let terminal_events = map_frames(&[completed(1, &output)])?;
    assert_eq!(projected_text(&streamed_events, Text), ["ans", "wer"]);
    assert_eq!(projected_text(&streamed_events, Refusal), ["can", "not"]);
    assert_eq!(
        projected_text(&streamed_events, Thinking),
        ["pl", "an", "detail"]
    );
    assert_eq!(projected_text(&terminal_events, Text), ["answer"]);
    assert_eq!(projected_text(&terminal_events, Refusal), ["cannot"]);
    assert_eq!(
        projected_text(&terminal_events, Thinking),
        ["plan", "detail"]
    );

    let streamed = crate::r#loop::assembly::assemble_response(&streamed_events)
        .ok_or_else(|| io::Error::other("streamed path did not assemble"))?;
    let terminal = crate::r#loop::assembly::assemble_response(&terminal_events)
        .ok_or_else(|| io::Error::other("terminal-only path did not assemble"))?;

    let streamed_raw = streamed
        .response_items
        .iter()
        .map(|item| item.item.raw())
        .collect::<Vec<_>>();
    let terminal_raw = terminal
        .response_items
        .iter()
        .map(|item| item.item.raw())
        .collect::<Vec<_>>();
    assert_eq!(streamed_raw, terminal_raw);
    assert_eq!(streamed_raw, output.iter().collect::<Vec<_>>());
    assert_eq!(streamed.text, terminal.text);
    assert_eq!(streamed.refusal, terminal.refusal);
    assert_eq!(streamed.thinking, terminal.thinking);
    assert_eq!(streamed.thinking, "plandetail");
    assert_eq!(streamed.reasoning, terminal.reasoning);
    assert_eq!(streamed.tool_calls.len(), terminal.tool_calls.len());
    assert_eq!(streamed.stop_reason, terminal.stop_reason);
    assert_eq!(
        serde_json::to_value(&streamed.usage)?,
        serde_json::to_value(&terminal.usage)?
    );
    assert_eq!(streamed.response_id, terminal.response_id);
    Ok(())
}
