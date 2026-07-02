//! Agent-tagged runtime events for multi-agent observability.
//!
//! [`ProviderEvent`] is the provider
//! contract — raw SSE stream events with no knowledge of agent identity.
//! [`AgentEvent`] wraps an [`AgentEventKind`] with the emitting agent's
//! `id` and `role`, making every event attributable in a multi-agent
//! runtime. The kind is either a raw provider stream event or a typed
//! [`SubagentLifecycle`] event emitted by the spawn/fork tools when a
//! child starts and completes.
//!
//! [`AgentEventSender`] ensures tagging is automatic: callers pass bare
//! [`ProviderEvent`] values (or [`SubagentLifecycle`] values via
//! [`AgentEventSender::send_subagent`]), and the sender stamps them with
//! the agent's identity before broadcasting. A child sender is created
//! from a parent via [`AgentEventSender::for_child`], sharing the
//! underlying channel but carrying the child's identity.
//!
//! [`SharedAgentEventChannel`] is a [`ToolContext`](crate::tool::context::ToolContext)
//! extension holding the raw broadcast sender, so fork/spawn tools can
//! clone it and build child senders without threading the channel
//! through every function signature.
//!
//! ## Subagent lifecycle events
//!
//! [`SubagentLifecycle`] is the typed contract embedders consume instead
//! of reverse-engineering child lifecycle from tool-output JSON. The
//! spawn and fork tools emit [`SubagentLifecycle::Started`] when a child
//! launches and [`SubagentLifecycle::Completed`] when it reaches a
//! terminal outcome, on two carriers:
//!
//! - **Live**: tagged [`AgentEvent`]s (kind
//!   [`AgentEventKind::Subagent`]) on the shared broadcast channel,
//!   stamped with the *child's* identity.
//! - **Replay/audit**: [`SessionEvent::Custom`](crate::session::events::SessionEvent::Custom)
//!   events appended to the *parent's* session store with `event_type`
//!   [`SUBAGENT_STARTED_EVENT_TYPE`] / [`SUBAGENT_COMPLETED_EVENT_TYPE`]
//!   and the serialized lifecycle event as `data`.
//!
//! The serde representation is stable: `snake_case` tags (`phase`,
//! `kind`), RFC 3339 timestamps, and the typed
//! [`AgentStopReason`] / [`Usage`] payloads.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::events::ProviderEvent;
use super::usage::Usage;
use crate::agent::output::AgentStopReason;
use crate::r#loop::inbound::MessageKind;

/// `event_type` of the [`SessionEvent::Custom`](crate::session::events::SessionEvent::Custom)
/// appended to the parent's store when a child launches. The event's
/// `data` is a serialized [`SubagentLifecycle::Started`].
pub const SUBAGENT_STARTED_EVENT_TYPE: &str = "subagent.started";

/// `event_type` of the [`SessionEvent::Custom`](crate::session::events::SessionEvent::Custom)
/// appended when the [`MessageRouter`](crate::agent::message_router::MessageRouter)
/// accepts an inter-agent message. The event's `data` is a serialized
/// [`AgentMessageLifecycle::Sent`].
pub const AGENT_MESSAGE_SENT_EVENT_TYPE: &str = "agent_message.sent";

/// `event_type` of the [`SessionEvent::Custom`](crate::session::events::SessionEvent::Custom)
/// appended to the recipient's store immediately before the framed
/// `UserMessage` when its loop injects a routed message. The event's
/// `data` is a serialized [`AgentMessageLifecycle::Delivered`].
pub const AGENT_MESSAGE_DELIVERED_EVENT_TYPE: &str = "agent_message.delivered";

/// `event_type` of the [`SessionEvent::Custom`](crate::session::events::SessionEvent::Custom)
/// appended to the parent's store when a child reaches a terminal
/// outcome. The event's `data` is a serialized
/// [`SubagentLifecycle::Completed`].
pub const SUBAGENT_COMPLETED_EVENT_TYPE: &str = "subagent.completed";

