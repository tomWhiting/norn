use std::error::Error;
use std::io;

use serde_json::{Value, json};

use super::*;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::response_item::ResponseItem;

mod codex_terminal;
mod equivalence;
mod terminal_boundaries;

type TestResult = Result<(), Box<dyn Error>>;

fn message(id: &str, content: &Value) -> Value {
    json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": "completed",
        "content": content,
    })
}

fn done_item(sequence: u64, output_index: u64, item: &Value) -> SseEvent {
    SseEvent {
        event_type: "response.output_item.done".to_owned(),
        data: json!({
            "type": "response.output_item.done",
            "sequence_number": sequence,
            "output_index": output_index,
            "item": item,
        }),
    }
}

fn completed(sequence: u64, output: &[Value]) -> SseEvent {
    SseEvent {
        event_type: "response.completed".to_owned(),
        data: json!({
            "type": "response.completed",
            "sequence_number": sequence,
            "response": {
                "id": "resp_test",
                "status": "completed",
                "output": output,
                "usage": {
                    "input_tokens": 7,
                    "input_tokens_details": {"cached_tokens": 1, "cache_write_tokens": 2},
                    "output_tokens": 3,
                    "output_tokens_details": {"reasoning_tokens": 2},
                    "total_tokens": 10
                },
            },
        }),
    }
}

fn failed(sequence: u64, output: &[Value], code: &str) -> SseEvent {
    SseEvent {
        event_type: "response.failed".to_owned(),
        data: json!({
            "type": "response.failed",
            "sequence_number": sequence,
            "response": {
                "id": "resp_failed",
                "status": "failed",
                "output": output,
                "error": {"code": code, "message": "authority detail is not rendered"},
            },
        }),
    }
}

fn only_ok(results: Vec<Result<ProviderEvent, ProviderError>>) -> TestResult {
    for result in results {
        if let Err(error) = result {
            return Err(io::Error::other(format!("unexpected provider error: {error}")).into());
        }
    }
    Ok(())
}

#[test]
fn failed_response_error_remains_authoritative_over_partial_tool_output() {
    let partial_call = json!({
        "type": "function_call",
        "call_id": "call_partial",
        "name": "read",
        "arguments": "{\"path\":",
        "status": "in_progress",
    });
    let mut mapper = ResponsesMapper::default();
    let preview = mapper.map_event(&done_item(0, 0, &partial_call));
    assert!(matches!(
        preview.as_slice(),
        [Ok(ProviderEvent::ResponseStreamEvent { .. })]
    ));
    let events = mapper.map_event(&failed(1, &[partial_call], "server_is_overloaded"));
    assert!(matches!(
        events.as_slice(),
        [
            Ok(ProviderEvent::ResponseStreamEvent { .. }),
            Ok(ProviderEvent::ResponseItemDone { .. }),
            Err(ProviderError::StreamError {
                transient: Some(crate::error::TransientKind::ServerError { status: 503 }),
                ..
            }),
        ]
    ));
}

