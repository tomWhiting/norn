//! SSE event parsing and mapping to `ProviderEvent`.
//!
//! Splits the streaming protocol surface across two files:
//!
//! * `sse.rs` (this file) — the raw byte-stream parser
//!   ([`parse_sse_bytes`], [`SseParser`]) and the SSE-to-`ProviderEvent`
//!   dispatcher ([`map_sse_event`]). All control flow lives here.
//! * [`super::sse_types`] — typed deserialization targets for
//!   `output_item.done` / `response.failed` / `response.incomplete`
//!   payloads, plus the [`classify_failed_error`] error-code classifier
//!   and the `Retry-After` regex parser.
//!
//! The split keeps this file under the project's 500-LOC-per-file
//! production budget without touching the wire protocol surface.

use serde::Deserialize;

use super::sse_types::{
    CustomToolCallItem, FunctionCallItem, ResponseFailedPayload, classify_failed_error,
    incomplete_stop_reason,
};
use crate::error::ProviderError;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ToolCallKind;
use crate::provider::usage::Usage;

/// An intermediate SSE event parsed from the byte stream.
#[derive(Clone, Debug)]
pub struct SseEvent {
    /// The SSE event type (e.g. `response.output_text.delta`).
    pub event_type: String,
    /// The parsed JSON data payload.
    pub data: serde_json::Value,
}

/// Parses a raw SSE byte stream into a sequence of `SseEvent` values.
///
/// SSE frames are delimited by double newlines. Each frame contains
/// `event:` and `data:` lines. Comment lines (starting with `:`) and
/// empty data lines are skipped.
pub fn parse_sse_bytes(raw: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event_type = String::new();
    let mut current_data = String::new();

    for line in raw.lines() {
        if line.starts_with(':') {
            continue;
        }

        if let Some(event_value) = line
            .strip_prefix("event: ")
            .or_else(|| line.strip_prefix("event:"))
        {
            current_event_type = event_value.trim().to_string();
        } else if let Some(data_value) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            current_data = data_value.to_string();
        } else if line.is_empty() {
            if !current_event_type.is_empty()
                && !current_data.is_empty()
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&current_data)
            {
                events.push(SseEvent {
                    event_type: current_event_type.clone(),
                    data,
                });
            }
            current_event_type.clear();
            current_data.clear();
        }
    }

    if !current_event_type.is_empty()
        && !current_data.is_empty()
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&current_data)
    {
        events.push(SseEvent {
            event_type: current_event_type,
            data,
        });
    }

    events
}

/// Stateful incremental SSE parser.
///
/// Accepts arbitrary byte chunks via [`SseParser::feed`] and yields complete
/// [`SseEvent`] values as frames become available. Frames split across chunk
/// boundaries — including individual `event:` or `data:` lines, the
/// double-newline frame delimiter, and multi-byte UTF-8 codepoints — are
/// reassembled correctly.
///
/// After the underlying byte stream completes, callers SHOULD invoke
/// [`SseParser::finish`] to flush any trailing frame that lacks a terminating
/// blank line, mirroring the tail-flush behaviour of [`parse_sse_bytes`].
///
/// The parser holds a byte buffer (`Vec<u8>`) rather than a `String` so that
/// multi-byte UTF-8 codepoints split across chunks are preserved. Newline
/// (`0x0A`) cannot appear inside a UTF-8 continuation byte, so byte-level
/// line scanning is safe.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    current_event_type: String,
    current_data: String,
}