/// Which delegation surface created a child agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentKind {
    /// `spawn_agent` — an asynchronous sub-agent with a fresh context.
    Spawn,
    /// `fork` — a sub-agent inheriting the parent's conversation context.
    Fork,
}

/// Identity and provenance of a child agent, carried on both lifecycle
/// phases so each event is self-contained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentDescriptor {
    /// Which tool created the child.
    pub kind: SubagentKind,
    /// Registry role label — the spawn `role` argument, or `"fork"`.
    pub role: String,
    /// Model identifier the child runs on.
    pub model: String,
    /// Resolved profile name when the child was spawned from a profile.
    /// Always `None` for forks.
    pub profile: Option<String>,
}

/// Typed lifecycle event for a spawned or forked child agent.
///
/// Emitted by the spawn/fork tools themselves — embedders match on this
/// instead of parsing tool-output JSON. Serialization is stable:
/// internally tagged on `phase` with `snake_case` variant names.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SubagentLifecycle {
    /// A child agent was launched and is now running.
    Started {
        /// Registry id of the agent that launched the child.
        parent_id: Uuid,
        /// Registry id of the child.
        child_id: Uuid,
        /// Provenance: kind, role, model, profile.
        descriptor: SubagentDescriptor,
        /// Wall-clock launch time.
        started_at: DateTime<Utc>,
    },
    /// A child agent's run reached a terminal outcome.
    ///
    /// `succeeded: true` means the run completed; `false` means it
    /// failed (`error` describes why) or stopped early (`stop` carries
    /// the typed [`AgentStopReason`]).
    Completed {
        /// Registry id of the agent that launched the child.
        parent_id: Uuid,
        /// Registry id of the child.
        child_id: Uuid,
        /// Provenance: kind, role, model, profile.
        descriptor: SubagentDescriptor,
        /// Wall-clock launch time (same value as the `Started` event).
        started_at: DateTime<Utc>,
        /// Wall-clock completion time.
        completed_at: DateTime<Utc>,
        /// Accumulated token usage across every provider call the child
        /// made — own calls only; descendant spend lives exclusively on
        /// `subtree_usage`.
        usage: Usage,
        /// Aggregated usage of the child's entire delegation subtree
        /// (W3.6 usage rollup, DECISION R4): `usage` plus the summed
        /// `subtree_usage` of every child result this child's own loop
        /// delivered. Each agent's own spend is counted exactly once,
        /// at its own level. The zeros-mean-unknown caveat propagates:
        /// a panicked or hard-errored child contributes unknown-zeros
        /// for its own spend while delivered descendant subtrees are
        /// still folded in.
        ///
        /// **Breaking schema change** to this documented-stable event,
        /// pre-announced in MERIDIAN-HANDOFF §8.2. Deliberately a
        /// required field with no `#[serde(default)]`: no norn path
        /// deserializes stored `subagent.completed` events (resume
        /// replays them as opaque `SessionEvent::Custom` data), and a
        /// store-only consumer replaying pre-W3.6 events through this
        /// type should fail loudly at the announced break rather than
        /// silently read fabricated zeros. The write side always
        /// populates it.
        subtree_usage: Usage,
        /// Whether the child's run completed successfully.
        succeeded: bool,
        /// Explanatory error when `succeeded` is `false`.
        error: Option<String>,
        /// Typed stop reason when the child stopped early without
        /// completing (schema budget, max iterations, timeout,
        /// cancellation, truncation). `None` on success or hard error.
        stop: Option<AgentStopReason>,
    },
}

impl SubagentLifecycle {
    /// Registry id of the child this event concerns.
    #[must_use]
    pub const fn child_id(&self) -> Uuid {
        match self {
            Self::Started { child_id, .. } | Self::Completed { child_id, .. } => *child_id,
        }
    }

    /// Registry id of the parent that launched the child.
    #[must_use]
    pub const fn parent_id(&self) -> Uuid {
        match self {
            Self::Started { parent_id, .. } | Self::Completed { parent_id, .. } => *parent_id,
        }
    }