#[test]
fn terminal_output_order_wins_over_completion_arrival_order() -> TestResult {
    let first = message(
        "msg_first",
        &json!([{"type": "output_text", "text": "first", "annotations": [], "logprobs": []}]),
    );
    let second = message(
        "msg_second",
        &json!([{"type": "output_text", "text": "second", "annotations": [], "logprobs": []}]),
    );
    let mut mapper = ResponsesMapper::default();

    only_ok(mapper.map_event(&done_item(0, 1, &second)))?;
    only_ok(mapper.map_event(&done_item(1, 0, &first)))?;
    let terminal = mapper.map_event(&completed(2, &[first, second]));

    let ids: Vec<_> = terminal
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderEvent::ResponseItemDone { item }) => item.item.id(),
            Ok(_) | Err(_) => None,
        })
        .collect();
    assert_eq!(ids, ["msg_first", "msg_second"]);
    assert!(matches!(
        terminal.last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
    Ok(())
}

#[test]
fn exact_duplicate_completion_never_duplicates_canonical_item() {
    let item = message(
        "msg_1",
        &json!([{"type": "output_text", "text": "hello", "annotations": [], "logprobs": []}]),
    );
    let frame = done_item(0, 0, &item);
    let mut mapper = ResponsesMapper::default();
    let first = mapper.map_event(&frame);
    let duplicate = mapper.map_event(&frame);
    assert_eq!(
        first
            .iter()
            .filter(|event| matches!(event, Ok(ProviderEvent::ResponseItemDone { .. })))
            .count(),
        0
    );
    assert_eq!(
        duplicate.len(),
        1,
        "duplicate retains only its raw envelope"
    );

    let terminal = mapper.map_event(&completed(1, &[item]));
    assert_eq!(
        terminal
            .iter()
            .filter(|event| matches!(event, Ok(ProviderEvent::ResponseItemDone { .. })))
            .count(),
        1
    );
}

#[test]
fn idless_tool_calls_reach_execution_with_call_ids_intact() -> TestResult {
    let function = json!({
        "type": "function_call",
        "call_id": "call_function",
        "name": "lookup",
        "arguments": "{}",
        "status": "completed"
    });
    let custom = json!({
        "type": "custom_tool_call",
        "call_id": "call_custom",
        "name": "patch",
        "input": "change",
        "status": "completed"
    });
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&completed(0, &[function, custom]));
    let items = events
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderEvent::ResponseItemDone { item }) => Some(item),
            Ok(_) | Err(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].provenance.item_id, None);
    assert_eq!(items[1].provenance.item_id, None);
    assert_eq!(
        items[0]
            .item
            .as_function_call()
            .ok_or("expected function call")?
            .call_id(),
        "call_function"
    );
    assert_eq!(
        items[1]
            .item
            .as_custom_tool_call()
            .ok_or("expected custom tool call")?
            .call_id(),
        "call_custom"
    );
    assert!(matches!(
        events.last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
    Ok(())
}

#[test]
fn unsupported_executable_item_is_retained_before_typed_failure() {
    let item = json!({
        "type": "local_shell_call",
        "id": "shell_1",
        "call_id": "call_shell_1",
        "status": "completed",
        "action": {"type": "exec", "command": ["pwd"], "env": {}},
    });
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&done_item(0, 0, &item));
    assert!(matches!(
        events.as_slice(),
        [
            Ok(ProviderEvent::ResponseStreamEvent { .. }),
            Ok(ProviderEvent::ResponseItemDone { .. }),
            Err(ProviderError::UnsupportedResponseItem),
        ]
    ));
}

#[test]
fn unknown_output_item_is_raw_then_retained_then_typed_unsupported() -> TestResult {
    let item = json!({
        "type": "future_output_item",
        "id": "future_item_1",
        "payload": {"kept": true},
    });
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&done_item(0, 0, &item));
    let (stream_event, retained) = match events.as_slice() {
        [
            Ok(ProviderEvent::ResponseStreamEvent { event }),
            Ok(ProviderEvent::ResponseItemDone { item }),
            Err(ProviderError::UnsupportedResponseItem),
        ] => (event, item),
        other => {
            return Err(io::Error::other(format!(
                "unexpected item sequence with {} entries",
                other.len()
            ))
            .into());
        }
    };
    assert_eq!(stream_event.raw().get("item"), Some(&item));
    assert_eq!(retained.item.raw(), &item);
    assert!(matches!(retained.item, ResponseItem::Opaque(_)));
    Ok(())
}

/// Sends a benign known event through the mapper and asserts the stream is
/// still alive: every mapped entry is `Ok` — no terminal latch, no error.
fn assert_stream_still_alive(mapper: &mut ResponsesMapper, sequence_number: u64) -> TestResult {
    let follow_up = SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": sequence_number,
            "output_index": 0,
            "item": {
                "id": "msg_alive",
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }
        }),
    };
    let events = mapper.map_event(&follow_up);
    if events.iter().any(Result::is_err) {
        return Err(io::Error::other(
            "known event after a skipped unknown event did not map cleanly",
        )
        .into());
    }
    Ok(())
}

