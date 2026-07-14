//! Chat Completions SSE chunk mapping.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::ProviderError;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::exec::SseEventMapper;
use crate::provider::openai::sse::SseEvent;
use crate::provider::request::ToolCallKind;
use crate::provider::usage::Usage;

/// Stateful mapper for Chat Completions streaming chunks.
///
/// The terminal [`ProviderEvent::Done`] is deliberately *not* emitted the
/// moment a `finish_reason` arrives. Under `stream_options.include_usage`
/// (always requested — see
/// [`build_payload`](super::request::build_payload)) an OpenAI-conformant
/// backend streams the token usage in a **separate final chunk** (empty
/// `choices`, populated `usage`) *after* the chunk that carried
/// `finish_reason`, immediately before the `[DONE]` sentinel. Emitting `Done`
/// on `finish_reason` would return the executor before that usage chunk is
/// read, silently reporting zero-token usage for the whole turn. Instead the
/// stop reason is recorded in `pending_stop` and the terminal `Done` is
/// emitted either the moment the usage chunk is observed
/// ([`Self::maybe_emit_terminal`]) or, for backends that never send one, at
/// clean stream close ([`Self::finish_on_clean_close`]) — usage absent there
/// is then legitimate.
#[derive(Debug, Default)]
pub(super) struct ChatCompletionsMapper {
    tool_calls: BTreeMap<ToolKey, ToolCallState>,
    latest_usage: Usage,
    emitted_output: bool,
    /// Stop reason recorded from a `finish_reason` chunk whose terminal
    /// `Done` is deferred until the trailing usage chunk (or stream close).
    pending_stop: Option<StopReason>,
    /// Whether a chunk carrying a populated `usage` object has been seen.
    /// Distinguishes "no usage reported yet" from a legitimate all-zero
    /// usage, and gates the deferred terminal emission.
    usage_seen: bool,
}

impl SseEventMapper for ChatCompletionsMapper {
    /// Maps a parsed SSE event into zero or more provider events.
    fn map_event(&mut self, event: &SseEvent) -> Vec<Result<ProviderEvent, ProviderError>> {
        if let Some(message) = stream_error_message(&event.data) {
            return vec![Err(stream_error_from_message(message))];
        }
        let Ok(chunk) = ChatChunk::deserialize(&event.data) else {
            return vec![Err(ProviderError::ResponseParseError {
                reason: "failed to deserialize chat completion chunk".to_owned(),
            })];
        };
        if let Some(usage) = chunk.usage {
            self.latest_usage = usage.into();
            self.usage_seen = true;
        }
        let mut out = Vec::new();
        for choice in chunk.choices {
            self.map_choice(choice, &mut out);
        }
        // Emit the deferred terminal event once both the stop reason and the
        // usage-bearing chunk have been observed. This covers the
        // spec-conformant split ordering (usage in a trailing empty-choices
        // chunk) and the bundled ordering (usage on the same chunk as
        // `finish_reason`) with one rule, without waiting for stream close.
        self.maybe_emit_terminal(&mut out);
        out
    }

    /// Builds a terminal event for local-compatible backends that close a
    /// text stream cleanly without emitting a final `finish_reason`.
    fn finish_on_clean_close(&mut self) -> Result<Option<ProviderEvent>, ProviderError> {
        if !self.tool_calls.is_empty() {
            return Err(ProviderError::ResponseParseError {
                reason:
                    "chat completions stream ended with incomplete tool calls before finish_reason"
                        .to_string(),
            });
        }
        // A `finish_reason` was recorded but no usage chunk ever arrived (a
        // backend that does not honor `stream_options.include_usage`, or that
        // closes the stream straight after `finish_reason`). The terminal
        // event is still owed: emit it with whatever usage was observed —
        // absent usage is legitimate at a clean close.
        if let Some(stop_reason) = self.pending_stop.take() {
            return Ok(Some(ProviderEvent::Done {
                stop_reason,
                usage: self.latest_usage.clone(),
                response_id: None,
            }));
        }
        if !self.emitted_output {
            return Ok(None);
        }
        Ok(Some(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: self.latest_usage.clone(),
            response_id: None,
        }))
    }

    fn dump_label<'event>(&self, _event: &'event SseEvent) -> &'event str {
        "chat.completion.chunk"
    }
}

