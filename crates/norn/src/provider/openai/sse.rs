//! SSE event parsing and mapping to `ProviderEvent`.
//!
//! Splits the streaming protocol surface across two files:
//!
//! * `super::sse_parser` — fail-closed raw byte-stream parsing, re-exported
//!   here as [`SseParser`].
//! * `sse.rs` (this file) — legacy lifecycle/delta UI projections through
//!   [`map_sse_event`].
//! * `super::sse_completed_item` — lossless completed-item capture.
//! * `super::sse_types` — typed deserialization targets for
//!   `output_item.done` / `response.failed` / `response.incomplete`
//!   payloads, plus the `classify_failed_error` error-code classifier
//!   and the `Retry-After` regex parser.
//!
//! The split keeps this file under the project's 500-LOC-per-file
//! production budget without touching the wire protocol surface.

use serde::Deserialize;

use super::sse_completed_item::map_completed_item;
pub use super::sse_parser::{SseEvent, SseParseError, SseParser};
use super::sse_types::{ResponseFailedPayload, classify_failed_error, incomplete_stop_reason};
use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::request::ToolCallKind;
use crate::provider::usage::Usage;

/// Parses a complete SSE transcript into a sequence of `SseEvent` values.
///
/// Test-support convenience over [`SseParser`] (a single `feed` followed by
/// the EOF [`SseParser::finish`] cleanup) — the production streaming path
/// never has the whole transcript in hand, so this exists only for tests
/// that replay recorded transcripts through the real parser.
#[cfg(test)]
pub(crate) fn parse_sse_bytes(raw: &str) -> Vec<SseEvent> {
    let mut parser = SseParser::new();
    let mut events = parser.feed(raw.as_bytes());
    events.extend(parser.finish());
    events
}

/// Maps an `SseEvent` to an optional `ProviderEvent`.
///
/// Unrecognized event types produce `None` and a structural debug record that
/// never copies the authority-controlled discriminator into ordinary logs.
pub(crate) fn map_sse_event(event: &SseEvent) -> Option<Result<ProviderEvent, ProviderError>> {
    match event.event_type.as_str() {
        "response.output_text.delta" => {
            let text = event
                .data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Ok(ProviderEvent::TextDelta { text }))
        }

        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let text = event
                .data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Ok(ProviderEvent::ThinkingDelta { text }))
        }

        "response.refusal.delta" | "response.refusal.done" => {
            let complete = event.event_type == "response.refusal.done";
            let event_type = if complete {
                "response.refusal.done"
            } else {
                "response.refusal.delta"
            };
            let text_field = if complete { "refusal" } else { "delta" };
            let required_string = |field: &'static str| {
                event
                    .data
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| ProviderError::ResponseParseError {
                        reason: format!("{event_type} missing required `{field}`"),
                    })
            };
            let required_index = |field: &'static str| {
                event
                    .data
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| ProviderError::ResponseParseError {
                        reason: format!("{event_type} missing required `{field}`"),
                    })
            };
            let mapped = (|| {
                let item_id = required_string("item_id")?;
                let output_index = required_index("output_index")?;
                let content_index = required_index("content_index")?;
                let refusal = required_string(text_field)?;
                if complete {
                    Ok(ProviderEvent::RefusalComplete {
                        item_id,
                        output_index,
                        content_index,
                        refusal,
                    })
                } else {
                    Ok(ProviderEvent::RefusalDelta {
                        item_id,
                        output_index,
                        content_index,
                        refusal,
                    })
                }
            })();
            Some(mapped)
        }

        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
            let delta = event
                .data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // The streaming `item_id` (`fc_*` on the wire, or its `id` alias
            // on older payloads) is the assembly merge key. Do NOT fall back
            // to `call_id` — that is a semantically distinct identifier
            // (`call_*`, the correlation key for `function_call_output`) and
            // collapsing the two corrupts downstream echo. If neither
            // `item_id` nor `id` is present, the delta cannot be merged and
            // is dropped with a warning.
            let Some(item_id) = event
                .data
                .get("item_id")
                .or_else(|| event.data.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
            else {
                tracing::warn!("tool call delta missing item_id/id, skipping");
                return None;
            };
            // The SSE event type discriminates structured `function_call`
            // deltas from freeform `custom_tool_call` deltas. The kind is
            // carried so assembly can pick the right wire envelope on echo
            // even if the matching `output_item.done` event never arrives.
            let kind = if event.event_type == "response.custom_tool_call_input.delta" {
                ToolCallKind::Custom
            } else {
                ToolCallKind::Function
            };
            // `call_id` is left `None` here: this stateless dispatcher does
            // not hold the per-response `item_id`->`call_id` correlation. The
            // stateful [`ResponsesMapper`](super::execute) stamps it from the
            // `response.output_item.added` event that announced this item.
            Some(Ok(ProviderEvent::ToolCallDelta {
                item_id,
                call_id: None,
                name: None,
                arguments_delta: delta,
                kind,
            }))
        }

        "response.output_item.done" => Some(map_completed_item(event)),

        "response.failed" => {
            // The error nests under `response.error`. Reject a malformed
            // shape with a local parse error; a structurally valid payload
            // with no error object remains a generic terminal stream error.
            let error_detail = match ResponseFailedPayload::deserialize(&event.data) {
                Ok(payload) => payload.response.and_then(|response| response.error),
                Err(_) => {
                    return Some(Err(ProviderError::ResponseParseError {
                        reason: "response.failed payload did not match the expected structure"
                            .to_owned(),
                    }));
                }
            };
            let err = match error_detail {
                Some(detail) => classify_failed_error(&detail),
                None => ProviderError::StreamError {
                    reason: "response.failed".to_string(),
                    transient: None,
                },
            };
            Some(Err(err))
        }

        "error" => Some(Err(ProviderError::StreamError {
            reason: "provider returned a standalone Responses error event".to_owned(),
            transient: None,
        })),

        "response.incomplete" => {
            // A `response.incomplete` event is the Responses API's terminal
            // frame for a deterministic model-side stop with partial output
            // (`max_output_tokens` / `content_filter`). It is NOT a
            // transport error: the stream completes normally with a typed
            // `Done` event so the agent loop classifies the turn as
            // `ResponseClass::Truncated` and surfaces
            // `AgentStepResult::Truncated` — a stopped run with partial
            // output — instead of failing a stop that would recur on every
            // retry. Text deltas already emitted are preserved by assembly;
            // usage and the response id are read from the same nested
            // `response` object that `response.completed` carries. The
            // reason nests under `response.incomplete_details.reason`;
            // unknown reasons surface as a terminal error (see
            // `incomplete_stop_reason`).
            let reason = match ResponseFailedPayload::deserialize(&event.data) {
                Ok(payload) => payload
                    .response
                    .and_then(|response| response.incomplete_details)
                    .and_then(|details| details.reason),
                Err(_) => {
                    return Some(Err(ProviderError::ResponseParseError {
                        reason: "response.incomplete payload did not match the expected structure"
                            .to_owned(),
                    }));
                }
            };
            match incomplete_stop_reason(reason.as_deref()) {
                Ok(stop_reason) => Some(Ok(ProviderEvent::Done {
                    stop_reason,
                    usage: extract_usage(&event.data),
                    response_id: extract_response_id(&event.data),
                })),
                Err(err) => Some(Err(err)),
            }
        }

        "response.output_text.done" => {
            let text = event
                .data
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Ok(ProviderEvent::TextComplete { text }))
        }

        "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
            let text = event
                .data
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Ok(ProviderEvent::ThinkingComplete { text }))
        }

        "response.created"
        | "response.output_item.added"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.function_call_arguments.done"
        | "response.custom_tool_call_input.done"
        | "response.web_search_call.in_progress"
        | "response.web_search_call.searching"
        | "response.web_search_call.completed"
        | "response.in_progress"
        | "response.queued"
        | "response.metadata" => None,

        _ => {
            tracing::debug!("unrecognized SSE event type, skipping");
            None
        }
    }
}