/// Unknown stream events are retained losslessly and SKIPPED — never fatal.
///
/// Codex-reference precedent: `codex-rs/codex-api/src/sse/responses.rs`
/// ignores unrecognized event types. Before this policy, an unknown frame
/// latched the mapper terminal and returned a fatal
/// `UnsupportedResponseEvent` — which is how a single undocumented frame
/// mid-stream killed live agents in the 2026-07-24 incident. Terminal
/// safety is preserved downstream: a stream that never delivers a known
/// terminal event produces no `Done`, and assembly fails the turn loudly.
#[test]
fn unknown_event_is_retained_and_skipped_without_killing_the_stream() -> TestResult {
    let raw = json!({
        "type": "response.future.delta",
        "sequence_number": 11,
        "payload": {"kept": true},
    });
    let event = SseEvent {
        event_type: "response.future.delta".to_owned(),
        data: raw.clone(),
    };
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&event);
    let raw_event = match events.as_slice() {
        [Ok(ProviderEvent::ResponseStreamEvent { event })] => event,
        other => {
            return Err(io::Error::other(format!(
                "unexpected event sequence with {} entries",
                other.len()
            ))
            .into());
        }
    };
    assert_eq!(raw_event.raw(), &raw);
    assert_stream_still_alive(&mut mapper, 12)
}

/// Replays the captured 2026-07-24 Variant-A strike frame: an undocumented
/// `keepalive` event the server emits during long server-side gaps (observed
/// mid-hosted-web-search under high reasoning effort). `keepalive` is in
/// neither the pinned public schema nor the pinned Codex overlay sources, so
/// it rides the unknown-event skip policy; this fixture pins that the exact
/// captured frame no longer kills the stream.
#[test]
fn captured_keepalive_frame_is_skipped_and_the_stream_survives() -> TestResult {
    let raw = json!({"type": "keepalive", "sequence_number": 22});
    let event = SseEvent {
        event_type: "keepalive".to_owned(),
        data: raw.clone(),
    };
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&event);
    let raw_event = match events.as_slice() {
        [Ok(ProviderEvent::ResponseStreamEvent { event })] => event,
        other => {
            return Err(io::Error::other(format!(
                "unexpected keepalive sequence with {} entries",
                other.len()
            ))
            .into());
        }
    };
    assert_eq!(raw_event.raw(), &raw);
    assert_stream_still_alive(&mut mapper, 23)
}

#[test]
fn output_text_delta_logprobs_remain_on_the_raw_preview_envelope() -> TestResult {
    let mut mapper = ResponsesMapper::default();
    only_ok(mapper.map_event(&SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "id": "msg_logprobs",
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }
        }),
    }))?;
    let raw = json!({
        "type": "response.output_text.delta",
        "sequence_number": 1,
        "item_id": "msg_logprobs",
        "output_index": 0,
        "content_index": 0,
        "delta": "answer",
        "logprobs": [{
            "token": "answer",
            "logprob": -0.1,
            "top_logprobs": [{"token": "Answer", "logprob": -0.2}]
        }]
    });
    let events = mapper.map_event(&SseEvent {
        event_type: "response.output_text.delta".to_owned(),
        data: raw.clone(),
    });
    let [
        Ok(ProviderEvent::ResponseStreamEvent { event }),
        Ok(ProviderEvent::TextDelta { text }),
    ] = events.as_slice()
    else {
        return Err(io::Error::other("unexpected output-text delta projection").into());
    };
    assert_eq!(event.raw(), &raw);
    assert_eq!(text, "answer");
    Ok(())
}