impl ChatCompletionsMapper {
    fn map_choice(
        &mut self,
        choice: ChatChoice,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            self.emitted_output = true;
            out.push(Ok(ProviderEvent::TextDelta { text: content }));
        }
        if let Some(reasoning) = choice.delta.reasoning_content
            && !reasoning.is_empty()
        {
            self.emitted_output = true;
            out.push(Ok(ProviderEvent::ThinkingDelta { text: reasoning }));
        }
        for call in choice.delta.tool_calls {
            self.map_tool_delta(choice.index, call, out);
        }
        if let Some(function_call) = choice.delta.function_call {
            self.map_legacy_function_delta(choice.index, function_call, out);
        }
        if let Some(reason) = choice.finish_reason {
            self.map_finish_reason(&reason, out);
        }
    }

    fn map_tool_delta(
        &mut self,
        choice_index: usize,
        call: ToolCallDelta,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        let key = ToolKey {
            choice_index,
            tool_index: call.index.unwrap_or(0),
        };
        let state = self.tool_calls.entry(key).or_default();
        if let Some(id) = call.id {
            state.call_id = Some(id);
        }
        if let Some(function) = call.function {
            // The Chat Completions stream carries the tool-call `id` on the
            // first chunk of each call; `state.call_id` is therefore `Some`
            // from that chunk onward and correlates every fragment of this
            // call. It is `None` only until the id chunk is seen — honest,
            // never fabricated.
            if let Some(name) = function.name {
                state.name = Some(name.clone());
                out.push(Ok(ProviderEvent::ToolCallDelta {
                    item_id: key.item_id(),
                    call_id: state.call_id.clone(),
                    name: Some(name),
                    arguments_delta: String::new(),
                    kind: ToolCallKind::Function,
                }));
            }
            if let Some(arguments) = function.arguments {
                state.arguments.push_str(&arguments);
                if !arguments.is_empty() {
                    out.push(Ok(ProviderEvent::ToolCallDelta {
                        item_id: key.item_id(),
                        call_id: state.call_id.clone(),
                        name: None,
                        arguments_delta: arguments,
                        kind: ToolCallKind::Function,
                    }));
                }
            }
        }
    }

    fn map_legacy_function_delta(
        &mut self,
        choice_index: usize,
        function: FunctionDelta,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        let key = ToolKey {
            choice_index,
            tool_index: 0,
        };
        let state = self.tool_calls.entry(key).or_default();
        // The deprecated top-level `function_call` streaming shape carries no
        // per-call id, so there is no `call_id` to correlate — emit a literal
        // `None`. (Reading `state.call_id` here would be wrong: this legacy
        // slot shares `ToolKey { tool_index: 0 }` with the modern
        // `tool_calls[0]` path, so a stream mixing both shapes could otherwise
        // stamp that call's id onto a legacy delta — a fabricated correlation.)
        if let Some(name) = function.name {
            state.name = Some(name.clone());
            out.push(Ok(ProviderEvent::ToolCallDelta {
                item_id: key.item_id(),
                call_id: None,
                name: Some(name),
                arguments_delta: String::new(),
                kind: ToolCallKind::Function,
            }));
        }
        if let Some(arguments) = function.arguments {
            state.arguments.push_str(&arguments);
            if !arguments.is_empty() {
                out.push(Ok(ProviderEvent::ToolCallDelta {
                    item_id: key.item_id(),
                    call_id: None,
                    name: None,
                    arguments_delta: arguments,
                    kind: ToolCallKind::Function,
                }));
            }
        }
    }

    /// Records the terminal stop reason from a `finish_reason` chunk.
    ///
    /// The [`ProviderEvent::Done`] is deferred (recorded in `pending_stop`)
    /// rather than emitted here, so the trailing `include_usage` chunk that a
    /// conformant backend streams *after* `finish_reason` is still consumed
    /// and its usage attributed — see the [`ChatCompletionsMapper`] type doc.
    /// For `tool_calls`/`function_call` the accumulated `ToolCallComplete`
    /// events are still emitted immediately; only their terminal `Done` is
    /// deferred, and only when every tool call was well-formed.
    fn map_finish_reason(
        &mut self,
        reason: &str,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        match reason {
            "tool_calls" | "function_call" => {
                if self.complete_tool_calls(out) {
                    self.pending_stop = Some(StopReason::ToolUse);
                }
            }
            "stop" => self.pending_stop = Some(StopReason::EndTurn),
            "length" => self.pending_stop = Some(StopReason::MaxTokens),
            "content_filter" => self.pending_stop = Some(StopReason::ContentFilter),
            _ => out.push(Err(ProviderError::ResponseParseError {
                reason: "provider returned an unknown chat completion finish_reason".to_owned(),
            })),
        }
    }

    /// Emits the deferred terminal [`ProviderEvent::Done`] once both a stop
    /// reason has been recorded and a usage-bearing chunk has been observed.
    ///
    /// Called at the end of every mapped chunk. When the two coincide (usage
    /// bundled on the `finish_reason` chunk) the terminal event is emitted on
    /// that chunk; when they are split (usage in the trailing empty-choices
    /// chunk) it is emitted on the usage chunk. If no usage chunk ever
    /// arrives, `pending_stop` remains set and
    /// [`Self::finish_on_clean_close`] emits the terminal event at stream
    /// close instead.
    fn maybe_emit_terminal(&mut self, out: &mut Vec<Result<ProviderEvent, ProviderError>>) {
        if self.usage_seen
            && let Some(stop_reason) = self.pending_stop.take()
        {
            out.push(Ok(ProviderEvent::Done {
                stop_reason,
                usage: self.latest_usage.clone(),
                response_id: None,
            }));
        }
    }

    fn complete_tool_calls(&mut self, out: &mut Vec<Result<ProviderEvent, ProviderError>>) -> bool {
        let calls = std::mem::take(&mut self.tool_calls);
        let mut all_complete = true;
        for (key, state) in calls {
            let Some(call_id) = state.call_id else {
                out.push(Err(ProviderError::ResponseParseError {
                    reason: format!("chat tool call {} missing id/call_id", key.item_id()),
                }));
                all_complete = false;
                continue;
            };
            let Some(name) = state.name else {
                out.push(Err(ProviderError::ResponseParseError {
                    reason: format!("chat tool call {} missing function name", key.item_id()),
                }));
                all_complete = false;
                continue;
            };
            out.push(Ok(ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments: state.arguments,
                kind: ToolCallKind::Function,
            }));
        }
        all_complete
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ToolKey {
    choice_index: usize,
    tool_index: usize,
}

