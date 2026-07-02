//! Session event types: message, model change, compaction, fork, label, custom.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a session event.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventId(String);

impl EventId {
    /// Generate a new unique event ID using UUID v7 (time-sortable).
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for EventId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl EventId {
    /// Return the inner string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Common base for every session event, forming a tree via parent links.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventBase {
    /// Unique identifier for this event.
    pub id: EventId,
    /// Parent event ID, forming a tree structure. `None` for root events.
    pub parent_id: Option<EventId>,
    /// When this event was created.
    pub timestamp: DateTime<Utc>,
}

impl EventBase {
    /// Create a new `EventBase` with a fresh ID and the current timestamp.
    #[must_use]
    pub fn new(parent_id: Option<EventId>) -> Self {
        Self {
            id: EventId::new(),
            parent_id,
            timestamp: Utc::now(),
        }
    }
}

/// A tool call referenced from an assistant message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallEvent {
    /// Provider-assigned correlation identifier (`call_*`). This is the
    /// only identifier the model accepts on a follow-up
    /// `function_call_output` echo; the `fc_*` item identifier from the
    /// stream is not persisted.
    pub call_id: String,
    /// Name of the tool being called.
    pub name: String,
    /// Structured arguments passed to the tool. For
    /// [`ToolCallKind::Custom`](crate::provider::request::ToolCallKind::Custom)
    /// calls this is the freeform `input` string wrapped as
    /// [`serde_json::Value::String`] — the wire form has no JSON envelope, so
    /// the persisted JSON value mirrors that.
    pub arguments: serde_json::Value,
    /// Which surface kind this call uses (function vs custom). Defaults to
    /// [`ToolCallKind::Function`](crate::provider::request::ToolCallKind::Function)
    /// so pre-existing session events deserialize without migration.
    #[serde(default)]
    pub kind: crate::provider::request::ToolCallKind,
}

/// Token usage from a single provider call.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventUsage {
    /// Tokens consumed by the input prompt.
    pub input_tokens: u64,
    /// Tokens produced in the response.
    pub output_tokens: u64,
    /// Tokens served from the provider's prompt cache.
    pub cache_read_tokens: u64,
    /// Tokens written into the provider's prompt cache.
    pub cache_write_tokens: u64,
    /// Estimated cost in USD, if the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A single session event. Each variant embeds an [`EventBase`] via the