impl SseParser {
    /// Creates a new parser with empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of bytes into the parser, returning any complete events.
    ///
    /// Empty chunks are a no-op (return an empty `Vec`). Otherwise the chunk
    /// is appended to the internal buffer and every complete `\n`-terminated
    /// line is consumed from the front. CRLF line endings are handled by
    /// stripping a trailing `\r` before processing.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_end = if pos > 0 && self.buffer[pos - 1] == b'\r' {
                pos - 1
            } else {
                pos
            };
            let line = String::from_utf8_lossy(&self.buffer[..line_end]).into_owned();
            self.buffer.drain(..=pos);
            self.process_line(&line, &mut events);
        }
        events
    }

    /// Flushes any trailing partial line and pending frame state.
    ///
    /// Callers MUST invoke this once the underlying byte stream has ended
    /// (otherwise a final frame that lacks a terminating blank line will be
    /// lost). Mirrors the tail-flush behaviour at the end of
    /// [`parse_sse_bytes`].
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let tail = std::mem::take(&mut self.buffer);
            let trimmed: &[u8] = if tail.last() == Some(&b'\r') {
                &tail[..tail.len() - 1]
            } else {
                &tail[..]
            };
            let line = String::from_utf8_lossy(trimmed).into_owned();
            self.process_line(&line, &mut events);
        }
        if !self.current_data.is_empty()
            && let Ok(data) = serde_json::from_str::<serde_json::Value>(&self.current_data)
        {
            events.push(SseEvent {
                event_type: std::mem::take(&mut self.current_event_type),
                data,
            });
        }
        self.current_event_type.clear();
        self.current_data.clear();
        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.starts_with(':') {
            return;
        }

        if let Some(event_value) = line
            .strip_prefix("event: ")
            .or_else(|| line.strip_prefix("event:"))
        {
            self.current_event_type = event_value.trim().to_string();
        } else if let Some(data_value) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            self.current_data = data_value.to_string();
        } else if line.is_empty() {
            if !self.current_data.is_empty()
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&self.current_data)
            {
                events.push(SseEvent {
                    event_type: self.current_event_type.clone(),
                    data,
                });
            }
            self.current_event_type.clear();
            self.current_data.clear();
        }
    }
}

