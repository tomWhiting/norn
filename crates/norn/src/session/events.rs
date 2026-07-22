//! Session event types: message, model change, compaction, child branch,
//! label, custom.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::response_publication_commitment::ResponsePublicationCommitment;

/// Unique identifier for a session event.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventId(String);

impl EventId {
    /// Generate a new unique event ID.
    ///
    /// Uses UUID v7. Nothing in norn relies on v7's time-ordering — the
    /// event LOG's order is the file's append order, and readers never
    /// sort by id — the version is simply retained pending an owner
    /// ruling on whether event ids join R8's session-id switch to v4.
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
    /// Exact Responses `caller` field for the matching result. Old sessions
    /// default to absent; explicit JSON `null` remains distinct.
    #[serde(
        default,
        skip_serializing_if = "crate::provider::request::ToolCallCaller::is_absent"
    )]
    pub caller: crate::provider::request::ToolCallCaller,
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

/// How a child session was minted from its parent — carried on
/// [`SessionEvent::ChildBranch`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildBranchKind {
    /// A fresh-identity child (`spawn_agent` / script spawn): seeded only
    /// with its task, no parent history.
    Spawn,
    /// A same-identity fork: seeded with the parent's full history copy.
    Fork,
}

impl ChildBranchKind {
    /// The wire/display label (`"spawn"` / `"fork"`), matching the serde
    /// form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Fork => "fork",
        }
    }
}

/// Which live context-edit mark a [`SessionEvent::ContextMark`] mirrors.
///
/// This is a **mechanical** discriminator: it records which of the live
/// [`ContextEdits`](crate::session::context_edit::ContextEdits) mark sets
/// the target event was added to, and nothing more. It deliberately
/// carries no reason, annotation, or curation vocabulary — that layer is
/// a separate design and must not grow here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMarkKind {
    /// The target event is suppressed: excluded from prompt construction
    /// while remaining in the store unchanged.
    Suppress,
    /// The target event entered the store through
    /// [`ContextEdits::inject`](crate::session::context_edit::ContextEdits::inject)
    /// and is tagged as an injection in the prompt view.
    Inject,
}