#[test]
fn known_audio_event_is_raw_then_typed_with_decoded_bytes() -> TestResult {
    let raw = json!({
        "type": "response.audio.delta",
        "sequence_number": 12,
        "response_id": "resp_example_only",
        "delta": "YXVkaW8=",
    });
    let event = SseEvent {
        event_type: "response.audio.delta".to_owned(),
        data: raw.clone(),
    };
    let mut mapper = ResponsesMapper::default();
    let events = mapper.map_event(&event);
    let raw_event = match events.as_slice() {
        [
            Ok(ProviderEvent::ResponseStreamEvent { event }),
            Ok(ProviderEvent::ResponseAudioFrame {
                stream_event: _,
                event:
                    ResponseAudioEvent::AudioDelta {
                        sequence_number: 12,
                        bytes: _,
                    },
            }),
        ] => event,
        other => {
            return Err(io::Error::other(format!(
                "unexpected audio event sequence with {} entries",
                other.len()
            ))
            .into());
        }
    };
    assert_eq!(raw_event.raw(), &raw);
    let [
        _,
        Ok(ProviderEvent::ResponseAudioFrame {
            stream_event,
            event,
        }),
    ] = events.as_slice()
    else {
        return Err(io::Error::other("typed audio frame missing").into());
    };
    assert_eq!(stream_event.as_ref(), raw_event.as_ref());
    assert_eq!(stream_event.raw(), raw_event.raw());
    assert_eq!(event.sequence_number(), 12);
    assert!(matches!(
        event,
        ResponseAudioEvent::AudioDelta { bytes, .. } if bytes == b"audio"
    ));
    Ok(())
}

#[test]
fn exact_duplicate_audio_sequence_remains_raw_only() {
    let wire = SseEvent {
        event_type: "response.audio.done".to_owned(),
        data: json!({
            "type": "response.audio.done",
            "sequence_number": 1,
        }),
    };
    let mut mapper = ResponsesMapper::default();
    assert_eq!(mapper.map_event(&wire).len(), 2);
    let duplicate = mapper.map_event(&wire);
    assert!(matches!(
        duplicate.as_slice(),
        [Ok(ProviderEvent::ResponseStreamEvent { .. })]
    ));
}

#[test]
fn refusal_remains_separate_from_output_text_after_reconciliation() -> TestResult {
    let item = message(
        "msg_1",
        &json!([
            {"type": "output_text", "text": "I can explain. ", "annotations": [], "logprobs": []},
            {"type": "refusal", "refusal": "I cannot do that."}
        ]),
    );
    let mut mapper = ResponsesMapper::default();
    let added = SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": [], "status": "in_progress"},
        }),
    };
    let text = SseEvent {
        event_type: "response.output_text.delta".to_owned(),
        data: json!({
            "type": "response.output_text.delta",
            "sequence_number": 1,
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "delta": "I can explain. ",
            "logprobs": [],
        }),
    };
    let refusal = SseEvent {
        event_type: "response.refusal.delta".to_owned(),
        data: json!({
            "type": "response.refusal.delta",
            "sequence_number": 2,
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 1,
            "delta": "I cannot do that.",
        }),
    };
    let refusal_done = SseEvent {
        event_type: "response.refusal.done".to_owned(),
        data: json!({
            "type": "response.refusal.done",
            "sequence_number": 3,
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 1,
            "refusal": "I cannot do that.",
        }),
    };
    let mut provider_events = Vec::new();
    for frame in [added, text, refusal, refusal_done, done_item(4, 0, &item)] {
        for event in mapper.map_event(&frame) {
            provider_events.push(event?);
        }
    }
    for event in mapper.map_event(&completed(5, &[item])) {
        provider_events.push(event?);
    }
    let response = crate::r#loop::assembly::assemble_response(&provider_events)
        .ok_or_else(|| io::Error::other("reconciled response did not assemble"))?;
    assert_eq!(response.text, "I can explain. ");
    assert_eq!(response.refusal.as_deref(), Some("I cannot do that."));
    Ok(())
}