    /// The child's provenance descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &SubagentDescriptor {
        match self {
            Self::Started { descriptor, .. } | Self::Completed { descriptor, .. } => descriptor,
        }
    }

    /// The session-store `event_type` for this phase
    /// ([`SUBAGENT_STARTED_EVENT_TYPE`] / [`SUBAGENT_COMPLETED_EVENT_TYPE`]).
    #[must_use]
    pub const fn session_event_type(&self) -> &'static str {
        match self {
            Self::Started { .. } => SUBAGENT_STARTED_EVENT_TYPE,
            Self::Completed { .. } => SUBAGENT_COMPLETED_EVENT_TYPE,
        }
    }
}

/// Typed lifecycle event for an inter-agent message, following the
/// [`SubagentLifecycle`] dual-carrier pattern: a live child-tagged
/// [`AgentEvent`] on the broadcast channel plus a
/// `SessionEvent::Custom` audit record (constants
/// [`AGENT_MESSAGE_SENT_EVENT_TYPE`] /
/// [`AGENT_MESSAGE_DELIVERED_EVENT_TYPE`]).
///
/// `Sent` precedes `Delivered` for the same `message_id` in any merged
/// view — `Delivered` is only emitted by the loop that drained the
/// channel the router enqueued into. A `Sent` without a matching
/// `Delivered` means the message was in flight (enqueued, not yet
/// drained) when the recipient's process ended: detectable loss, never
/// silent. Serialization is stable: internally tagged on `phase` with
/// `snake_case` variant names and RFC 3339 timestamps.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum AgentMessageLifecycle {
    /// The router accepted the message (the send call returned success).
    Sent {
        /// Unique message identifier, shared with the `Delivered` phase.
        message_id: Uuid,
        /// Sender agent id.
        from_id: Uuid,
        /// Sender's registry label at send time (path / `root` / bare
        /// UUID).
        from: String,
        /// Recipient agent id.
        to_id: Uuid,
        /// Recipient's registry label at send time.
        to: String,
        /// How the recipient treats the message.
        kind: MessageKind,
        /// Router-minted per-recipient sequence number.
        seq: u64,
        /// Unescaped sender content, verbatim, for audit.
        content: String,
        /// Wall-clock send time.
        sent_at: DateTime<Utc>,
    },
    /// The recipient's loop injected the message into its conversation.
    Delivered {
        /// Unique message identifier, shared with the `Sent` phase.
        message_id: Uuid,
        /// Sender agent id.
        from_id: Uuid,
        /// Recipient agent id.
        to_id: Uuid,
        /// Router-minted per-recipient sequence number.
        seq: u64,
        /// Wall-clock injection time.
        delivered_at: DateTime<Utc>,
    },
}

impl AgentMessageLifecycle {
    /// Unique id of the message this event concerns.
    #[must_use]
    pub const fn message_id(&self) -> Uuid {
        match self {
            Self::Sent { message_id, .. } | Self::Delivered { message_id, .. } => *message_id,
        }
    }

    /// The session-store `event_type` for this phase
    /// ([`AGENT_MESSAGE_SENT_EVENT_TYPE`] /
    /// [`AGENT_MESSAGE_DELIVERED_EVENT_TYPE`]).
    #[must_use]
    pub const fn session_event_type(&self) -> &'static str {
        match self {
            Self::Sent { .. } => AGENT_MESSAGE_SENT_EVENT_TYPE,
            Self::Delivered { .. } => AGENT_MESSAGE_DELIVERED_EVENT_TYPE,
        }
    }
}

/// Live token estimate for the next provider request.
///
/// This is runtime telemetry, not provider-reported billing usage. The
/// runner emits it after context preflight and any in-flight compaction,
/// immediately before calling the provider, so operator surfaces can show
/// a useful live `↑` count while the stream is still open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentUsageEstimate {
    /// Estimated prompt/input tokens for the provider request about to be
    /// sent.
    pub input_tokens: u64,
}