/// `base` field. Events are self-contained — no accumulated state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEvent {
    /// A message from the user.
    UserMessage {
        /// Common event metadata.
        base: EventBase,
        /// The user's message text.
        content: String,
    },

    /// A message from the assistant, possibly including tool calls.
    AssistantMessage {
        /// Common event metadata.
        base: EventBase,
        /// The assistant's text content. Empty string when no text was produced.
        content: String,
        /// The assistant's reasoning/thinking content. Empty string when none.
        thinking: String,
        /// Structured reasoning output items captured from the provider
        /// stream (`OpenAI` Responses `response.output_item.done` with
        /// `item.type == "reasoning"`). Persisted so a resumed session can
        /// replay the model's reasoning state across tool iterations — on
        /// stateless-replay backends the `encrypted_content` blob is the
        /// only way to thread reasoning, and dropping these items silently
        /// shrinks a resumed conversation (a real incident lost ~30k tokens
        /// of reasoning when a 269k live run resumed at ~236k).
        ///
        /// This reuses [`crate::provider::reasoning::ReasoningItem`]
        /// directly rather than minting a session-local mirror (as
        /// [`EventUsage`]/[`ToolCallEvent`] do): `ReasoningItem` is already
        /// the stable persisted wire shape — it is serde-round-tripped
        /// inside [`Message`](crate::provider::request::Message) today and
        /// its field attributes were designed for persistence — so
        /// mirroring it (plus `ReasoningSummaryPart`/`ReasoningContentPart`)
        /// would buy zero schema benefit. Capture-everything here; the
        /// request serializer filters to items carrying `encrypted_content`
        /// at replay time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reasoning: Vec<crate::provider::reasoning::ReasoningItem>,
        /// Tool calls made in this response.
        tool_calls: Vec<ToolCallEvent>,
        /// Token usage for this provider call.
        usage: EventUsage,
        /// Why the model stopped generating (`end_turn`, `tool_use`,
        /// `max_tokens`, `content_filter`). Empty string for events
        /// persisted before this field was added.
        #[serde(default)]
        stop_reason: String,
        /// Server-assigned response ID for conversation chaining.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
    },

    /// A TTS-friendly spoken response produced via the `spoken_response`
    /// dynamic tool. The `content` is the validated tool argument object
    /// conforming to the `SpokenResponse` event schema.
    SpokenResponse {
        /// Common event metadata.
        base: EventBase,
        /// Validated spoken-response content conforming to the configured schema.
        content: serde_json::Value,
    },

    /// The result of executing a tool.
    ToolResult {
        /// Common event metadata.
        base: EventBase,
        /// ID of the tool call this result corresponds to.
        tool_call_id: String,
        /// Name of the tool that was executed.
        tool_name: String,
        /// Structured output from the tool.
        output: serde_json::Value,
        /// Execution duration in milliseconds.
        duration_ms: u64,
    },

    /// A change in the active model.
    ModelChange {
        /// Common event metadata.
        base: EventBase,
        /// The model being switched from.
        old_model: String,
        /// The model being switched to.
        new_model: String,
    },

    /// A compaction summary replacing a range of earlier events.
    Compaction {
        /// Common event metadata.
        base: EventBase,
        /// Summary text for the compacted range.
        summary: String,
        /// IDs of events replaced by this compaction.
        replaced_event_ids: Vec<EventId>,
    },

    /// A fork event linking to a child session.
    Fork {
        /// Common event metadata.
        base: EventBase,
        /// The event from which the fork originated.
        source_event_id: EventId,
        /// ID of the forked child session.
        forked_session_id: String,
    },

    /// Completion reference for a previously forked child session.
    ///
    /// Appended to the parent's timeline when a forked sub-agent reaches a
    /// terminal status. Carries a pointer to the child session plus the
    /// validated structured output, accumulated token usage, and wall-clock
    /// duration. This is a *completion reference*, not a content merge — the
    /// child's own events remain in the [`SessionTree`](crate::session::tree::SessionTree)
    /// branch, and visualisers can render the branch joining back at this
    /// event without flattening the tree into a DAG.
    ForkComplete {
        /// Common event metadata.
        base: EventBase,
        /// ID of the forked child session this event reports back on.
        forked_session_id: String,
        /// Structured output produced by the fork (validated against the
        /// fork's output schema when one was supplied).
        result_summary: serde_json::Value,
        /// Accumulated token usage across every provider call the fork made.
        usage: EventUsage,
        /// Wall-clock duration of the fork in milliseconds.
        duration_ms: u64,
    },

    /// A named checkpoint in the session timeline.
    Label {
        /// Common event metadata.
        base: EventBase,
        /// Label name.
        label: String,
        /// Optional description.
        description: Option<String>,
    },

    /// An application-defined custom event.
    Custom {
        /// Common event metadata.
        base: EventBase,
        /// Application-specific event type discriminator.
        event_type: String,
        /// Arbitrary structured data.
        data: serde_json::Value,
    },

    /// A rules-engine injection that fired and entered the active context.
    ///
    /// Persisted for every fired rule regardless of delivery mode so the
    /// event stream is the single source of truth for rule presence: the
    /// prompt view tags this event
    /// [`ContentTag::Rule`](crate::agent_loop::context::ContentTag::Rule),
    /// the rules engine rebuilds its presence set from those tags, and a
    /// rule is only re-injected once its event has been compacted or
    /// suppressed out of the view. Also the immutable audit record of which
    /// rule fired and how its content was delivered.
    RuleInjection {
        /// Common event metadata.
        base: EventBase,
        /// Identifier of the rule that fired.
        rule_id: String,
        /// How the rule content was delivered to the model.
        delivery: crate::rules::types::DeliveryMode,
        /// Whether the rule fired before or after the matched action.
        timing: crate::rules::types::TriggerTiming,
        /// The raw (unformatted) rule content that was delivered. The
        /// delivery-mode prefix is applied at render time via
        /// [`DeliveryMode::format_conversation_content`](crate::rules::types::DeliveryMode::format_conversation_content)
        /// so the stored content stays canonical.
        content: String,
    },
}

