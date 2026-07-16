//! Conversion from session events to provider messages.
//!
//! Bridges the canonical [`SessionEvent`] model (append-only, audit-grade)
//! to the normalised [`Message`] format consumed by providers. Only
//! conversation-relevant events produce messages; metadata events
//! (model changes, forks, labels, custom) are skipped.

use std::collections::HashMap;

use crate::provider::request::{AssistantToolCall, Message, MessageRole, ToolCallKind};
use crate::session::events::SessionEvent;

/// Convert a slice of session events into provider-ready messages.
///
/// Maps conversation-relevant variants (`UserMessage`, `AssistantMessage`,
/// `ToolResult`) to [`Message`] structs. Metadata variants (`ModelChange`,
/// `Fork`, `Label`, `Custom`, `SpokenResponse`, `Compaction`) are skipped.
///
/// The kind of each tool call is plumbed from its originating
/// `AssistantMessage` to the matching `ToolResult` so the serializer can pick
/// the correct wire envelope (`function_call_output` versus
/// `custom_tool_call_output`). When no matching call is present in the event
/// stream the kind falls back to [`ToolCallKind::Function`], matching the
/// behaviour of legacy events persisted before the field existed.
#[must_use]
pub fn events_to_messages(events: &[SessionEvent]) -> Vec<Message> {
    events_to_messages_inner(events, false)
}

/// Convert prompt-view events into provider-ready messages.
///
/// Unlike [`events_to_messages`], compaction events are converted into
/// developer messages. This function must only be used with a prompt view that
/// has already removed the events replaced by each compaction; otherwise the
/// model would receive both the full transcript and the summary.
#[must_use]
pub fn prompt_events_to_messages(events: &[SessionEvent]) -> Vec<Message> {
    events_to_messages_inner(events, true)
}

fn events_to_messages_inner(events: &[SessionEvent], include_compactions: bool) -> Vec<Message> {
    // First pass: index every assistant-emitted tool call by its `call_id`
    // so the second pass can stamp the matching kind onto each `ToolResult`.
    // The map is small (one entry per tool call in the session) and only
    // built once per conversion.
    let kinds = collect_tool_call_kinds(events);
    events
        .iter()
        .filter_map(|event| event_to_message(event, &kinds, include_compactions))
        .collect()
}

/// Build a `call_id -> ToolCallKind` map over every `AssistantMessage` event's
/// `tool_calls`. A later assistant message that re-uses the same `call_id`
/// overwrites the earlier one, which is the desired behaviour: the most
/// recent assistant turn defines the kind.
fn collect_tool_call_kinds(events: &[SessionEvent]) -> HashMap<String, ToolCallKind> {
    let mut kinds = HashMap::new();
    for event in events {
        if let Some(tool_calls) = event.assistant_tool_calls() {
            for tc in tool_calls {
                kinds.insert(tc.call_id, tc.kind);
            }
        }
    }
    kinds
}