impl ToolKey {
    fn item_id(self) -> String {
        format!("chatcmpl:{}:{}", self.choice_index, self.tool_index)
    }
}

#[derive(Debug, Default)]
struct ToolCallState {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    index: usize,
    #[serde(default)]
    delta: ChatDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    content: Option<String>,
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
    function_call: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

impl From<ChatUsage> for Usage {
    fn from(value: ChatUsage) -> Self {
        Self {
            input_tokens: value.prompt_tokens,
            output_tokens: value.completion_tokens,
            ..Self::default()
        }
    }
}

fn stream_error_message(data: &serde_json::Value) -> Option<&str> {
    let error = data.get("error")?;
    if let Some(message) = error.get("message").and_then(serde_json::Value::as_str) {
        return Some(message);
    }
    if let Some(message) = error.as_str() {
        return Some(message);
    }
    data.get("message").and_then(serde_json::Value::as_str)
}

fn stream_error_from_message(message: &str) -> ProviderError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("number of tokens")
    {
        return ProviderError::InvalidRequest {
            message: "provider rejected the request because it exceeded a context limit".to_owned(),
        };
    }
    // In-band chat error objects carry no transport semantics the client
    // can verify, so they never opt into retry (`transient: None`).
    ProviderError::StreamError {
        reason: "provider reported an in-band stream error".to_owned(),
        transient: None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use super::*;

    fn event(data: serde_json::Value) -> SseEvent {
        SseEvent {
            event_type: String::new(),
            data,
        }
    }

    #[test]
    fn maps_text_delta_and_done_usage() {
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "hello"}, "finish_reason": null}
            ]
        })));
        assert!(matches!(
            &events[0],
            Ok(ProviderEvent::TextDelta { text }) if text == "hello",
        ));

        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {}, "finish_reason": "stop"}
            ],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3}
        })));
        assert!(matches!(
            &events[0],
            Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage,
                response_id: None,
            }) if usage.input_tokens == 10 && usage.output_tokens == 3,
        ));
    }

    #[test]
    fn usage_chunk_after_finish_reason_is_attributed() {
        // Spec-conformant split ordering under stream_options.include_usage:
        // content chunk, then a finish_reason chunk with NO usage, then a
        // trailing empty-choices chunk carrying the usage, then [DONE]. The
        // terminal Done must be deferred to the usage chunk and report the
        // real token counts — never zero.
        let mut mapper = ChatCompletionsMapper::default();

        let content = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "hello"}, "finish_reason": null}
            ]
        })));
        assert!(matches!(
            &content[0],
            Ok(ProviderEvent::TextDelta { text }) if text == "hello",
        ));

        // finish_reason chunk with no usage — must NOT yet emit Done.
        let finish = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {}, "finish_reason": "stop"}
            ]
        })));
        assert!(
            !finish
                .iter()
                .any(|ev| matches!(ev, Ok(ProviderEvent::Done { .. }))),
            "Done must be deferred until the usage chunk: {finish:?}",
        );

        // Trailing usage-only chunk (empty choices) — Done emitted here with
        // the real usage.
        let usage = mapper.map_event(&event(serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 128, "completion_tokens": 64}
        })));
        assert!(matches!(
            usage.as_slice(),
            [Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage,
                response_id: None,
            })] if usage.input_tokens == 128 && usage.output_tokens == 64,
        ));
    }

    #[test]
    fn finish_reason_without_usage_chunk_defers_to_clean_close() {
        // A backend that sends finish_reason and then closes the stream
        // without any usage chunk must still terminate: no Done on the
        // finish_reason chunk, and finish_on_clean_close synthesizes the
        // terminal event. Usage absent there is legitimate (all zeros).
        let mut mapper = ChatCompletionsMapper::default();
        let _ = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "hi"}, "finish_reason": null}
            ]
        })));
        let finish = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {}, "finish_reason": "stop"}
            ]
        })));
        assert!(
            !finish
                .iter()
                .any(|ev| matches!(ev, Ok(ProviderEvent::Done { .. }))),
            "no Done until the stream ends when no usage chunk arrives: {finish:?}",
        );

        let done = mapper
            .finish_on_clean_close()
            .expect("clean close must synthesize the deferred terminal event");
        assert!(matches!(
            done,
            Some(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage,
                response_id: None,
            }) if usage.input_tokens == 0 && usage.output_tokens == 0,
        ));
    }

    #[test]
    fn tool_call_usage_chunk_after_finish_is_attributed() {
        // The same split ordering for a tool-call turn: the ToolCallComplete
        // events flow on the finish_reason chunk, but the terminal Done is
        // deferred to the trailing usage chunk.
        let mut mapper = ChatCompletionsMapper::default();
        let _ = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": null
            }]
        })));

        let finish = mapper.map_event(&event(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        })));
        assert!(
            finish.iter().any(|ev| matches!(
                ev,
                Ok(ProviderEvent::ToolCallComplete { call_id, .. }) if call_id == "call_abc",
            )),
            "tool call must complete on the finish_reason chunk: {finish:?}",
        );
        assert!(
            !finish
                .iter()
                .any(|ev| matches!(ev, Ok(ProviderEvent::Done { .. }))),
            "Done must be deferred until the usage chunk: {finish:?}",
        );

        let usage = mapper.map_event(&event(serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 9, "completion_tokens": 11}
        })));
        assert!(matches!(
            usage.as_slice(),
            [Ok(ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage,
                response_id: None,
            })] if usage.input_tokens == 9 && usage.output_tokens == 11,
        ));
    }

    #[test]
    fn maps_reasoning_content_delta_to_thinking() {
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {
                    "index": 0,
                    "delta": {"reasoning_content": "thinking"},
                    "finish_reason": null
                }
            ]
        })));

        assert!(matches!(
            &events[0],
            Ok(ProviderEvent::ThinkingDelta { text }) if text == "thinking",
        ));
    }

    #[test]
    fn maps_stream_error_object_to_terminal_error() -> Result<(), Box<dyn std::error::Error>> {
        const SECRET: &str = "stream-error-secret-must-not-escape";
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "error": {
                "message": format!("The number of tokens exceeds the context length: {SECRET}\nforged-log-line")
            },
            "message": SECRET
        })));

        let Err(ProviderError::InvalidRequest { message }) = &events[0] else {
            return Err(std::io::Error::other(format!(
                "expected InvalidRequest, got {:?}",
                events[0]
            ))
            .into());
        };
        assert_eq!(
            message,
            "provider rejected the request because it exceeded a context limit"
        );
        assert!(!message.contains(SECRET), "rendered error: {message}");
        assert!(
            !message.contains("forged-log-line"),
            "rendered error: {message}"
        );
        Ok(())
    }

    #[test]
    fn maps_unknown_stream_error_without_disclosing_provider_text()
    -> Result<(), Box<dyn std::error::Error>> {
        const SECRET: &str = "stream-error-secret-must-not-escape";
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "error": {"message": format!("{SECRET}\nforged-log-line")}
        })));

        let Err(ProviderError::StreamError { reason, transient }) = &events[0] else {
            return Err(std::io::Error::other(format!(
                "expected StreamError, got {:?}",
                events[0]
            ))
            .into());
        };
        assert_eq!(reason, "provider reported an in-band stream error");
        assert_eq!(*transient, None);
        assert!(!reason.contains(SECRET), "rendered error: {reason}");
        assert!(
            !reason.contains("forged-log-line"),
            "rendered error: {reason}"
        );
        Ok(())
    }

    #[test]
    fn malformed_chunk_does_not_disclose_provider_payload() -> Result<(), Box<dyn std::error::Error>>
    {
        const SECRET: &str = "malformed-chunk-secret-must-not-escape";
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": {"unexpected": format!("{SECRET}\n\u{1b}[31mforged-log-line")}
        })));

        let Err(ProviderError::ResponseParseError { reason }) = &events[0] else {
            return Err(std::io::Error::other(format!(
                "expected ResponseParseError, got {:?}",
                events[0]
            ))
            .into());
        };
        assert_eq!(reason, "failed to deserialize chat completion chunk");
        assert!(!reason.contains(SECRET), "rendered error: {reason}");
        assert!(
            !reason.contains("forged-log-line"),
            "rendered error: {reason}"
        );
        Ok(())
    }

    #[test]
    fn assembles_streamed_tool_call_before_tool_use_done() {
        let mut mapper = ChatCompletionsMapper::default();
        let first = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\""}
                    }]
                },
                "finish_reason": null
            }]
        })));
        assert!(first.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::ToolCallDelta {
                item_id,
                name: Some(name),
                ..
            }) if item_id == "chatcmpl:0:0" && name == "read_file",
        )));

        let second = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": ":\"README.md\"}"}
                    }]
                },
                "finish_reason": null
            }]
        })));
        assert!(second.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::ToolCallDelta {
                item_id,
                arguments_delta,
                ..
            }) if item_id == "chatcmpl:0:0" && arguments_delta == ":\"README.md\"}",
        )));

        let done = mapper.map_event(&event(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 5}
        })));
        assert!(done.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                kind: ToolCallKind::Function,
            }) if call_id == "call_abc"
                && name == "read_file"
                && arguments == "{\"path\":\"README.md\"}",
        )));
        assert!(done.iter().any(|event| matches!(
            event,
            Ok(ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }),
        )));
    }

    #[test]
    fn missing_tool_call_id_is_error() {
        let mut mapper = ChatCompletionsMapper::default();
        let _ = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": null
            }]
        })));
        let done = mapper.map_event(&event(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        })));
        assert!(done.iter().any(|event| {
            matches!(
                event,
                Err(ProviderError::ResponseParseError { reason })
                    if reason.contains("missing id")
            )
        }));
        assert!(
            !done
                .iter()
                .any(|event| matches!(event, Ok(ProviderEvent::Done { .. })))
        );
    }

    #[test]
    fn missing_tool_name_does_not_disclose_provider_call_id()
    -> Result<(), Box<dyn std::error::Error>> {
        const SECRET: &str = "call-id-secret-must-not-escape";
        let mut mapper = ChatCompletionsMapper::default();
        let _ = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": format!("{SECRET}\nforged-log-line"),
                        "type": "function",
                        "function": {"arguments": "{}"}
                    }]
                },
                "finish_reason": null
            }]
        })));
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        })));

        let Some(Err(ProviderError::ResponseParseError { reason })) = events.first() else {
            return Err(std::io::Error::other(format!(
                "expected ResponseParseError, got {events:?}"
            ))
            .into());
        };
        assert_eq!(reason, "chat tool call chatcmpl:0:0 missing function name");
        assert!(!reason.contains(SECRET), "rendered error: {reason}");
        assert!(
            !reason.contains("forged-log-line"),
            "rendered error: {reason}"
        );
        Ok(())
    }

    #[test]
    fn clean_close_after_text_synthesizes_end_turn() {
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "hello"}, "finish_reason": null}
            ]
        })));
        assert!(matches!(
            &events[0],
            Ok(ProviderEvent::TextDelta { text }) if text == "hello",
        ));

        let done = mapper
            .finish_on_clean_close()
            .expect("clean close should be accepted");
        assert!(matches!(
            done,
            Some(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            }),
        ));
    }

    #[test]
    fn clean_close_with_incomplete_tool_call_is_error() {
        let mut mapper = ChatCompletionsMapper::default();
        let _ = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\""}
                    }]
                },
                "finish_reason": null
            }]
        })));

        assert!(matches!(
            mapper.finish_on_clean_close(),
            Err(ProviderError::ResponseParseError { reason })
                if reason.contains("incomplete tool calls"),
        ));
    }

    #[test]
    fn unknown_finish_reason_is_parse_error_without_disclosure()
    -> Result<(), Box<dyn std::error::Error>> {
        const SECRET: &str = "finish-reason-secret-must-not-escape";
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": format!("{SECRET}\nforged-log-line")
            }]
        })));
        let Err(ProviderError::ResponseParseError { reason }) = &events[0] else {
            return Err(std::io::Error::other(format!(
                "expected ResponseParseError, got {:?}",
                events[0]
            ))
            .into());
        };
        assert_eq!(
            reason,
            "provider returned an unknown chat completion finish_reason"
        );
        assert!(!reason.contains(SECRET), "rendered error: {reason}");
        assert!(
            !reason.contains("forged-log-line"),
            "rendered error: {reason}"
        );
        Ok(())
    }
}