impl SessionEvent {
    /// Return the common [`EventBase`] for any event variant.
    #[must_use]
    pub fn base(&self) -> &EventBase {
        match self {
            Self::UserMessage { base, .. }
            | Self::AssistantMessage { base, .. }
            | Self::SpokenResponse { base, .. }
            | Self::ToolResult { base, .. }
            | Self::ModelChange { base, .. }
            | Self::Compaction { base, .. }
            | Self::Fork { base, .. }
            | Self::ForkComplete { base, .. }
            | Self::Label { base, .. }
            | Self::Custom { base, .. }
            | Self::RuleInjection { base, .. } => base,
        }
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
    use super::*;

    #[test]
    fn event_id_unique() {
        let a = EventId::new();
        let b = EventId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn event_id_display_fromstr_roundtrip() {
        let id = EventId::new();
        let s = id.to_string();
        let parsed: EventId = s.parse().expect("infallible");
        assert_eq!(id, parsed);
    }

    #[test]
    fn event_base_has_timestamp() {
        let before = Utc::now();
        let base = EventBase::new(None);
        let after = Utc::now();
        assert!(base.timestamp >= before);
        assert!(base.timestamp <= after);
    }

    #[test]
    fn session_event_base_accessor() {
        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hello".to_owned(),
        };
        assert!(event.base().parent_id.is_none());
    }

    #[test]
    fn session_event_serde_roundtrip() {
        let event = SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc_1".to_owned(),
            tool_name: "Read".to_owned(),
            output: serde_json::json!({"lines": 42}),
            duration_ms: 150,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let _: SessionEvent = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn assistant_message_serde_roundtrip_with_thinking() {
        let event = SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "answer".to_owned(),
            thinking: "first let me think".to_owned(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::AssistantMessage {
                content, thinking, ..
            } => {
                assert_eq!(content, "answer");
                assert_eq!(thinking, "first let me think");
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn assistant_message_serde_roundtrip_empty() {
        let event = SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::AssistantMessage {
                content, thinking, ..
            } => {
                assert!(content.is_empty());
                assert!(thinking.is_empty());
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn assistant_message_empty_reasoning_omitted_from_wire() {
        // Wire-format stability: an AssistantMessage with no reasoning must
        // not emit a "reasoning" key (skip_serializing_if pinned), so events
        // persisted after this change are byte-identical to the pre-change
        // format when reasoning is absent.
        let event = SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "answer".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            !json.contains("\"reasoning\""),
            "empty reasoning must be skipped: {json}"
        );
    }

    #[test]
    fn assistant_message_legacy_line_without_reasoning_deserializes_empty() {
        // A legacy JSONL line persisted before the `reasoning` field existed
        // must deserialize with #[serde(default)] to an empty vec.
        let legacy = serde_json::json!({
            "type": "AssistantMessage",
            "base": {
                "id": EventId::new().to_string(),
                "parent_id": null,
                "timestamp": Utc::now(),
            },
            "content": "answer",
            "thinking": "",
            "tool_calls": [],
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
            },
        });
        let parsed: SessionEvent =
            serde_json::from_value(legacy).expect("legacy line deserializes");
        match parsed {
            SessionEvent::AssistantMessage { reasoning, .. } => {
                assert!(reasoning.is_empty(), "missing field defaults to empty vec");
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn assistant_message_reasoning_serde_roundtrip() {
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
        let item = ReasoningItem {
            id: "rs_1".to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "thought".to_owned(),
            }],
            content: None,
            encrypted_content: Some("blob".to_owned()),
        };
        let event = SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "answer".to_owned(),
            thinking: String::new(),
            reasoning: vec![item.clone()],
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"reasoning\""));
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::AssistantMessage { reasoning, .. } => {
                assert_eq!(reasoning, vec![item]);
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn rule_injection_serde_roundtrip() {
        let event = SessionEvent::RuleInjection {
            base: EventBase::new(None),
            rule_id: "rust-conventions".to_owned(),
            delivery: crate::rules::types::DeliveryMode::SystemContextAppend,
            timing: crate::rules::types::TriggerTiming::After,
            content: "Follow conventions.".to_owned(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::RuleInjection {
                rule_id,
                delivery,
                content,
                ..
            } => {
                assert_eq!(rule_id, "rust-conventions");
                assert_eq!(
                    delivery,
                    crate::rules::types::DeliveryMode::SystemContextAppend
                );
                assert_eq!(content, "Follow conventions.");
            }
            _ => panic!("expected RuleInjection"),
        }
    }

    #[test]
    fn all_variants_base_accessor() {
        let base = || EventBase::new(None);
        let id = EventId::new();
        let events = vec![
            SessionEvent::UserMessage {
                base: base(),
                content: String::new(),
            },
            SessionEvent::AssistantMessage {
                base: base(),
                content: String::new(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: vec![],
                usage: EventUsage::default(),
                stop_reason: String::new(),
                response_id: None,
            },
            SessionEvent::SpokenResponse {
                base: base(),
                content: serde_json::Value::Null,
            },
            SessionEvent::ToolResult {
                base: base(),
                tool_call_id: String::new(),
                tool_name: String::new(),
                output: serde_json::Value::Null,
                duration_ms: 0,
            },
            SessionEvent::ModelChange {
                base: base(),
                old_model: String::new(),
                new_model: String::new(),
            },
            SessionEvent::Compaction {
                base: base(),
                summary: String::new(),
                replaced_event_ids: vec![],
            },
            SessionEvent::Fork {
                base: base(),
                source_event_id: id,
                forked_session_id: String::new(),
            },
            SessionEvent::ForkComplete {
                base: base(),
                forked_session_id: String::new(),
                result_summary: serde_json::Value::Null,
                usage: EventUsage::default(),
                duration_ms: 0,
            },
            SessionEvent::Label {
                base: base(),
                label: String::new(),
                description: None,
            },
            SessionEvent::Custom {
                base: base(),
                event_type: String::new(),
                data: serde_json::Value::Null,
            },
            SessionEvent::RuleInjection {
                base: base(),
                rule_id: String::new(),
                delivery: crate::rules::types::DeliveryMode::ContextInjection,
                timing: crate::rules::types::TriggerTiming::Before,
                content: String::new(),
            },
        ];
        for e in &events {
            let _ = e.base();
        }
    }
}
