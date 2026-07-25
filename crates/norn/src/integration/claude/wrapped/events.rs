use std::collections::HashMap;
use std::sync::Arc;

use claude_runner::ClaudeEvent;
use claude_runner::events::ContentItem;
use claude_runner::events::Usage as ClaudeUsage;

use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::session::events::{EventBase, SessionEvent, ToolCallEvent};

use super::super::adapter::tool_data_pair;

pub(super) fn claude_session_id_of(event: &ClaudeEvent) -> Option<&str> {
    match event {
        ClaudeEvent::System(data) => data.session_id.as_deref(),
        ClaudeEvent::Assistant { session_id, .. }
        | ClaudeEvent::User { session_id, .. }
        | ClaudeEvent::StreamEvent { session_id, .. }
        | ClaudeEvent::Result { session_id, .. } => session_id.as_deref(),
        _ => None,
    }
}

pub(super) fn capture_session_events(
    events: &[ClaudeEvent],
    session_id: &Arc<parking_lot::Mutex<Option<String>>>,
) -> Vec<SessionEvent> {
    let mut output = Vec::new();
    for event in events {
        if let Some(observed_session_id) = claude_session_id_of(event) {
            let mut stored_session_id = session_id.lock();
            *stored_session_id = Some(observed_session_id.to_owned());
        }

        match event {
            ClaudeEvent::Assistant { message, .. } => {
                let tool_calls = message
                    .content
                    .iter()
                    .filter_map(|item| {
                        if let ContentItem::ToolUse { id, tool_data } = item {
                            let (name, input) = tool_data_pair(tool_data);
                            Some(ToolCallEvent {
                                call_id: id.clone(),
                                name,
                                arguments: input,
                                kind: ToolCallKind::Function,
                                caller: ToolCallCaller::Absent,
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                output.push(SessionEvent::AssistantMessage {
                    response_items: Vec::new(),
                    base: EventBase::new(None),
                    content: message.text_items().collect::<Vec<&str>>().join("\n"),
                    thinking: String::new(),
                    reasoning: Vec::new(),
                    tool_calls,
                    usage: message.usage.as_ref().map(event_usage).unwrap_or_default(),
                    stop_reason: message.stop_reason.clone().unwrap_or_default(),
                    response_id: None,
                });
            }
            ClaudeEvent::User { message, .. } => {
                for item in &message.content {
                    match item {
                        ContentItem::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => output.push(SessionEvent::ToolResult {
                            base: EventBase::new(None),
                            tool_call_id: tool_use_id.clone(),
                            tool_name: String::new(),
                            output: content.clone(),
                            spool_ref: None,
                            duration_ms: 0,
                        }),
                        ContentItem::Text { text } => output.push(SessionEvent::UserMessage {
                            base: EventBase::new(None),
                            content: text.clone(),
                        }),
                        _ => {}
                    }
                }
            }
            ClaudeEvent::Result {
                subtype,
                is_error,
                duration_ms,
                duration_api_ms,
                result,
                error,
                num_turns,
                session_id,
                structured_output,
                stop_reason,
                total_cost_usd,
                usage,
                sdk_metadata,
            } => {
                let data = HashMap::from([
                    ("subtype".to_owned(), serde_json::json!(subtype)),
                    ("is_error".to_owned(), serde_json::json!(is_error)),
                    ("duration_ms".to_owned(), serde_json::json!(duration_ms)),
                    (
                        "duration_api_ms".to_owned(),
                        serde_json::json!(duration_api_ms),
                    ),
                    ("result".to_owned(), serde_json::json!(result)),
                    ("error".to_owned(), serde_json::json!(error)),
                    ("num_turns".to_owned(), serde_json::json!(num_turns)),
                    ("session_id".to_owned(), serde_json::json!(session_id)),
                    (
                        "structured_output".to_owned(),
                        serde_json::json!(structured_output),
                    ),
                    ("stop_reason".to_owned(), serde_json::json!(stop_reason)),
                    (
                        "total_cost_usd".to_owned(),
                        serde_json::json!(total_cost_usd),
                    ),
                    ("usage".to_owned(), serde_json::json!(usage)),
                    ("sdk_metadata".to_owned(), serde_json::json!(sdk_metadata)),
                ]);
                output.push(SessionEvent::Custom {
                    base: EventBase::new(None),
                    event_type: "claude_code.result".to_owned(),
                    data: serde_json::json!(data),
                });
            }
            _ => {}
        }
    }
    output
}

fn event_usage(usage: &ClaudeUsage) -> crate::session::events::EventUsage {
    crate::session::events::EventUsage {
        input_tokens: usage.input_tokens.unwrap_or(0),
        output_tokens: usage.output_tokens.unwrap_or(0),
        cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        cost_usd: None,
    }
}