/// Marker that the tagged agent's in-flight provider stream failed on a
/// retryable error and the loop is about to replay the same request.
///
/// The failed attempt may already have broadcast partial
/// [`ProviderEvent`] deltas; the retry attempt re-streams the turn from
/// the start, so without this marker observers would render the replayed
/// deltas appended to the failed attempt's partials. Emitted immediately
/// **before** the retry attempt's first event: observers discard any
/// partial output accumulated for this agent's current turn (a no-op
/// when the failed attempt emitted nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentStreamRetry {
    /// 1-based index of the provider attempt about to start (`2` is the
    /// first retry).
    pub attempt: u32,
}

/// The payload of an [`AgentEvent`]: a raw provider stream event, a
/// typed subagent lifecycle event, typed usage telemetry, or a typed
/// inter-agent message event.
#[derive(Clone, Debug)]
pub enum AgentEventKind {
    /// A raw provider stream event from the tagged agent's own loop.
    Provider(ProviderEvent),
    /// A typed subagent lifecycle event. The wrapping [`AgentEvent`] is
    /// tagged with the *child's* identity (`agent_id == child_id`).
    Subagent(SubagentLifecycle),
    /// A typed inter-agent message event. The wrapping [`AgentEvent`] is
    /// tagged with the *sender's* identity for
    /// [`AgentMessageLifecycle::Sent`] and the *recipient's* for
    /// [`AgentMessageLifecycle::Delivered`].
    Message(AgentMessageLifecycle),
    /// A live prompt/input estimate for the tagged agent's next provider
    /// request.
    UsageEstimate(AgentUsageEstimate),
    /// The tagged agent's provider stream failed on a retryable error and
    /// is about to be replayed; observers reset any partial output from
    /// the failed attempt.
    StreamRetry(AgentStreamRetry),
}

/// An [`AgentEventKind`] tagged with the identity of the agent it
/// concerns.
///
/// Every agent in the runtime — root, fork, spawn — emits
/// `AgentEvent` values on a shared broadcast channel. The TUI
/// dispatches on `agent_id` to route root events to the main scroll
/// region and child events to the activity panel.
#[derive(Clone, Debug)]
pub struct AgentEvent {
    /// Registry id of the agent the event concerns: the emitting agent
    /// for provider events, the child for subagent lifecycle events.
    pub agent_id: Uuid,
    /// Human-readable role label (e.g. `"root"`, `"fork/gpt-5.5"`).
    pub agent_role: Arc<str>,
    /// The event payload.
    pub event: AgentEventKind,
}

/// Broadcast sender that auto-tags events with agent identity before
/// sending.
///
/// Each agent holds its own `AgentEventSender` wrapping the same
/// underlying `broadcast::Sender<AgentEvent>`. The identity fields are
/// set at construction time and stamped onto every event — callers
/// cannot forget to tag.
#[derive(Clone, Debug)]
pub struct AgentEventSender {
    tx: broadcast::Sender<AgentEvent>,
    agent_id: Uuid,
    agent_role: Arc<str>,
}

impl AgentEventSender {
    /// Create a sender for a specific agent on the given channel.
    #[must_use]
    pub fn new(tx: broadcast::Sender<AgentEvent>, agent_id: Uuid, agent_role: String) -> Self {
        Self {
            tx,
            agent_id,
            agent_role: Arc::from(agent_role),
        }
    }