/// Why provider-owned conversation state must begin a new epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEpochBoundaryReason {
    /// The visible timeline was migrated from legacy storage, whose provider
    /// anchors cannot be proven to name the exact strict replay state.
    MigratedLegacy,
    /// A session created before provider-state affinity was recorded has
    /// adopted its first credential-and-authority identity. Earlier response
    /// anchors cannot be attributed to that identity and must not be reused.
    ProviderIdentityAdoption,
    /// The following records form a legacy, uncommitted provider-state
    /// publication group.
    ResponseStatePublication,
    /// The following provider-state publication group is committed by its
    /// ordered length and canonical digest.
    ResponseStatePublicationV1(ResponsePublicationCommitment),
    /// A non-identity fork changed the provider-facing history.
    FilteredFork,
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
        /// Ordered completed Responses items for this turn. Empty on legacy
        /// sessions and providers without a Responses-compatible item model.
        /// When non-empty, this is the authoritative replay representation;
        /// the flat fields below are compatibility projections.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        response_items: Vec<crate::provider::response_item::ResponseTranscriptItem>,
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
        /// Why the model stopped generating (`end_turn`, `continue_turn`,
        /// `tool_use`, `max_tokens`, `content_filter`). Empty string for events
        /// persisted before this field was added.
        #[serde(default)]
        stop_reason: String,
        /// Server-assigned response ID returned by the provider. It is usable
        /// for conversation chaining only when a preceding
        /// [`crate::session::ProviderStateProvenance`] custom event targets it
        /// and records `stored: true`.
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
        /// Structured output from the tool. When the tool's full output
        /// exceeded the model-facing inline budget, this is the bounded
        /// head/tail projection and [`spool_ref`](Self::ToolResult::spool_ref)
        /// points at the verbatim full payload.
        output: serde_json::Value,
        /// Data-dir-relative reference
        /// (`<root-session-id>/spool/<event-id>.bin` — the id component
        /// is the OWNING ROOT session's; child sessions spool into the
        /// same root-keyed directory their timeline lives under) to the
        /// spooled verbatim full output, present only when `output` is
        /// the bounded
        /// projection of an over-budget payload persisted through a
        /// spool-equipped store. Resolved via
        /// [`read_spooled_output`](crate::session::spool::read_spooled_output).
        /// `None` for outputs within budget (the event carries the full
        /// output inline) and for events persisted before the spool
        /// existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spool_ref: Option<String>,
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

    /// Durable boundary invalidating every earlier provider response anchor.
    ///
    /// This event produces no prompt content. A later native assistant
    /// response may establish the first anchor of the new provider epoch.
    ProviderEpochBoundary {
        /// Common event metadata.
        base: EventBase,
        /// Why the prior provider epoch cannot continue.
        reason: ProviderEpochBoundaryReason,
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

    /// The durable branch-point recording that a child session was minted
    /// under this timeline.
    ///
    /// Appended to the **parent's** store as the child's name reservation
    /// (PARENT-FIRST: this event is durable before any child file or index
    /// row keyed by the name exists), and to the **child's** store as its
    /// provenance header. The parent's ever-used child-name set is replayed
    /// from these events — a name recorded here is reserved for all time
    /// within that parent, even for ephemeral children (whose reservation
    /// append is the only durable trace they leave).
    ChildBranch {
        /// Common event metadata.
        base: EventBase,
        /// Session id of the minting parent. `None` when the parent itself
        /// runs ephemeral (no persisted session). Lets a fork-seeded child
        /// distinguish reservation events it inherited in its seed copy
        /// from reservations it appended as a parent itself.
        parent_session_id: Option<String>,
        /// The child's session id. `None` records an ephemeral child
        /// honestly — absence stated, never a fake id.
        child_session_id: Option<String>,
        /// The child's full coordination path address, e.g.
        /// `root/fork-1a2b3c4d`. The last segment is the reserved
        /// per-parent name.
        path_address: String,
        /// The parent's last event id at branch time (`None` for an empty
        /// parent log) — the durable anchor for where in the parent's
        /// timeline the branch occurred.
        parent_event_anchor: Option<EventId>,
        /// Whether the child was minted by `fork` (history-seeded) or
        /// `spawn` (fresh).
        kind: ChildBranchKind,
    },

    /// Completion reference for a previously forked child session.
    ///
    /// Appended to the parent's timeline when a forked sub-agent reaches a
    /// terminal status. Carries a pointer to the child session plus the
    /// validated structured output, accumulated token usage, and wall-clock
    /// duration. This is a *completion reference*, not a content merge —
    /// the child's own events remain in its own session file (under the
    /// root's `children/` directory), and visualisers can render the branch
    /// joining back at this event without flattening the tree into a DAG.
    ForkComplete {
        /// Common event metadata.
        base: EventBase,
        /// Session id of the forked child this event reports back on.
        /// `None` when the fork ran ephemeral and left no session file —
        /// absence stated honestly, never a registry-id stand-in that
        /// points at a session existing nowhere on disk. Events persisted
        /// before this field became optional always carry a value and
        /// deserialize as `Some`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forked_session_id: Option<String>,
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

    /// The durable twin of a live context-edit mark (suppress / inject).
    ///
    /// Appended by
    /// [`ContextEdits`](crate::session::context_edit::ContextEdits) at the
    /// moment the live mark is applied, so a resumed session rebuilds the
    /// identical prompt view instead of silently resurfacing suppressed
    /// events or losing injection tags. Compaction supersession needs no
    /// twin — [`SessionEvent::Compaction`] already carries
    /// `replaced_event_ids`.
    ///
    /// This event is pure bookkeeping: it never renders into the prompt,
    /// produces no provider message, and carries exactly what the live
    /// mark holds — the mark kind and the target event ID. No reasons, no
    /// annotation vocabulary (held for a separate design).
    ContextMark {
        /// Common event metadata.
        base: EventBase,
        /// Which live mark set this event mirrors.
        mark: ContextMarkKind,
        /// ID of the event the mark applies to.
        target_event_id: EventId,
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
        /// Provenance-derived authority of the rule. Missing only on readable
        /// pre-D8 rows, which reconstruct conservatively at User authority.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::rules::source::RuleOrigin>,
        /// How the rule content was delivered to the model.
        delivery: crate::rules::types::DeliveryMode,
        /// Whether the rule fired before or after the matched action.
        timing: crate::rules::types::TriggerTiming,
        /// The raw (unformatted) rule content that was delivered. The
        /// delivery-mode formatting is applied by the canonical rule
        /// projection so the stored content stays canonical.
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
            | Self::ProviderEpochBoundary { base, .. }
            | Self::Compaction { base, .. }
            | Self::ChildBranch { base, .. }
            | Self::ForkComplete { base, .. }
            | Self::Label { base, .. }
            | Self::Custom { base, .. }
            | Self::ContextMark { base, .. }
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
            spool_ref: None,
            duration_ms: 150,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let _: SessionEvent = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn tool_result_line_without_spool_ref_deserializes_as_none() {
        // A JSONL line persisted before the spool existed (session-fidelity
        // Gap 5) has no `spool_ref` field and must deserialize with
        // #[serde(default)] to `None`.
        let legacy = serde_json::json!({
            "type": "ToolResult",
            "base": {
                "id": EventId::new().to_string(),
                "parent_id": null,
                "timestamp": Utc::now(),
            },
            "tool_call_id": "tc_pre_spool",
            "tool_name": "read",
            "output": {"lines": 42},
            "duration_ms": 5,
        });
        let parsed: SessionEvent =
            serde_json::from_value(legacy).expect("legacy line deserializes");
        match parsed {
            SessionEvent::ToolResult { spool_ref, .. } => {
                assert!(spool_ref.is_none(), "missing field defaults to None");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn tool_result_spool_ref_round_trips_and_none_is_not_serialized() {
        // The reference must survive append/read intact, and within-budget
        // results (`None`) keep the exact line shape events had before the
        // spool existed — `skip_serializing_if` elides the field.
        let spooled = SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc_spooled".to_owned(),
            tool_name: "bash".to_owned(),
            output: serde_json::json!({"truncated_for_model": true}),
            spool_ref: Some("sess-1/spool/evt-1.bin".to_owned()),
            duration_ms: 9,
        };
        let json = serde_json::to_string(&spooled).expect("serialize");
        match serde_json::from_str::<SessionEvent>(&json).expect("deserialize") {
            SessionEvent::ToolResult { spool_ref, .. } => {
                assert_eq!(spool_ref.as_deref(), Some("sess-1/spool/evt-1.bin"));
            }
            _ => panic!("expected ToolResult"),
        }

        let inline = SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc_inline".to_owned(),
            tool_name: "bash".to_owned(),
            output: serde_json::json!({"ok": true}),
            spool_ref: None,
            duration_ms: 1,
        };
        let json = serde_json::to_string(&inline).expect("serialize");
        assert!(
            !json.contains("spool_ref"),
            "a None spool_ref must not appear on the persisted line: {json}",
        );
    }

    #[test]
    fn assistant_message_serde_roundtrip_with_thinking() {
        let event = SessionEvent::AssistantMessage {
            response_items: Vec::new(),
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
            response_items: Vec::new(),
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
        assert!(
            !json.contains("response_audio"),
            "a missing audio sidecar must preserve the legacy wire shape: {json}",
        );
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
            response_items: Vec::new(),
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
            response_items: Vec::new(),
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
    fn context_mark_serde_roundtrip_both_kinds() {
        for kind in [ContextMarkKind::Suppress, ContextMarkKind::Inject] {
            let target = EventId::new();
            let event = SessionEvent::ContextMark {
                base: EventBase::new(None),
                mark: kind,
                target_event_id: target.clone(),
            };
            let json = serde_json::to_string(&event).expect("serialize");
            assert!(json.contains("\"ContextMark\""), "tagged variant: {json}");
            let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
            match parsed {
                SessionEvent::ContextMark {
                    mark,
                    target_event_id,
                    ..
                } => {
                    assert_eq!(mark, kind);
                    assert_eq!(target_event_id, target);
                }
                _ => panic!("expected ContextMark"),
            }
        }
    }

    #[test]
    fn context_mark_kind_wire_names_are_snake_case() {
        // The wire discriminator is part of the persisted format; pin it.
        assert_eq!(
            serde_json::to_string(&ContextMarkKind::Suppress).expect("serialize"),
            "\"suppress\""
        );
        assert_eq!(
            serde_json::to_string(&ContextMarkKind::Inject).expect("serialize"),
            "\"inject\""
        );
    }

    #[test]
    fn rule_injection_serde_roundtrip() {
        let event = SessionEvent::RuleInjection {
            base: EventBase::new(None),
            rule_id: "rust-conventions".to_owned(),
            origin: Some(crate::rules::source::RuleOrigin::Operator),
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
                response_items: Vec::new(),
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
                spool_ref: None,
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
            SessionEvent::ChildBranch {
                base: base(),
                parent_session_id: None,
                child_session_id: None,
                path_address: "root/spawn-abcd1234".to_owned(),
                parent_event_anchor: Some(id),
                kind: ChildBranchKind::Spawn,
            },
            SessionEvent::ForkComplete {
                base: base(),
                forked_session_id: None,
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
                origin: None,
                delivery: crate::rules::types::DeliveryMode::ContextInjection,
                timing: crate::rules::types::TriggerTiming::Before,
                content: String::new(),
            },
            SessionEvent::ContextMark {
                base: base(),
                mark: ContextMarkKind::Suppress,
                target_event_id: EventId::new(),
            },
        ];
        for e in &events {
            let _ = e.base();
        }
    }

    /// Old persisted `ForkComplete` lines always carried a
    /// `forked_session_id` string; after the field became `Option` they
    /// must deserialize as `Some(..)` — no old-file breakage.
    #[test]
    fn fork_complete_old_shape_deserializes_as_some() {
        let old_line = serde_json::json!({
            "type": "ForkComplete",
            "base": {
                "id": EventId::new().to_string(),
                "parent_id": null,
                "timestamp": Utc::now(),
            },
            "forked_session_id": "0192f7e2-aaaa-bbbb-cccc-1234567890ab",
            "result_summary": {"response": "done"},
            "usage": {
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
            },
            "duration_ms": 42,
        });
        let parsed: SessionEvent =
            serde_json::from_value(old_line).expect("old-shape line deserializes");
        match parsed {
            SessionEvent::ForkComplete {
                forked_session_id, ..
            } => {
                assert_eq!(
                    forked_session_id.as_deref(),
                    Some("0192f7e2-aaaa-bbbb-cccc-1234567890ab"),
                    "the always-present old field must land as Some",
                );
            }
            other => panic!("expected ForkComplete, got {other:?}"),
        }
    }

    /// An ephemeral fork's `ForkComplete` omits the key entirely and a
    /// missing key deserializes as `None` — honest absence round-trips.
    #[test]
    fn fork_complete_none_omits_key_and_roundtrips() {
        let event = SessionEvent::ForkComplete {
            base: EventBase::new(None),
            forked_session_id: None,
            result_summary: serde_json::Value::Null,
            usage: EventUsage::default(),
            duration_ms: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            !json.contains("forked_session_id"),
            "None must omit the key: {json}",
        );
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::ForkComplete {
                forked_session_id, ..
            } => assert!(forked_session_id.is_none()),
            other => panic!("expected ForkComplete, got {other:?}"),
        }
    }

    #[test]
    fn child_branch_serde_roundtrip() {
        let anchor = EventId::new();
        let event = SessionEvent::ChildBranch {
            base: EventBase::new(None),
            parent_session_id: Some("parent-id".to_owned()),
            child_session_id: Some("child-id".to_owned()),
            path_address: "root/reviewer-1a2b3c4d".to_owned(),
            parent_event_anchor: Some(anchor.clone()),
            kind: ChildBranchKind::Fork,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: SessionEvent = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            SessionEvent::ChildBranch {
                parent_session_id,
                child_session_id,
                path_address,
                parent_event_anchor,
                kind,
                ..
            } => {
                assert_eq!(parent_session_id.as_deref(), Some("parent-id"));
                assert_eq!(child_session_id.as_deref(), Some("child-id"));
                assert_eq!(path_address, "root/reviewer-1a2b3c4d");
                assert_eq!(parent_event_anchor, Some(anchor));
                assert_eq!(kind, ChildBranchKind::Fork);
            }
            other => panic!("expected ChildBranch, got {other:?}"),
        }
    }
}