fn event_to_message(
    event: &SessionEvent,
    kinds: &HashMap<String, ToolCallKind>,
    include_compactions: bool,
) -> Option<Message> {
    match event {
        SessionEvent::UserMessage { content, .. } => Some(Message {
            response_items: Vec::new(),
            role: MessageRole::User,
            content: Some(content.clone()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        }),

        SessionEvent::AssistantMessage {
            response_items,
            thinking,
            reasoning,
            ..
        } => {
            let tool_calls = event.assistant_tool_calls()?;
            let content = event.assistant_text()?;
            Some(Message {
                response_items: response_items.clone(),
                role: MessageRole::Assistant,
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                thinking: thinking.clone(),
                // Rebuild the assistant turn's captured reasoning items so a
                // resumed session keeps the model's reasoning state. No filter
                // on `encrypted_content` here: capture-everything at persist,
                // filter-at-replay is the existing division of labour — the
                // Responses serializer echoes only the items carrying the blob.
                reasoning: reasoning.clone(),
                tool_calls: tool_calls
                    .into_iter()
                    .map(|tc| AssistantToolCall {
                        call_id: tc.call_id,
                        name: tc.name,
                        arguments: match tc.kind {
                            // `Value`'s `Display` renders compact JSON and is
                            // total — unlike a fallible serializer round-trip,
                            // it can never silently collapse arguments to "".
                            ToolCallKind::Custom => tc
                                .arguments
                                .as_str()
                                .map_or_else(|| tc.arguments.to_string(), String::from),
                            ToolCallKind::Function => tc.arguments.to_string(),
                        },
                        kind: tc.kind,
                    })
                    .collect(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            })
        }

        SessionEvent::ToolResult {
            tool_call_id,
            tool_name,
            output,
            ..
        } => Some(Message {
            response_items: Vec::new(),
            role: MessageRole::ToolResult,
            content: Some(value_to_content_string(output)),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.clone()),
            tool_name: Some(tool_name.clone()),
            // The kind is looked up from the originating AssistantMessage's
            // ToolCallEvent. A miss (no matching call_id in the slice) is the
            // legacy path — fall back to None so `serialize_tool_result`
            // defaults to `function_call_output`.
            tool_call_kind: kinds.get(tool_call_id).copied(),
        }),

        SessionEvent::Compaction { summary, .. } if include_compactions => Some(Message {
            response_items: Vec::new(),
            role: MessageRole::Developer,
            content: Some(format!("Prior conversation compaction summary:\n{summary}")),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        }),

        // A fired rule delivered as conversation content (ContextInjection
        // or MessageDelivery) re-renders to the same prefixed User message
        // it produced live, so a resumed session sees identical text.
        // SystemContextAppend rules deliver through the system prompt, not
        // the message stream, so they render to nothing here.
        SessionEvent::RuleInjection {
            rule_id,
            delivery,
            content,
            ..
        } => delivery
            .format_conversation_content(rule_id, content)
            .map(|formatted| Message {
                response_items: Vec::new(),
                role: MessageRole::User,
                content: Some(formatted),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }),

        SessionEvent::ModelChange { .. }
        | SessionEvent::ChildBranch { .. }
        | SessionEvent::ForkComplete { .. }
        | SessionEvent::Label { .. }
        | SessionEvent::Custom { .. }
        | SessionEvent::ContextMark { .. }
        | SessionEvent::SpokenResponse { .. }
        | SessionEvent::Compaction { .. } => None,
    }
}

/// Render a tool output value as message content: strings pass through
/// verbatim; anything else renders as compact JSON via `Value`'s total
/// `Display` — a fallible serializer here could silently collapse a tool
/// result to the empty string.
fn value_to_content_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]
mod tests {
    use super::*;
    use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

    #[test]
    fn empty_events_produce_empty_messages() {
        assert!(events_to_messages(&[]).is_empty());
    }

    /// Persisted reasoning items — both encrypted and plain — must rebuild
    /// onto `Message.reasoning` byte-identical to what was captured, with no
    /// filtering on `encrypted_content` at conversion time (capture
    /// everything; the request serializer filters at replay).
    #[test]
    fn assistant_message_reasoning_items_rebuilt() {
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};