/// Maps an `SseEvent` to an optional `ProviderEvent`.
///
/// Unrecognized event types are logged at debug level and produce `None`.
pub fn map_sse_event(event: &SseEvent) -> Option<Result<ProviderEvent, ProviderError>> {
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
                tracing::warn!(
                    event_type = event.event_type.as_str(),
                    "tool call delta missing item_id/id, skipping",
                );
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
            Some(Ok(ProviderEvent::ToolCallDelta {
                item_id,
                name: None,
                arguments_delta: delta,
                kind,
            }))
        }

        "response.output_item.done" => {
            let item = event.data.get("item").unwrap_or(&event.data);
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match item_type {
                "function_call" => {
                    // Typed deserialization isolates the wire shape and lets
                    // serde enforce that `call_id` is present. A
                    // `call_id`-less event cannot produce a usable
                    // `ToolCallComplete` — there is no legitimate downstream
                    // value for it — so the event is dropped with a warning
                    // rather than padded with an empty string.
                    match FunctionCallItem::deserialize(item) {
                        Ok(FunctionCallItem {
                            call_id,
                            name,
                            arguments,
                        }) => Some(Ok(ProviderEvent::ToolCallComplete {
                            call_id,
                            name,
                            arguments,
                            kind: ToolCallKind::Function,
                        })),
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "function_call output_item.done failed to deserialize, skipping",
                            );
                            None
                        }
                    }
                }
                "custom_tool_call" => {
                    // Custom tool calls carry their freeform body in an
                    // `input` field rather than `arguments` (per
                    // `reference/codex-rs/protocol-models.rs:815-826`). The
                    // assembled value is plumbed through the same
                    // `arguments` field as function calls; the `kind` marker
                    // tells the serializer to echo back with `input` + the
                    // `custom_tool_call_output` envelope.
                    match CustomToolCallItem::deserialize(item) {
                        Ok(CustomToolCallItem {
                            call_id,
                            name,
                            input,
                        }) => Some(Ok(ProviderEvent::ToolCallComplete {
                            call_id,
                            name,
                            arguments: input,
                            kind: ToolCallKind::Custom,
                        })),
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "custom_tool_call output_item.done failed to deserialize, skipping",
                            );
                            None
                        }
                    }
                }
                "compaction" | "compaction_summary" | "context_compaction" => {
                    Some(Ok(ProviderEvent::Compaction {
                        item_type: item_type.to_string(),
                        encrypted_content: item
                            .get("encrypted_content")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                    }))
                }
                _ => None,
            }
        }

        "response.completed" => {
            let usage = extract_usage(&event.data);
            let stop_reason = extract_stop_reason(&event.data);
            Some(Ok(ProviderEvent::Done {
                stop_reason,
                usage,
                response_id: extract_response_id(&event.data),
            }))
        }

        "response.failed" => {
            // The error nests under `response.error`. Deserialize and classify
            // by error code; if the payload is malformed or the nesting is
            // absent, fall back to a generic stream error (never an empty
            // string).
            let error_detail = ResponseFailedPayload::deserialize(&event.data)
                .ok()
                .and_then(|p| p.response)
                .and_then(|r| r.error);
            let err = match error_detail {
                Some(detail) => classify_failed_error(&detail),
                None => ProviderError::StreamError {
                    reason: "response.failed".to_string(),
                },
            };
            Some(Err(err))
        }

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
            let reason = ResponseFailedPayload::deserialize(&event.data)
                .ok()
                .and_then(|p| p.response)
                .and_then(|r| r.incomplete_details)
                .and_then(|d| d.reason);
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

        other => {
            tracing::debug!(event_type = other, "unrecognized SSE event type, skipping");
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

/// Derives the [`StopReason`] for a `response.completed` event.
///
/// Only reachable from `response.completed`, whose status is always
/// `"completed"` — truncation stops arrive on the dedicated
/// `response.incomplete` event and are mapped by
/// [`incomplete_stop_reason`] instead.
fn extract_stop_reason(data: &serde_json::Value) -> StopReason {
    let end_turn = data
        .get("response")
        .and_then(|r| r.get("output"))
        .and_then(|o| o.as_array())
        .and_then(|arr| arr.last())
        .and_then(|item| item.get("type"))
        .and_then(|v| v.as_str());

    match end_turn {
        Some("function_call") => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
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
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::time::Duration;

    use super::*;

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
    fn output_item_done_emits_tool_call_complete() {
        // R9-1: done payload with distinct `fc_*` id and `call_*` call_id —
        // ToolCallComplete must propagate call_id, NOT fall back to the item id.
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
        match map_sse_event(&done) {
            Some(Ok(ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                kind: _,
            })) => {
                assert_eq!(call_id, "call_xyz", "must propagate call_id");
                assert_ne!(call_id, "fc_abc", "must NOT fall back to item id");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, "{\"city\": \"NYC\"}");
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    #[test]
    fn output_item_done_custom_tool_call_emits_complete_with_custom_kind() {
        // F5: a `custom_tool_call` output_item.done event must yield a
        // `ToolCallComplete` carrying the freeform `input` in the `arguments`
        // slot, the `call_id` from the wire, and `kind = Custom` so the
        // serializer picks the `custom_tool_call_output` envelope downstream.
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
        match map_sse_event(&done) {
            Some(Ok(ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                kind,
            })) => {
                assert_eq!(call_id, "call_custom");
                assert_eq!(name, "apply_patch");
                assert_eq!(
                    arguments, "*** BEGIN PATCH ***\n@@\n-foo\n+bar\n*** END PATCH ***",
                    "freeform input must pass through verbatim",
                );
                assert_eq!(kind, ToolCallKind::Custom);
            }
            other => panic!("expected custom-kind ToolCallComplete, got {other:?}"),
        }
    }

    #[test]
    fn output_item_done_custom_tool_call_missing_call_id_skipped() {
        // A custom_tool_call done event lacking `call_id` must be dropped,
        // not padded with an empty string — same rule as function_call.
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
        assert!(map_sse_event(&done).is_none());
    }

    #[test]
    fn output_item_done_compaction_emits_opaque_event() {
        let done = SseEvent {
            event_type: "response.output_item.done".to_string(),
            data: serde_json::json!({
                "item": {
                    "type": "compaction",
                    "encrypted_content": "enc_state",
                }
            }),
        };
        match map_sse_event(&done) {
            Some(Ok(ProviderEvent::Compaction {
                item_type,
                encrypted_content,
            })) => {
                assert_eq!(item_type, "compaction");
                assert_eq!(encrypted_content.as_deref(), Some("enc_state"));
            }
            other => panic!("expected Compaction, got {other:?}"),
        }
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
    fn output_item_done_missing_call_id_skipped() {
        // R9-2: a `function_call` done item with no `call_id` cannot produce
        // a usable ToolCallComplete — the event must be skipped, not emitted
        // with an empty-string fallback.
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
        assert!(map_sse_event(&done).is_none());
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
    fn completed_event_mapping() {
        let event = SseEvent {
            event_type: "response.completed".to_string(),
            data: serde_json::json!({
                "response": {
                    "status": "completed",
                    "usage": {"input_tokens": 100, "output_tokens": 50}
                }
            }),
        };
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::Done {
                stop_reason, usage, ..
            })) => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            other => panic!("expected Done, got {other:?}"),
        }
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
        // no error code the classifier degrades to StreamError carrying the
        // message — proving the message is read from `response.error`, not the
        // top level.
        let event = failed_event(None, Some("rate limit exceeded"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "rate limit exceeded");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_reads_code_and_message_from_nested_response() {
        // #1: both code and message are extracted from the nested location.
        // `server_is_overloaded` now gets the retry-eligible HTTP 503 prefix
        // (see F1+F2 in the work-order pack), so the assertion verifies both
        // the prefix and the original message body survive.
        let event = failed_event(Some("server_is_overloaded"), Some("overloaded; retry"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "HTTP 503: overloaded; retry");
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
                assert_eq!(message, "Your prompt was invalid.");
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
                assert_eq!(message, "Flagged for security review.");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn failed_server_overloaded_encoded_as_http_503_for_retry() {
        // #7: server_is_overloaded is transient back-pressure. It must surface
        // with an `HTTP 503:` prefix so the loop-level retry classifier
        // recognises it as `RetryableError::ServerError`.
        let event = failed_event(Some("server_is_overloaded"), Some("busy"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "HTTP 503: busy");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_server_overloaded_empty_message_uses_fallback() {
        // Missing/blank message still produces the retry-eligible prefix.
        let event = failed_event(Some("server_is_overloaded"), None);
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "HTTP 503: server is overloaded");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_slow_down_encoded_as_http_503_for_retry() {
        // #7b: slow_down is also transient back-pressure. Same encoding so the
        // retry classifier picks it up identically.
        let event = failed_event(Some("slow_down"), Some("rate"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "HTTP 503: rate");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_slow_down_empty_message_uses_fallback() {
        let event = failed_event(Some("slow_down"), None);
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "HTTP 503: slow down");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn failed_unknown_code_preserves_message() {
        // #8: unknown codes do NOT get the HTTP 503 prefix — opting an unknown
        // error into automatic retry would silently amplify novel failure modes.
        let event = failed_event(Some("some_future_error"), Some("weird failure"));
        match map_sse_event(&event) {
            Some(Err(ProviderError::StreamError { reason })) => {
                assert_eq!(reason, "weird failure");
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
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
        // carrying the verbatim reason.
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
                assert!(
                    err.to_string().contains("some_future_reason"),
                    "error must carry the verbatim reason: {err}"
                );
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
    fn failed_malformed_payload_falls_back_without_panic() {
        // #10: a payload that does not match the expected shape degrades to a
        // generic stream error rather than panicking or yielding an empty
        // string. Exercise both a wrong-typed payload and a missing nesting.
        for data in [
            serde_json::json!("just a string"),
            serde_json::json!({"unexpected": true}),
            serde_json::json!({"response": {"error": null}}),
        ] {
            let event = SseEvent {
                event_type: "response.failed".to_string(),
                data,
            };
            match map_sse_event(&event) {
                Some(Err(ProviderError::StreamError { reason })) => {
                    assert_eq!(reason, "response.failed");
                }
                other => panic!("expected generic StreamError fallback, got {other:?}"),
            }
        }
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
    fn parser_finish_flushes_trailing_frame() {
        // Frame without a terminating blank line.
        let mut parser = SseParser::new();
        let early = parser.feed(b"event: a\ndata: {\"v\":1}\n");
        assert!(early.is_empty());
        let tail = parser.finish();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].event_type, "a");
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

    #[test]
    fn tool_use_stop_reason() {
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
        match map_sse_event(&event) {
            Some(Ok(ProviderEvent::Done { stop_reason, .. })) => {
                assert_eq!(stop_reason, StopReason::ToolUse);
            }
            other => panic!("expected ToolUse stop reason, got {other:?}"),
        }
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