    /// Tag and broadcast a [`ProviderEvent`].
    ///
    /// The `agent_role` clone is a reference-count bump (`Arc<str>`),
    /// not a heap allocation.
    pub fn send(&self, event: ProviderEvent) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event: AgentEventKind::Provider(event),
        });
    }

    /// Tag and broadcast a typed [`SubagentLifecycle`] event.
    ///
    /// Called on a *child-tagged* sender (see [`Self::for_child`]) so
    /// the wrapping [`AgentEvent::agent_id`] matches the lifecycle
    /// event's `child_id`.
    pub fn send_subagent(&self, event: SubagentLifecycle) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event: AgentEventKind::Subagent(event),
        });
    }

    /// Tag and broadcast a typed [`AgentMessageLifecycle`] event.
    ///
    /// Called on a sender carrying the *sender's* identity for
    /// [`AgentMessageLifecycle::Sent`] and the *recipient's* for
    /// [`AgentMessageLifecycle::Delivered`].
    pub fn send_message(&self, event: AgentMessageLifecycle) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event: AgentEventKind::Message(event),
        });
    }

    /// Tag and broadcast live request-usage telemetry.
    pub fn send_usage_estimate(&self, estimate: AgentUsageEstimate) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event: AgentEventKind::UsageEstimate(estimate),
        });
    }

    /// Tag and broadcast a stream-retry marker. Emitted by the loop's
    /// provider-retry wrapper immediately before a retry attempt begins,
    /// so observers can discard the failed attempt's partial deltas
    /// before the replay streams in.
    pub fn send_stream_retry(&self, retry: AgentStreamRetry) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event: AgentEventKind::StreamRetry(retry),
        });
    }

    /// Create a child sender sharing the same broadcast channel but
    /// carrying a different agent identity.
    #[must_use]
    pub fn for_child(&self, agent_id: Uuid, agent_role: String) -> Self {
        Self {
            tx: self.tx.clone(),
            agent_id,
            agent_role: Arc::from(agent_role),
        }
    }

    /// The id of the agent this sender is tagged with.
    #[must_use]
    pub fn agent_id(&self) -> Uuid {
        self.agent_id
    }
}

