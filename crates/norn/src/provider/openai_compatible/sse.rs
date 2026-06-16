//! Chat Completions SSE chunk mapping.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::ProviderError;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ToolCallKind;
use crate::provider::usage::Usage;

/// Stateful mapper for Chat Completions streaming chunks.
#[derive(Debug, Default)]
pub(super) struct ChatCompletionsMapper {
    tool_calls: BTreeMap<ToolKey, ToolCallState>,
    latest_usage: Usage,
}

impl ChatCompletionsMapper {
    /// Maps a parsed SSE event into zero or more provider events.
    pub(super) fn map_event(
        &mut self,
        event: &crate::provider::openai::sse::SseEvent,
    ) -> Vec<Result<ProviderEvent, ProviderError>> {
        let Ok(chunk) = ChatChunk::deserialize(&event.data) else {
            return vec![Err(ProviderError::ResponseParseError {
                reason: format!(
                    "failed to deserialize chat completion chunk: {}",
                    event.data
                ),
            })];
        };
        if let Some(usage) = chunk.usage {
            self.latest_usage = usage.into();
        }
        let mut out = Vec::new();
        for choice in chunk.choices {
            self.map_choice(choice, &mut out);
        }
        out
    }

    fn map_choice(
        &mut self,
        choice: ChatChoice,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            out.push(Ok(ProviderEvent::TextDelta { text: content }));
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
            if let Some(name) = function.name {
                state.name = Some(name.clone());
                out.push(Ok(ProviderEvent::ToolCallDelta {
                    item_id: key.item_id(),
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
        if let Some(name) = function.name {
            state.name = Some(name.clone());
            out.push(Ok(ProviderEvent::ToolCallDelta {
                item_id: key.item_id(),
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
                    name: None,
                    arguments_delta: arguments,
                    kind: ToolCallKind::Function,
                }));
            }
        }
    }

    fn map_finish_reason(
        &mut self,
        reason: &str,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) {
        match reason {
            "tool_calls" | "function_call" => {
                if self.complete_tool_calls(out) {
                    out.push(Ok(ProviderEvent::Done {
                        stop_reason: StopReason::ToolUse,
                        usage: self.latest_usage.clone(),
                        response_id: None,
                    }));
                }
            }
            "stop" => out.push(Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: self.latest_usage.clone(),
                response_id: None,
            })),
            "length" => out.push(Ok(ProviderEvent::Done {
                stop_reason: StopReason::MaxTokens,
                usage: self.latest_usage.clone(),
                response_id: None,
            })),
            "content_filter" => out.push(Ok(ProviderEvent::Done {
                stop_reason: StopReason::ContentFilter,
                usage: self.latest_usage.clone(),
                response_id: None,
            })),
            other => out.push(Err(ProviderError::ResponseParseError {
                reason: format!("unknown chat completion finish_reason '{other}'"),
            })),
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
                    reason: format!("chat tool call {call_id} missing function name"),
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
    use crate::provider::openai::sse::SseEvent;

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
    fn unknown_finish_reason_is_parse_error() {
        let mut mapper = ChatCompletionsMapper::default();
        let events = mapper.map_event(&event(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "mystery"}]
        })));
        assert!(matches!(
            &events[0],
            Err(ProviderError::ResponseParseError { reason }) if reason.contains("mystery"),
        ));
    }
}