fn extract_usage(data: &serde_json::Value) -> Usage {
    let usage_obj = data
        .get("response")
        .and_then(|r| r.get("usage"))
        .or_else(|| data.get("usage"));

    let Some(u) = usage_obj else {
        return Usage::default();
    };

    Usage {
        input_tokens: u
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: u
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: u
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

/// Extracts the server-assigned response id from the nested `response`
/// object carried by `response.completed` and `response.incomplete` events.
fn extract_response_id(data: &serde_json::Value) -> Option<String> {
    data.get("response")
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// Extracts the `(item_id, call_id)` correlation pair announced by a
/// `response.output_item.added` event for a tool-call item.
///
/// The Responses API announces every output item with an
/// `output_item.added` event before streaming that item's content; for
/// `function_call` / `custom_tool_call` items the announcement carries both
/// the streaming `item_id` (`fc_*` / `ctc_*`) and the `call_id` (`call_*`)
/// the model expects on echoes. The stateful
/// [`ResponsesMapper`](super::execute::ResponsesMapper) records this pair so
/// it can stamp the `call_id` onto the item's subsequent
/// [`ProviderEvent::ToolCallDelta`] fragments.
///
/// Returns `None` for non-tool items (messages, reasoning) and for payloads
/// missing either id — no correlation is recorded, and a delta whose item was
/// never announced simply carries `call_id: None` (never a fabricated value).
pub(crate) fn output_item_added_call_id(event: &SseEvent) -> Option<(String, String)> {
    let item = event.data.get("item")?;
    let item_type = item.get("type").and_then(|v| v.as_str())?;
    if item_type != "function_call" && item_type != "custom_tool_call" {
        return None;
    }
    let item_id = item.get("id").and_then(|v| v.as_str())?;
    let call_id = item.get("call_id").and_then(|v| v.as_str())?;
    Some((item_id.to_owned(), call_id.to_owned()))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::*;
    use crate::error::TransientKind;
    use crate::provider::events::StopReason;

    #[test]
    fn parse_multi_frame_sse() {
        let raw = r#"event: response.output_text.delta
data: {"delta": "Hello"}

event: response.function_call_arguments.delta
data: {"item_id": "call_1", "delta": "{\"x\":1}"}

event: response.completed
data: {"response": {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 5}}}

"#;
        let events = parse_sse_bytes(raw);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "response.output_text.delta");
        assert_eq!(
            events[1].event_type,
            "response.function_call_arguments.delta"
        );
        assert_eq!(events[2].event_type, "response.completed");
    }

    #[test]
    fn comment_lines_skipped() {
        let raw = ": this is a comment\nevent: response.created\ndata: {}\n\n";
        let events = parse_sse_bytes(raw);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn text_delta_mapping() {
        let event = SseEvent {
            event_type: "response.output_text.delta".to_string(),
            data: serde_json::json!({"delta": "world"}),
        };
        let result = map_sse_event(&event);
        match result {
            Some(Ok(ProviderEvent::TextDelta { text })) => assert_eq!(text, "world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn thinking_delta_from_reasoning_summary() {
        let event = SseEvent {
            event_type: "response.reasoning_summary_text.delta".to_string(),
            data: serde_json::json!({"delta": "thinking..."}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ThinkingDelta { text })) => assert_eq!(text, "thinking..."),
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    #[test]
    fn thinking_delta_from_reasoning_text() {
        let event = SseEvent {
            event_type: "response.reasoning_text.delta".to_string(),
            data: serde_json::json!({"delta": "reasoning..."}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ThinkingDelta { text })) => assert_eq!(text, "reasoning..."),
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_delta_mapping() {
        let event = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_1", "delta": "{\"a\":"}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ToolCallDelta {
                item_id,
                call_id: _,
                name,
                arguments_delta,
                kind: _,
            })) => {
                assert_eq!(item_id, "fc_1");
                assert!(name.is_none());
                assert_eq!(arguments_delta, "{\"a\":");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    #[test]
    fn output_item_done_captures_function_call_with_both_identities() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "function_call",
                    "id": "fc_abc",
                    "call_id": "call_xyz",
                    "name": "get_weather",
                    "arguments": "{\"city\": \"NYC\"}"
                }
            }),
        };
        let mapped = map_sse_event(&done);
        assert!(
            matches!(&mapped, Some(Ok(ProviderEvent::ResponseItemDone { .. }))),
            "expected canonical function call, got {mapped:?}"
        );
        let Some(Ok(ProviderEvent::ResponseItemDone { item })) = mapped else {
            return;
        };
        let call = item.item.as_function_call();
        assert!(call.is_some(), "function call must remain typed");
        let Some(call) = call else {
            return;
        };
        assert_eq!(item.provenance.item_id.as_deref(), Some("fc_abc"));
        assert_eq!(call.call_id(), "call_xyz");
        assert_ne!(call.call_id(), "fc_abc");
        assert_eq!(call.name(), "get_weather");
        assert_eq!(call.arguments(), "{\"city\": \"NYC\"}");
    }

    #[test]
    fn output_item_done_captures_custom_tool_call_verbatim() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "custom_tool_call",
                    "id": "ctc_abc",
                    "call_id": "call_custom",
                    "name": "apply_patch",
                    "input": "*** BEGIN PATCH ***\n@@\n-foo\n+bar\n*** END PATCH ***",
                }
            }),
        };
        let mapped = map_sse_event(&done);
        assert!(
            matches!(&mapped, Some(Ok(ProviderEvent::ResponseItemDone { .. }))),
            "expected canonical custom tool call, got {mapped:?}"
        );
        let Some(Ok(ProviderEvent::ResponseItemDone { item })) = mapped else {
            return;
        };
        let call = item.item.as_custom_tool_call();
        assert!(call.is_some(), "custom tool call must remain typed");
        let Some(call) = call else {
            return;
        };
        assert_eq!(call.call_id(), "call_custom");
        assert_eq!(call.name(), "apply_patch");
        assert_eq!(
            call.input(),
            "*** BEGIN PATCH ***\n@@\n-foo\n+bar\n*** END PATCH ***",
            "freeform input must pass through verbatim",
        );
    }

    #[test]
    fn output_item_done_custom_tool_call_missing_call_id_is_error() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "custom_tool_call",
                    "id": "ctc_abc",
                    "name": "freeform",
                    "input": "anything",
                }
            }),
        };
        assert!(matches!(
            map_sse_event(&done),
            Some(Err(ProviderError::ResponseParseError { .. }))
        ));
    }

    #[test]
    fn output_item_done_reasoning_captures_full_item() {
        // Encrypted-reasoning seam: a `reasoning` output_item.done carries
        // the structured item — id, summary parts, content parts, and the
        // encrypted_content blob — through to a ReasoningItemDone event so
        // assembly can attach it to the assistant message for stateless
        // replay.
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "reasoning",
                    "id": "rs_abc",
                    "summary": [{"type": "summary_text", "text": "I considered the options"}],
                    "content": [{"type": "reasoning_text", "text": "raw chain of thought"}],
                    "encrypted_content": "gAAAAB-opaque-blob",
                }
            }),
        };
        let mapped = map_sse_event(&done);
        assert!(
            matches!(&mapped, Some(Ok(ProviderEvent::ResponseItemDone { .. }))),
            "expected canonical reasoning item, got {mapped:?}"
        );
        let Some(Ok(ProviderEvent::ResponseItemDone { item })) = mapped else {
            return;
        };
        let reasoning = item.item.as_reasoning();
        assert!(reasoning.is_some(), "reasoning item must remain typed");
        let Some(reasoning) = reasoning else {
            return;
        };
        assert_eq!(item.item.id(), Some("rs_abc"));
        assert_eq!(
            reasoning.summary(),
            &[serde_json::json!({
                "type": "summary_text",
                "text": "I considered the options"
            })],
        );
        assert_eq!(
            reasoning.content(),
            Some(
                [serde_json::json!({
                    "type": "reasoning_text",
                    "text": "raw chain of thought"
                })]
                .as_slice()
            ),
        );
        assert_eq!(reasoning.encrypted_content(), Some("gAAAAB-opaque-blob"),);
    }

    #[test]
    fn output_item_done_reasoning_without_encrypted_content_still_captured() {
        // store: true requests receive reasoning items without
        // encrypted_content — they are still captured (for observability
        // and assembly); the request serializer decides not to replay them.
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "reasoning",
                    "id": "rs_plain",
                    "summary": [],
                }
            }),
        };
        let mapped = map_sse_event(&done);
        assert!(
            matches!(&mapped, Some(Ok(ProviderEvent::ResponseItemDone { .. }))),
            "expected canonical reasoning item, got {mapped:?}"
        );
        let Some(Ok(ProviderEvent::ResponseItemDone { item })) = mapped else {
            return;
        };
        let reasoning = item.item.as_reasoning();
        assert!(reasoning.is_some(), "reasoning item must remain typed");
        let Some(reasoning) = reasoning else {
            return;
        };
        assert_eq!(item.item.id(), Some("rs_plain"));
        assert!(reasoning.summary().is_empty());
        assert!(reasoning.content().is_none());
        assert!(reasoning.encrypted_content().is_none());
    }

    #[test]
    fn output_item_done_compaction_emits_opaque_event() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "compaction",
                    "id": "cmp_1",
                    "encrypted_content": "enc_state",
                }
            }),
        };
        let mapped = map_sse_event(&done);
        assert!(
            matches!(&mapped, Some(Ok(ProviderEvent::ResponseItemDone { .. }))),
            "expected canonical compaction, got {mapped:?}"
        );
        let Some(Ok(ProviderEvent::ResponseItemDone { item })) = mapped else {
            return;
        };
        assert_eq!(item.item.item_type(), "compaction");
        assert_eq!(item.item.raw()["encrypted_content"], "enc_state");
    }

    #[test]
    fn function_call_delta_carries_function_kind() {
        // A function_call_arguments.delta produces a ToolCallDelta with
        // kind = Function so assembly classifies the slot correctly even
        // before the Complete arrives.
        let event = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_1", "delta": "{"}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ToolCallDelta { kind, .. })) => {
                assert_eq!(kind, ToolCallKind::Function);
            }
            other => panic!("expected function-kind ToolCallDelta, got {other:?}"),
        }
    }

    #[test]
    fn custom_tool_call_delta_carries_custom_kind() {
        // F5: custom_tool_call_input.delta carries kind = Custom so the
        // delta-only fallback path (no Complete event) still produces the
        // right kind on the AssembledToolCall.
        let event = SseEvent {
            event_type: "response.custom_tool_call_input.delta".to_string(),
            data: serde_json::json!({"item_id": "ctc_1", "delta": "*** Begin"}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ToolCallDelta { kind, .. })) => {
                assert_eq!(kind, ToolCallKind::Custom);
            }
            other => panic!("expected custom-kind ToolCallDelta, got {other:?}"),
        }
    }

    #[test]
    fn output_item_done_missing_call_id_is_error() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "function_call",
                    "id": "fc_abc",
                    "name": "get_weather",
                    "arguments": "{}"
                }
            }),
        };
        assert!(matches!(
            map_sse_event(&done),
            Some(Err(ProviderError::ResponseParseError { .. }))
        ));
    }

    #[test]
    fn deltas_accumulate_without_double_counting_from_done() {
        let d1 = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_abc", "delta": "{\"city\""}),
        };
        let d2 = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_abc", "delta": ": \"NYC\""}),
        };
        let d3 = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"item_id": "fc_abc", "delta": "}"}),
        };
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "function_call",
                    "id": "fc_abc",
                    "call_id": "call_xyz",
                    "name": "get_weather",
                    "arguments": "{\"city\": \"NYC\"}"
                }
            }),
        };

        let mut accumulated = String::new();
        for event in &[d1, d2, d3, done] {
            if let Some(Ok(ProviderEvent::ToolCallDelta {
                arguments_delta, ..
            })) = map_sse_event(event)
            {
                accumulated.push_str(&arguments_delta);
            }
        }

        assert_eq!(accumulated, "{\"city\": \"NYC\"}");
    }

    #[test]
    fn completed_event_is_owned_by_the_stateful_terminal_mapper() {
        let event = SseEvent {
            event_type: "response.completed".to_string(),
            data: serde_json::json!({
                "response": {
                    "status": "completed",
                    "usage": {"input_tokens": 100, "output_tokens": 50}
                }
            }),
        };
        assert!(map_sse_event(&event).is_none());
    }

    // Helper: build a `response.failed` event whose error nests under
    // `response.error`, matching the real Responses API wire format.
    fn failed_event(code: Option<&str>, message: Option<&str>) -> SseEvent {
        let mut error = serde_json::Map::new();
        if let Some(c) = code {
            error.insert("code".to_string(), serde_json::json!(c));
        }
        if let Some(m) = message {
            error.insert("message".to_string(), serde_json::json!(m));
        }
        SseEvent {
            event_type: "response.failed".to_string(),
            data: serde_json::json!({ "response": { "error": error } }),
        }
    }

    #[test]
    fn failed_event_mapping() {
        // #20: existing test updated to the correctly nested structure. With
        // no error code the classifier degrades to a terminal StreamError
        // with a fixed local reason; provider text is never exposed.
        let event = failed_event(None, Some("rate limit exceeded"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason, transient })) => {
                assert_eq!(
                    reason,
                    "provider returned response.failed without an error code"
                );
                assert_eq!(transient, None);
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_reads_code_and_message_from_nested_response() {
        // #1: both code and message are extracted from the nested location.
        // `server_is_overloaded` carries the retry-eligible structured
        // 503 server-error kind and a fixed local reason.
        let event = failed_event(Some("server_is_overloaded"), Some("overloaded; retry"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason, transient })) => {
                assert_eq!(reason, "provider reported server_is_overloaded");
                assert_eq!(transient, Some(TransientKind::ServerError { status: 503 }));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_context_length_exceeded_maps_to_context_window() {
        // #2
        let event = failed_event(Some("context_length_exceeded"), Some("too long"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::ContextWindowExceeded)) => {}
            other => panic!("expected ContextWindowExceeded, got {other:?}"),
        }
    }

    #[test]
    fn failed_rate_limit_parses_retry_after() {
        // #3
        let event = failed_event(
            Some("rate_limit_exceeded"),
            Some("Rate limit reached. Please try again in 11.054s."),
        );
        match map_sse_event(&event) {
            Some(Err(ProviderError::RateLimited { retry_after })) => {
                assert_eq!(retry_after, Some(Duration::from_secs_f64(11.054)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn failed_insufficient_quota_maps_to_quota_exceeded() {
        // #4
        let event = failed_event(Some("insufficient_quota"), Some("no credit"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::QuotaExceeded)) => {}
            other => panic!("expected QuotaExceeded, got {other:?}"),
        }
    }

    #[test]
    fn failed_invalid_prompt_maps_to_invalid_request() {
        // #5
        let event = failed_event(Some("invalid_prompt"), Some("Your prompt was invalid."));
        match map_sse_event(&event) {
            Some(Err(ProviderError::InvalidRequest { message })) => {
                assert_eq!(message, "provider rejected the request as invalid_prompt");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn failed_cyber_policy_maps_to_invalid_request() {
        // #6
        let event = failed_event(Some("cyber_policy"), Some("Flagged for security review."));
        match map_sse_event(&event) {
            Some(Err(ProviderError::InvalidRequest { message })) => {
                assert_eq!(
                    message,
                    "provider rejected the request under its cybersecurity policy"
                );
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn failed_server_overloaded_is_structurally_retryable_server_error() {
        // #7: server_is_overloaded is transient back-pressure. It must carry
        // the structured 503 server-error kind so the public taxonomy (and
        // the loop-level retry policy derived from it) classifies it as a
        // retryable `RetryableError::ServerError`.
        let event = failed_event(Some("server_is_overloaded"), Some("busy"));
        match map_sse_event(&event) {
            Some(Err(err @ ProviderError::StreamError { .. })) => {
                assert_eq!(
                    err.class(),
                    crate::error::ErrorClass::Retryable {
                        kind: TransientKind::ServerError { status: 503 },
                    },
                );
                assert!(!err.to_string().contains("busy"));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_server_overloaded_empty_message_uses_fallback() {
        // Missing/blank message still yields a descriptive reason and the
        // retry-eligible structured kind.
        let event = failed_event(Some("server_is_overloaded"), None);
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason, transient })) => {
                assert_eq!(reason, "provider reported server_is_overloaded");
                assert_eq!(transient, Some(TransientKind::ServerError { status: 503 }));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_slow_down_is_structurally_retryable_server_error() {
        // #7b: slow_down is also transient back-pressure — same structured
        // classification so the retry policy picks it up identically.
        let event = failed_event(Some("slow_down"), Some("rate"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason, transient })) => {
                assert_eq!(reason, "provider reported slow_down");
                assert_eq!(transient, Some(TransientKind::ServerError { status: 503 }));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_slow_down_empty_message_uses_fallback() {
        let event = failed_event(Some("slow_down"), None);
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason, transient })) => {
                assert_eq!(reason, "provider reported slow_down");
                assert_eq!(transient, Some(TransientKind::ServerError { status: 503 }));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_unknown_code_redacts_message_and_stays_terminal() {
        // #8: unknown codes never set `transient` — opting an unknown error
        // into automatic retry would silently amplify novel failure modes.
        let event = failed_event(Some("some_future_error"), Some("weird failure"));
        match map_sse_event(&event) {
            Some(Err(err @ ProviderError::StreamError { .. })) => {
                assert!(!err.to_string().contains("weird failure"));
                assert!(!err.is_retryable(), "unknown codes must stay terminal");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    fn mapped_terminal_error(event: &SseEvent) -> Result<ProviderError, io::Error> {
        match map_sse_event(event) {
            Some(Err(error)) => Ok(error),
            _ => Err(io::Error::other(
                "terminal wire event did not map to a provider error",
            )),
        }
    }

    fn extract_opaque_tag<'a>(reason: &'a str, prefix: &str) -> Result<&'a str, io::Error> {
        reason
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| io::Error::other("terminal diagnostic omitted its opaque tag"))
    }

    #[test]
    fn distinct_unknown_terminal_values_survive_wire_dispatch_opaquely()
    -> Result<(), Box<dyn std::error::Error>> {
        let first_failed =
            mapped_terminal_error(&failed_event(Some("private-failed-first\r\nsecret"), None))?;
        let second_failed =
            mapped_terminal_error(&failed_event(Some("private-failed-second\r\nsecret"), None))?;
        let ProviderError::StreamError {
            reason: first_failed_reason,
            transient: None,
        } = &first_failed
        else {
            return Err(io::Error::other("first failed code was not terminal").into());
        };
        let ProviderError::StreamError {
            reason: second_failed_reason,
            transient: None,
        } = &second_failed
        else {
            return Err(io::Error::other("second failed code was not terminal").into());
        };
        let failed_prefix = "provider returned unknown response.failed code [opaque:";
        let first_failed_tag = extract_opaque_tag(first_failed_reason, failed_prefix)?;
        let second_failed_tag = extract_opaque_tag(second_failed_reason, failed_prefix)?;

        let incomplete_event = |reason: &str| SseEvent {
            event_type: "response.incomplete".to_owned(),
            data: serde_json::json!({
                "response": {"incomplete_details": {"reason": reason}}
            }),
        };
        let first_incomplete =
            mapped_terminal_error(&incomplete_event("private-incomplete-first\r\nsecret"))?;
        let second_incomplete =
            mapped_terminal_error(&incomplete_event("private-incomplete-second\r\nsecret"))?;
        let ProviderError::ResponseParseError {
            reason: first_incomplete_reason,
        } = &first_incomplete
        else {
            return Err(io::Error::other("first incomplete reason was not terminal").into());
        };
        let ProviderError::ResponseParseError {
            reason: second_incomplete_reason,
        } = &second_incomplete
        else {
            return Err(io::Error::other("second incomplete reason was not terminal").into());
        };
        let incomplete_prefix = "response.incomplete carried an unknown reason [opaque:";
        let first_incomplete_tag = extract_opaque_tag(first_incomplete_reason, incomplete_prefix)?;
        let second_incomplete_tag =
            extract_opaque_tag(second_incomplete_reason, incomplete_prefix)?;

        for tag in [
            first_failed_tag,
            second_failed_tag,
            first_incomplete_tag,
            second_incomplete_tag,
        ] {
            assert_eq!(tag.len(), 64);
            assert!(tag.chars().all(|character| character.is_ascii_hexdigit()));
        }
        assert_ne!(first_failed_tag, second_failed_tag);
        assert_ne!(first_incomplete_tag, second_incomplete_tag);
        for rendered in [
            first_failed.to_string(),
            second_failed.to_string(),
            first_incomplete.to_string(),
            second_incomplete.to_string(),
        ] {
            assert!(!rendered.contains("private-"));
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains('\r'));
            assert!(!rendered.contains('\n'));
        }
        assert_eq!(first_failed.class(), crate::error::ErrorClass::Terminal);
        assert_eq!(second_failed.class(), crate::error::ErrorClass::Terminal);
        assert_eq!(first_incomplete.class(), crate::error::ErrorClass::Terminal);
        assert_eq!(
            second_incomplete.class(),
            crate::error::ErrorClass::Terminal
        );
        Ok(())
    }

    #[test]
    fn malformed_terminal_payloads_fail_with_local_non_disclosing_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let failed = mapped_terminal_error(&SseEvent {
            event_type: "response.failed".to_owned(),
            data: serde_json::json!({"response": "sentinel-private-failed-shape\u{1b}"}),
        })?;
        let incomplete = mapped_terminal_error(&SseEvent {
            event_type: "response.incomplete".to_owned(),
            data: serde_json::json!({"response": "sentinel-private-incomplete-shape\u{1b}"}),
        })?;

        let failed_rendered = failed.to_string();
        let incomplete_rendered = incomplete.to_string();
        assert!(failed_rendered.contains("did not match the expected structure"));
        assert!(incomplete_rendered.contains("did not match the expected structure"));
        assert!(!failed_rendered.contains("sentinel-private"));
        assert!(!incomplete_rendered.contains("sentinel-private"));
        assert!(!failed_rendered.contains('\u{1b}'));
        assert!(!incomplete_rendered.contains('\u{1b}'));
        assert_eq!(failed.class(), crate::error::ErrorClass::Terminal);
        assert_eq!(incomplete.class(), crate::error::ErrorClass::Terminal);
        Ok(())
    }

    #[test]
    fn incomplete_max_output_tokens_completes_with_typed_stop() {
        // BLOCKER regression: a `response.incomplete` event with reason
        // `max_output_tokens` is a deterministic model-side stop, NOT a
        // stream error. It must complete the stream with
        // `StopReason::MaxTokens` and carry the usage and response id from
        // the nested `response` object so the loop classifies the turn as
        // Truncated instead of failing (and retrying) the call.
        let event = SseEvent {
            event_type: "response.incomplete".to_string(),
            data: serde_json::json!({
                "response": {
                    "id": "resp_incomplete_1",
                    "status": "incomplete",
                    "incomplete_details": {"reason": "max_output_tokens"},
                    "usage": {"input_tokens": 7, "output_tokens": 9}
                }
            }),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            })) => {
                assert_eq!(stop_reason, StopReason::MaxTokens);
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 9);
                assert_eq!(response_id.as_deref(), Some("resp_incomplete_1"));
            }
            other => panic!("expected Done with MaxTokens, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_content_filter_completes_with_typed_stop() {
        let event = SseEvent {
            event_type: "response.incomplete".to_string(),
            data: serde_json::json!({
                "response": {
                    "id": "resp_incomplete_2",
                    "status": "incomplete",
                    "incomplete_details": {"reason": "content_filter"},
                    "usage": {"input_tokens": 3, "output_tokens": 4}
                }
            }),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            })) => {
                assert_eq!(stop_reason, StopReason::ContentFilter);
                assert_eq!(usage.input_tokens, 3);
                assert_eq!(usage.output_tokens, 4);
                assert_eq!(response_id.as_deref(), Some("resp_incomplete_2"));
            }
            other => panic!("expected Done with ContentFilter, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_unknown_reason_is_terminal_parse_error() {
        // An unrecognized incomplete reason must NOT be guessed as
        // MaxTokens (dishonest) and must NOT be retryable (the stop is
        // deterministic). It surfaces as a terminal ResponseParseError
        // without copying the authority-controlled reason.
        let event = SseEvent {
            event_type: "response.incomplete".to_string(),
            data: serde_json::json!({
                "response": {
                    "incomplete_details": {"reason": "some_future_reason"}
                }
            }),
        };
        match map_sse_event(&event) {
            Some(Err(err @ ProviderError::ResponseParseError { .. })) => {
                assert!(!err.to_string().contains("some_future_reason"));
                assert!(
                    !err.is_retryable(),
                    "a deterministic incomplete stop must never classify retryable"
                );
            }
            other => panic!("expected terminal ResponseParseError, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_missing_reason_is_terminal_parse_error() {
        // No incomplete_details.reason at all: same refusal to guess.
        for data in [
            serde_json::json!({"response": {"incomplete_details": {}}}),
            serde_json::json!({"response": {}}),
            serde_json::json!({}),
        ] {
            let event = SseEvent {
                event_type: "response.incomplete".to_string(),
                data,
            };
            match map_sse_event(&event) {
                Some(Err(err @ ProviderError::ResponseParseError { .. })) => {
                    assert!(!err.is_retryable());
                }
                other => panic!("expected terminal ResponseParseError, got {other:?}"),
            }
        }
    }

    #[test]
    fn failed_payload_without_error_object_has_generic_terminal_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        // Structurally valid payloads without an error object remain terminal
        // and generic. Wrong-typed payloads are rejected by the separate
        // malformed-terminal-payload regression above.
        for data in [
            serde_json::json!({"unexpected": true}),
            serde_json::json!({"response": {"error": null}}),
        ] {
            let event = SseEvent {
                event_type: "response.failed".to_string(),
                data,
            };
            let Some(Err(ProviderError::StreamError { reason, transient })) = map_sse_event(&event)
            else {
                return Err(io::Error::other(
                    "response.failed without an error object was not a generic terminal error",
                )
                .into());
            };
            assert_eq!(reason, "response.failed");
            assert_eq!(transient, None);
        }
        Ok(())
    }

    #[test]
    fn unrecognized_event_skipped() {
        let event = SseEvent {
            event_type: "response.some_future_event".to_string(),
            data: serde_json::json!({}),
        };
        assert!(map_sse_event(&event).is_none());
    }

    #[test]
    fn parser_chunk_boundary_mid_data() {
        let mut parser = SseParser::new();
        let mut events = parser.feed(b"event: response.output_text.delta\ndata: {\"delta\":\"hel");
        events.extend(parser.feed(b"lo\"}\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "response.output_text.delta");
        assert_eq!(
            events[0].data.get("delta").and_then(|v| v.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn parser_two_frames_in_one_chunk() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: a\ndata: \"first\"\n\nevent: b\ndata: \"second\"\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "a");
        assert_eq!(events[0].data, serde_json::json!("first"));
        assert_eq!(events[1].event_type, "b");
        assert_eq!(events[1].data, serde_json::json!("second"));
    }

    #[test]
    fn parser_empty_chunk_between_valid_chunks() {
        let mut parser = SseParser::new();
        let mut events = parser.feed(b"event: foo\ndata: ");
        assert!(events.is_empty());
        events.extend(parser.feed(b""));
        assert!(events.is_empty());
        events.extend(parser.feed(b"{\"x\":1}\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "foo");
        assert_eq!(events[0].data, serde_json::json!({"x": 1}));
    }

    #[test]
    fn parser_utf8_codepoint_split_across_chunks() {
        // 'é' is 0xC3 0xA9 in UTF-8.
        let mut parser = SseParser::new();
        let mut events = parser.feed(b"event: e\ndata: \"\xc3");
        assert!(events.is_empty());
        events.extend(parser.feed(b"\xa9\"\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, serde_json::json!("\u{00e9}"));
    }

    #[test]
    fn parser_event_and_data_lines_in_separate_chunks() {
        let mut parser = SseParser::new();
        let mut events = parser.feed(b"event: response.output_text.delta\n");
        assert!(events.is_empty());
        events.extend(parser.feed(b"data: {\"delta\":\"x\"}\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "response.output_text.delta");
    }

    #[test]
    fn parser_crlf_line_endings() {
        let mut parser = SseParser::new();
        let events =
            parser.feed(b"event: response.output_text.delta\r\ndata: {\"delta\":\"crlf\"}\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "response.output_text.delta");
        assert_eq!(
            events[0].data.get("delta").and_then(|v| v.as_str()),
            Some("crlf")
        );
    }

    #[test]
    fn parser_double_newline_split_across_chunks() {
        let mut parser = SseParser::new();
        let mut events = parser.feed(b"event: a\ndata: {\"v\":1}\n");
        assert!(events.is_empty());
        events.extend(parser.feed(b"\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "a");
    }

    #[test]
    fn parser_finish_discards_trailing_frame() {
        // EOF is not an SSE event delimiter.
        let mut parser = SseParser::new();
        let early = parser.feed(b"event: a\ndata: {\"v\":1}\n");
        assert!(early.is_empty());
        let tail = parser.finish();
        assert!(tail.is_empty());
        assert_eq!(parser.error(), None);
    }

    #[test]
    fn parser_finish_with_no_trailing_state() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: a\ndata: {\"v\":1}\n\n");
        assert_eq!(events.len(), 1);
        let tail = parser.finish();
        assert!(tail.is_empty());
    }

    #[test]
    fn parser_data_only_frame_no_event_type() {
        // The R1 contract relaxes the event-type-non-empty guard; data-only
        // frames are emitted with an empty event_type.
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: {\"v\":1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "");
        assert_eq!(events[0].data, serde_json::json!({"v": 1}));
    }

    #[test]
    fn parser_comment_line_skipped() {
        let mut parser = SseParser::new();
        let events = parser.feed(b": ping\nevent: a\ndata: {\"v\":1}\n\n");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parser_empty_chunk_no_crash() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"");
        assert!(events.is_empty());
    }

    /// Regression test (final-state hardening, T1 item 8): per the SSE
    /// specification, successive `data:` lines within a frame concatenate
    /// joined by `\n`. The previous implementation replaced the buffer, so
    /// a multi-line payload was silently truncated to its final line.
    #[test]
    fn successive_data_lines_concatenate_with_newline() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: e\ndata: {\ndata: \"a\": 1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "e");
        assert_eq!(
            events[0].data,
            serde_json::json!({"a": 1}),
            "the two data lines must be joined by a newline and parsed as one payload"
        );
    }

    /// EOF discards a valid but unterminated multiline frame.
    #[test]
    fn successive_data_lines_without_blank_line_are_discarded() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: e\ndata: {\ndata: \"b\": 2}\n");
        assert!(events.is_empty());
        let tail = parser.finish();
        assert!(tail.is_empty());
        assert_eq!(parser.error(), None);
    }

    /// A corrupt non-sentinel frame is a typed terminal protocol fault. Events
    /// preceding it remain available, while later bytes cannot silently
    /// resume the poisoned stream.
    #[test]
    fn corrupt_frame_fails_closed_after_preceding_events() {
        let mut parser = SseParser::new();
        let events = parser.feed(
            b"event: a\ndata: {\"ok\": 1}\n\n\
              event: corrupt\ndata: {\"truncated\": \n\n\
              event: b\ndata: {\"ok\": 2}\n\n",
        );
        assert_eq!(events.len(), 1, "only the preceding frame may survive");
        assert_eq!(events[0].event_type, "a");
        assert_eq!(parser.error(), Some(SseParseError::InvalidJson));
        assert!(parser.feed(b"event: later\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn done_sentinel_frame_is_dropped() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: [DONE]\n\n");
        assert!(events.is_empty(), "[DONE] must not surface as an event");
    }

    #[test]
    fn corrupt_trailing_frame_is_discarded_at_eof() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: tail\ndata: {\"broken\":");
        assert!(events.is_empty());
        assert!(parser.finish().is_empty());
        assert_eq!(parser.error(), None);
    }

    /// Parser failures and mapper diagnostics never render authority bytes.
    #[test]
    fn frame_failures_are_typed_and_non_disclosing() {
        let mut parser = SseParser::new();
        assert!(parser.feed(b"data: [DONE]\n\n").is_empty());
        assert_eq!(parser.error(), None);

        let secret = "sentinel-private-frame-secret";
        assert!(
            parser
                .feed(format!("event: private\ndata: {{\"{secret}\": \n\n").as_bytes())
                .is_empty()
        );
        let rendered = parser
            .error()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(!rendered.contains(secret));
        assert_eq!(rendered, "SSE stream contained an invalid JSON frame");
    }

    #[test]
    fn completed_tool_call_is_owned_by_the_stateful_terminal_mapper() {
        let event = SseEvent {
            event_type: "response.completed".to_string(),
            data: serde_json::json!({
                "response": {
                    "status": "completed",
                    "output": [{"type": "function_call"}],
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }),
        };
        assert!(map_sse_event(&event).is_none());
    }

    #[test]
    fn custom_tool_call_input_delta_maps_to_tool_call_delta() {
        // Even when `call_id` is present in the delta payload, ToolCallDelta
        // must carry only the streaming `item_id` — `call_id` is reserved
        // for the eventual ToolCallComplete and the two are never merged.
        let event = SseEvent {
            event_type: "response.custom_tool_call_input.delta".to_string(),
            data: serde_json::json!({"item_id": "ctc_1", "call_id": "call_1", "delta": "*** Begin"}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::ToolCallDelta {
                item_id,
                call_id: _,
                name,
                arguments_delta,
                kind: _,
            })) => {
                assert_eq!(item_id, "ctc_1");
                assert!(name.is_none());
                assert_eq!(arguments_delta, "*** Begin");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    #[test]
    fn output_item_added_call_id_extracts_tool_correlation() {
        // C7: the correlation pair is read from a function_call / custom_tool_call
        // announcement and ignored for everything else.
        let func = SseEvent {
            event_type: "response.output_item.added".to_string(),
            data: serde_json::json!({
                "item": {"type": "function_call", "id": "fc_9", "call_id": "call_9"}
            }),
        };
        assert_eq!(
            output_item_added_call_id(&func),
            Some(("fc_9".to_string(), "call_9".to_string())),
        );
        let custom = SseEvent {
            event_type: "response.output_item.added".to_string(),
            data: serde_json::json!({
                "item": {"type": "custom_tool_call", "id": "ctc_9", "call_id": "call_c"}
            }),
        };
        assert_eq!(
            output_item_added_call_id(&custom),
            Some(("ctc_9".to_string(), "call_c".to_string())),
        );
        // Non-tool items and payloads missing an id yield no correlation.
        for data in [
            serde_json::json!({"item": {"type": "message", "id": "msg_1"}}),
            serde_json::json!({"item": {"type": "function_call", "id": "fc_1"}}),
            serde_json::json!({"item": {"type": "function_call", "call_id": "call_1"}}),
            serde_json::json!({"unexpected": true}),
        ] {
            let ev = SseEvent {
                event_type: "response.output_item.added".to_string(),
                data,
            };
            assert_eq!(output_item_added_call_id(&ev), None);
        }
    }

    #[test]
    fn tool_call_delta_skipped_when_only_call_id_present() {
        // R8 acceptance: when item_id/id are absent, the delta cannot be
        // merged. Falling back to call_id would corrupt downstream echo, so
        // the event must be dropped (not emitted with call_id as the merge
        // key).
        let event = SseEvent {
            event_type: "response.function_call_arguments.delta".to_string(),
            data: serde_json::json!({"call_id": "call_xyz", "delta": "{"}),
        };
        assert!(map_sse_event(&event).is_none());
    }

    #[test]
    fn known_lifecycle_events_are_explicitly_ignored() {
        for event_type in [
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.content_part.done",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_part.done",
            "response.function_call_arguments.done",
            "response.custom_tool_call_input.done",
            "response.in_progress",
            "response.queued",
            "response.metadata",
        ] {
            let event = SseEvent {
                event_type: event_type.to_string(),
                data: serde_json::json!({}),
            };
            assert!(
                map_sse_event(&event).is_none(),
                "{event_type} should be explicitly ignored (None), not mapped to an event",
            );
        }
    }

    #[test]
    fn output_text_done_maps_to_text_complete() {
        let event = SseEvent {
            event_type: "response.output_text.done".to_string(),
            data: serde_json::json!({"text": "The full response text."}),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::TextComplete { text })) => {
                assert_eq!(text, "The full response text.");
            }
            other => panic!("expected TextComplete, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_done_maps_to_thinking_complete() {
        for event_type in [
            "response.reasoning_summary_text.done",
            "response.reasoning_text.done",
        ] {
            let event = SseEvent {
                event_type: event_type.to_string(),
                data: serde_json::json!({"text": "My reasoning was..."}),
            };
            match map_sse_event(&event) {
                Some(Ok(ProviderEvent::ThinkingComplete { text })) => {
                    assert_eq!(text, "My reasoning was...");
                }
                other => panic!("expected ThinkingComplete for {event_type}, got {other:?}"),
            }
        }
    }
}