/// Shared broadcast sender installed as a
/// [`ToolContext`](crate::tool::context::ToolContext) extension.
///
/// Fork and spawn tools read this extension to create child
/// [`AgentEventSender`] instances. The TUI driver installs it during
/// runtime setup; REPL and print modes install it when they need
/// streaming events.
#[derive(Clone, Debug)]
pub struct SharedAgentEventChannel(pub broadcast::Sender<AgentEvent>);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;

    fn descriptor() -> SubagentDescriptor {
        SubagentDescriptor {
            kind: SubagentKind::Spawn,
            role: "researcher".to_owned(),
            model: "haiku".to_owned(),
            profile: Some("developer".to_owned()),
        }
    }

    #[test]
    fn sender_tags_events_with_agent_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        sender.send(ProviderEvent::TextDelta {
            text: "hello".to_string(),
        });
        let received = rx.try_recv().unwrap();
        assert_eq!(received.agent_id, Uuid::nil());
        assert_eq!(&*received.agent_role, "root");
        assert!(matches!(
            received.event,
            AgentEventKind::Provider(ProviderEvent::TextDelta { text }) if text == "hello"
        ));
    }

    #[test]
    fn for_child_shares_channel_with_different_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let parent = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        let child_id = Uuid::from_u128(42);
        let child = parent.for_child(child_id, "fork/haiku".to_string());

        parent.send(ProviderEvent::TextDelta {
            text: "from parent".to_string(),
        });
        child.send(ProviderEvent::TextDelta {
            text: "from child".to_string(),
        });

        let first = rx.try_recv().unwrap();
        assert_eq!(first.agent_id, Uuid::nil());
        assert_eq!(&*first.agent_role, "root");

        let second = rx.try_recv().unwrap();
        assert_eq!(second.agent_id, child_id);
        assert_eq!(&*second.agent_role, "fork/haiku");
    }

    #[test]
    fn send_succeeds_with_no_receivers() {
        let (tx, rx) = broadcast::channel::<AgentEvent>(16);
        drop(rx);
        let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        sender.send(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        });
    }

    #[test]
    fn send_usage_estimate_tags_agent_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        sender.send_usage_estimate(AgentUsageEstimate {
            input_tokens: 12_345,
        });

        let received = rx.try_recv().unwrap();
        assert_eq!(received.agent_id, Uuid::nil());
        assert_eq!(&*received.agent_role, "root");
        assert!(matches!(
            received.event,
            AgentEventKind::UsageEstimate(AgentUsageEstimate {
                input_tokens: 12_345
            })
        ));
    }

    #[test]
    fn send_subagent_tags_with_child_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let parent_id = Uuid::from_u128(1);
        let child_id = Uuid::from_u128(2);
        let root = AgentEventSender::new(tx, parent_id, "root".to_string());
        let child_sender = root.for_child(child_id, "spawn/haiku".to_string());

        child_sender.send_subagent(SubagentLifecycle::Started {
            parent_id,
            child_id,
            descriptor: descriptor(),
            started_at: Utc::now(),
        });

        let received = rx.try_recv().unwrap();
        assert_eq!(received.agent_id, child_id);
        assert_eq!(&*received.agent_role, "spawn/haiku");
        match received.event {
            AgentEventKind::Subagent(lifecycle) => {
                assert_eq!(lifecycle.child_id(), child_id);
                assert_eq!(lifecycle.parent_id(), parent_id);
                assert_eq!(lifecycle.descriptor().kind, SubagentKind::Spawn);
            }
            AgentEventKind::Provider(_)
            | AgentEventKind::Message(_)
            | AgentEventKind::UsageEstimate(_)
            | AgentEventKind::StreamRetry(_) => {
                panic!("expected subagent lifecycle event")
            }
        }
    }

    /// The serialized form is the stable contract embedders (meridian)
    /// match on — `snake_case` `phase` / `kind` tags, full identity on
    /// every event.
    #[test]
    fn started_serde_shape_is_stable() {
        let started_at = DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let event = SubagentLifecycle::Started {
            parent_id: Uuid::from_u128(1),
            child_id: Uuid::from_u128(2),
            descriptor: descriptor(),
            started_at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "phase": "started",
                "parent_id": "00000000-0000-0000-0000-000000000001",
                "child_id": "00000000-0000-0000-0000-000000000002",
                "descriptor": {
                    "kind": "spawn",
                    "role": "researcher",
                    "model": "haiku",
                    "profile": "developer",
                },
                "started_at": "2026-06-12T10:00:00Z",
            }),
        );
        let parsed: SubagentLifecycle = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.child_id(), Uuid::from_u128(2));
        assert_eq!(parsed.session_event_type(), SUBAGENT_STARTED_EVENT_TYPE);
    }

    #[test]
    fn completed_serde_shape_is_stable() {
        let started_at = DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let completed_at = DateTime::parse_from_rfc3339("2026-06-12T10:00:05Z")
            .unwrap()
            .with_timezone(&Utc);
        let event = SubagentLifecycle::Completed {
            parent_id: Uuid::from_u128(1),
            child_id: Uuid::from_u128(2),
            descriptor: SubagentDescriptor {
                kind: SubagentKind::Fork,
                role: "fork".to_owned(),
                model: "gpt-5.5".to_owned(),
                profile: None,
            },
            started_at,
            completed_at,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
            subtree_usage: Usage {
                input_tokens: 17,
                output_tokens: 8,
                ..Usage::default()
            },
            succeeded: false,
            error: Some("fork reached its max-iterations cap".to_owned()),
            stop: Some(AgentStopReason::MaxIterationsReached),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["phase"], "completed");
        assert_eq!(value["descriptor"]["kind"], "fork");
        assert_eq!(value["usage"]["input_tokens"], 10);
        assert_eq!(value["usage"]["output_tokens"], 5);
        assert_eq!(value["subtree_usage"]["input_tokens"], 17);
        assert_eq!(value["subtree_usage"]["output_tokens"], 8);
        assert_eq!(value["succeeded"], false);
        assert_eq!(value["stop"]["reason"], "max_iterations_reached");
        assert_eq!(value["completed_at"], "2026-06-12T10:00:05Z");

        let parsed: SubagentLifecycle = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.session_event_type(), SUBAGENT_COMPLETED_EVENT_TYPE);
        match parsed {
            SubagentLifecycle::Completed {
                succeeded,
                stop,
                usage,
                subtree_usage,
                ..
            } => {
                assert!(!succeeded);
                assert_eq!(stop, Some(AgentStopReason::MaxIterationsReached));
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(subtree_usage.input_tokens, 17);
            }
            SubagentLifecycle::Started { .. } => panic!("expected completed phase"),
        }
    }

    /// DECISION R4 (W3.6): `subtree_usage` is a **required** field on
    /// the `completed` phase — a pre-W3.6 serialized event without it
    /// must fail to deserialize loudly instead of silently reading
    /// fabricated zeros. The break is pre-announced in
    /// MERIDIAN-HANDOFF §8.2; consumers update their match alongside
    /// the pin bump.
    #[test]
    fn completed_without_subtree_usage_is_rejected() {
        let legacy = serde_json::json!({
            "phase": "completed",
            "parent_id": "00000000-0000-0000-0000-000000000001",
            "child_id": "00000000-0000-0000-0000-000000000002",
            "descriptor": {
                "kind": "spawn",
                "role": "researcher",
                "model": "haiku",
                "profile": null,
            },
            "started_at": "2026-06-12T10:00:00Z",
            "completed_at": "2026-06-12T10:00:05Z",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "cost_usd": null,
            },
            "succeeded": true,
            "error": null,
            "stop": null,
        });
        let result: Result<SubagentLifecycle, _> = serde_json::from_value(legacy);
        let err = result.expect_err("pre-W3.6 completed events must be rejected, not zero-filled");
        assert!(
            err.to_string().contains("subtree_usage"),
            "the error must name the missing field: {err}",
        );
    }

    /// The serialized message-Sent shape is the stable contract audit
    /// consumers match on — `snake_case` `phase` tag, full identity and
    /// attribution, verbatim content, RFC 3339 timestamp.
    #[test]
    fn message_sent_serde_shape_is_stable() {
        let sent_at = DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let event = AgentMessageLifecycle::Sent {
            message_id: Uuid::from_u128(9),
            from_id: Uuid::from_u128(1),
            from: "/smoke/child".to_owned(),
            to_id: Uuid::from_u128(2),
            to: "root".to_owned(),
            kind: MessageKind::Update,
            seq: 42,
            content: "status: <done> & \"verified\"".to_owned(),
            sent_at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "phase": "sent",
                "message_id": "00000000-0000-0000-0000-000000000009",
                "from_id": "00000000-0000-0000-0000-000000000001",
                "from": "/smoke/child",
                "to_id": "00000000-0000-0000-0000-000000000002",
                "to": "root",
                "kind": "update",
                "seq": 42,
                "content": "status: <done> & \"verified\"",
                "sent_at": "2026-06-12T10:00:00Z",
            }),
        );
        let parsed: AgentMessageLifecycle = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.message_id(), Uuid::from_u128(9));
        assert_eq!(parsed.session_event_type(), AGENT_MESSAGE_SENT_EVENT_TYPE);
    }

    #[test]
    fn message_delivered_serde_shape_is_stable() {
        let delivered_at = DateTime::parse_from_rfc3339("2026-06-12T10:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let event = AgentMessageLifecycle::Delivered {
            message_id: Uuid::from_u128(9),
            from_id: Uuid::from_u128(1),
            to_id: Uuid::from_u128(2),
            seq: 42,
            delivered_at,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "phase": "delivered",
                "message_id": "00000000-0000-0000-0000-000000000009",
                "from_id": "00000000-0000-0000-0000-000000000001",
                "to_id": "00000000-0000-0000-0000-000000000002",
                "seq": 42,
                "delivered_at": "2026-06-12T10:00:01Z",
            }),
        );
        let parsed: AgentMessageLifecycle = serde_json::from_value(value).unwrap();
        assert_eq!(
            parsed.session_event_type(),
            AGENT_MESSAGE_DELIVERED_EVENT_TYPE,
        );
    }

    #[test]
    fn send_message_tags_with_sender_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let sender_id = Uuid::from_u128(7);
        let sender = AgentEventSender::new(tx, sender_id, "/parent/worker".to_owned());
        sender.send_message(AgentMessageLifecycle::Delivered {
            message_id: Uuid::from_u128(9),
            from_id: Uuid::from_u128(1),
            to_id: sender_id,
            seq: 1,
            delivered_at: Utc::now(),
        });
        let received = rx.try_recv().unwrap();
        assert_eq!(received.agent_id, sender_id);
        match received.event {
            AgentEventKind::Message(lifecycle) => {
                assert_eq!(lifecycle.message_id(), Uuid::from_u128(9));
            }
            other => panic!("expected message event, got {other:?}"),
        }
    }
}