        let encrypted = ReasoningItem {
            id: "rs_enc".to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "encrypted thought".to_owned(),
            }],
            content: None,
            encrypted_content: Some("opaque-blob".to_owned()),
        };
        let plain = ReasoningItem {
            id: "rs_plain".to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "plain thought".to_owned(),
            }],
            content: None,
            encrypted_content: None,
        };
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "answer".to_owned(),
            thinking: "summary".to_owned(),
            reasoning: vec![encrypted.clone(), plain.clone()],
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].reasoning,
            vec![encrypted, plain],
            "rebuilt reasoning must match captured items, encrypted and plain alike",
        );
    }

    /// A legacy `AssistantMessage` event without a `reasoning` field (and
    /// any event captured before the field existed) rebuilds with an empty
    /// reasoning set — no panic, no phantom items.
    #[test]
    fn assistant_message_without_reasoning_rebuilds_empty() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "answer".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert!(msgs[0].reasoning.is_empty());
    }

    #[test]
    fn user_message_converts() {
        let events = vec![SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hello".to_owned(),
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content.as_deref(), Some("hello"));
    }

    #[test]
    fn assistant_message_converts_with_tool_calls() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "the answer".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_tc1".to_owned(),
                name: "read".to_owned(),
                arguments: serde_json::json!({"path": "/tmp/test"}),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::Assistant);
        assert_eq!(msgs[0].content.as_deref(), Some("the answer"));
        assert_eq!(msgs[0].tool_calls.len(), 1);
        assert_eq!(msgs[0].tool_calls[0].name, "read");
        let parsed: serde_json::Value =
            serde_json::from_str(&msgs[0].tool_calls[0].arguments).unwrap();
        assert_eq!(parsed, serde_json::json!({"path": "/tmp/test"}));
    }

    #[test]
    fn assistant_message_thinking_preserved() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "the answer".to_owned(),
            thinking: "first let me reason".to_owned(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].thinking, "first let me reason");
        assert_eq!(msgs[0].content.as_deref(), Some("the answer"));
    }

    #[test]
    fn assistant_message_empty_thinking_skipped_in_serialization() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "hi".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert!(msgs[0].thinking.is_empty());
        let json = serde_json::to_string(&msgs[0]).expect("serialize");
        assert!(
            !json.contains("\"thinking\""),
            "empty thinking should be skipped: {json}"
        );
    }

    #[test]
    fn assistant_message_empty_content_becomes_none() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs[0].content, None);
    }

    #[test]
    fn tool_result_converts() {
        let events = vec![SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc1".to_owned(),
            tool_name: "read".to_owned(),
            output: serde_json::json!({"content": "file data"}),
            spool_ref: None,
            duration_ms: 42,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::ToolResult);
        assert_eq!(msgs[0].tool_call_id.as_deref(), Some("tc1"));
        assert_eq!(msgs[0].tool_name.as_deref(), Some("read"));
        assert!(msgs[0].content.is_some());
    }

    #[test]
    fn tool_result_string_output_unwrapped() {
        let events = vec![SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc1".to_owned(),
            tool_name: "bash".to_owned(),
            output: serde_json::Value::String("hello world".to_owned()),
            spool_ref: None,
            duration_ms: 5,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs[0].content.as_deref(), Some("hello world"));
    }

    #[test]
    fn metadata_events_skipped() {
        let events = vec![
            SessionEvent::ModelChange {
                base: EventBase::new(None),
                old_model: "gpt-4".to_owned(),
                new_model: "gpt-5".to_owned(),
            },
            SessionEvent::Label {
                base: EventBase::new(None),
                label: "checkpoint".to_owned(),
                description: None,
            },
            SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: "test".to_owned(),
                data: serde_json::Value::Null,
            },
        ];
        assert!(events_to_messages(&events).is_empty());
    }

    #[test]
    fn full_conversation_roundtrip() {
        let events = vec![
            SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "what dir?".to_owned(),
            },
            SessionEvent::AssistantMessage {
                response_items: Vec::new(),
                base: EventBase::new(None),
                content: "let me check".to_owned(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: vec![ToolCallEvent {
                    call_id: "call_tc1".to_owned(),
                    name: "bash".to_owned(),
                    arguments: serde_json::json!({"command": "pwd"}),
                    kind: crate::provider::request::ToolCallKind::Function,
                }],
                usage: EventUsage::default(),
                stop_reason: String::new(),
                response_id: None,
            },
            SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "call_tc1".to_owned(),
                tool_name: "bash".to_owned(),
                output: serde_json::json!({"stdout": "/home/user"}),
                spool_ref: None,
                duration_ms: 10,
            },
            SessionEvent::AssistantMessage {
                response_items: Vec::new(),
                base: EventBase::new(None),
                content: "/home/user".to_owned(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: vec![],
                usage: EventUsage::default(),
                stop_reason: String::new(),
                response_id: None,
            },
        ];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[1].role, MessageRole::Assistant);
        assert_eq!(msgs[2].role, MessageRole::ToolResult);
        assert_eq!(msgs[3].role, MessageRole::Assistant);
    }

    #[test]
    fn prompt_conversion_includes_compaction_summary() {
        let events = vec![SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "old work summarized".to_string(),
            replaced_event_ids: Vec::new(),
        }];

        assert!(events_to_messages(&events).is_empty());

        let msgs = prompt_events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::Developer);
        assert!(
            msgs[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("old work summarized")),
        );
    }

    /// Non-string arguments on a Custom-kind call (and all Function-kind
    /// arguments) must render as their full compact JSON — never an
    /// empty string, which the old fallible-serializer fallback could
    /// silently produce.
    #[test]
    fn non_string_arguments_render_full_json_never_empty() {
        let args = serde_json::json!({"nested": {"key": "value"}, "n": 7});
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![
                ToolCallEvent {
                    call_id: "call_custom_obj".to_owned(),
                    name: "apply_patch".to_owned(),
                    arguments: args.clone(),
                    kind: crate::provider::request::ToolCallKind::Custom,
                },
                ToolCallEvent {
                    call_id: "call_fn".to_owned(),
                    name: "read".to_owned(),
                    arguments: args.clone(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
            ],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        for tc in &msgs[0].tool_calls {
            assert!(!tc.arguments.is_empty(), "arguments must never be empty");
            let parsed: serde_json::Value = serde_json::from_str(&tc.arguments).unwrap();
            assert_eq!(parsed, args, "round-trips losslessly for {}", tc.call_id);
        }
    }

    #[test]
    fn rule_injection_renders_prefixed_message_on_resume() {
        use crate::rules::types::{DeliveryMode, TriggerTiming};

        let events = vec![
            SessionEvent::RuleInjection {
                base: EventBase::new(None),
                rule_id: "conv".to_owned(),
                delivery: DeliveryMode::ContextInjection,
                timing: TriggerTiming::Before,
                content: "ctx body".to_owned(),
            },
            SessionEvent::RuleInjection {
                base: EventBase::new(None),
                rule_id: "msg".to_owned(),
                delivery: DeliveryMode::MessageDelivery,
                timing: TriggerTiming::Before,
                content: "msg body".to_owned(),
            },
            SessionEvent::RuleInjection {
                base: EventBase::new(None),
                rule_id: "sys".to_owned(),
                delivery: DeliveryMode::SystemContextAppend,
                timing: TriggerTiming::After,
                content: "sys body".to_owned(),
            },
        ];
        let msgs = events_to_messages(&events);
        // SystemContextAppend delivers via the system prompt, not a message.
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content.as_deref(), Some("[Context: conv] ctx body"));
        assert_eq!(msgs[1].content.as_deref(), Some("[Rule: msg] msg body"));
    }

    #[test]
    fn custom_tool_call_arguments_not_double_quoted() {
        let events = vec![SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_custom".to_owned(),
                name: "apply_patch".to_owned(),
                arguments: serde_json::Value::String("apply patch content".to_owned()),
                kind: crate::provider::request::ToolCallKind::Custom,
            }],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].tool_calls.len(), 1);
        assert_eq!(
            msgs[0].tool_calls[0].arguments, "apply patch content",
            "Custom tool call arguments must not be double-quoted",
        );
        assert_eq!(
            msgs[0].tool_calls[0].kind,
            crate::provider::request::ToolCallKind::Custom,
        );
    }
}
