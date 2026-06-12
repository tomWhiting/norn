//! Typed subagent lifecycle emission shared by spawn and fork.
//!
//! [`LifecycleEmitter`] is built once per child by
//! [`SpawnAgentTool`](super::spawn::SpawnAgentTool) /
//! [`ForkTool`](super::fork_tool::ForkTool) and emits the typed
//! [`SubagentLifecycle`] events on both carriers:
//!
//! - **Live**: a child-tagged [`AgentEvent`](crate::provider::agent_event::AgentEvent)
//!   on the shared broadcast channel (when the runtime installed
//!   [`SharedAgentEventChannel`](crate::provider::agent_event::SharedAgentEventChannel)).
//! - **Replay/audit**: a [`SessionEvent::Custom`] appended to the
//!   *parent's* event store with `event_type`
//!   [`SUBAGENT_STARTED_EVENT_TYPE`](crate::provider::agent_event::SUBAGENT_STARTED_EVENT_TYPE)
//!   / [`SUBAGENT_COMPLETED_EVENT_TYPE`](crate::provider::agent_event::SUBAGENT_COMPLETED_EVENT_TYPE)
//!   and the serialized lifecycle event as `data` — following the
//!   `loop.*` Custom-event convention used by the agent loop.
//!
//! [`LifecycleEmitter::emit_started`] is called by the tool before the
//! child task launches, so the `Started` event always precedes the
//! child's own provider events on the channel.
//! [`LifecycleEmitter::emit_completed`] is called from the child's
//! wrapper task once the run reaches a terminal outcome. Store appends
//! are best-effort (logged on failure) — the audit record must never
//! abort result delivery.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agent::output::AgentStopReason;
use crate::provider::agent_event::{
    AgentEventSender, AgentMessageLifecycle, SubagentDescriptor, SubagentLifecycle,
};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Append an [`AgentMessageLifecycle`] audit record to `store` as a
/// [`SessionEvent::Custom`] (`agent_message.sent` /
/// `agent_message.delivered`), best-effort: failures are logged, never
/// propagated — the audit record must never abort message delivery,
/// matching [`LifecycleEmitter::emit`]'s store-append contract.
pub(crate) fn append_message_audit(store: &EventStore, event: &AgentMessageLifecycle) {
    let event_type = event.session_event_type();
    match serde_json::to_value(event) {
        Ok(data) => {
            let session_event = SessionEvent::Custom {
                base: EventBase::new(store.last_event_id()),
                event_type: event_type.to_owned(),
                data,
            };
            if let Err(e) = store.append(session_event) {
                tracing::warn!(
                    message_id = %event.message_id(),
                    event_type,
                    error = %e,
                    "agent message audit: failed to append event to store",
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                message_id = %event.message_id(),
                event_type,
                error = %e,
                "agent message audit: failed to serialize lifecycle event",
            );
        }
    }
}

/// Terminal outcome projection handed to
/// [`LifecycleEmitter::emit_completed`].
pub(super) struct SubagentCompletion {
    /// Accumulated token usage across every provider call the child made.
    ///
    /// Honest limitation: when the child's run ended in a hard
    /// [`NornError`](crate::error::NornError) (the runner's `Err` path,
    /// which carries no usage) — or when the child's wrapper task itself
    /// panicked — this is [`Usage::default`] (all zeros), meaning
    /// "unknown", not "no tokens consumed". Every early-stop
    /// [`AgentStepResult`](crate::r#loop::runner::AgentStepResult) arm
    /// (timeout, cancellation, truncation, schema exhaustion, max
    /// iterations) does carry real accumulated usage.
    pub(super) usage: Usage,
    /// Aggregated usage of the child's entire delegation subtree (W3.6):
    /// [`Self::usage`] plus the summed `subtree_usage` of every result
    /// the child's own loop delivered. Computed by the wrapper as
    /// `usage + children_usage`; on the panic/hard-error paths the own
    /// component is unknown-zeros while delivered descendant subtrees
    /// are still folded in (read from the shared
    /// [`ChildrenUsage`](crate::r#loop::children_usage::ChildrenUsage)
    /// accumulator, which survives the unwound task).
    pub(super) subtree_usage: Usage,
    /// Whether the child's run completed successfully.
    pub(super) succeeded: bool,
    /// Explanatory error when `succeeded` is `false`.
    pub(super) error: Option<String>,
    /// Typed stop reason when the child stopped early; `None` on
    /// success or hard error.
    pub(super) stop: Option<AgentStopReason>,
}

/// Per-child emitter for the typed [`SubagentLifecycle`] events.
///
/// Holds the child-tagged event sender (when a broadcast channel is
/// installed), the parent's event store, and the child's identity /
/// provenance, so the spawn and fork wrappers emit identical, complete
/// events without recomputing any of it.
pub(super) struct LifecycleEmitter {
    /// Child-tagged sender on the shared broadcast channel. `None` when
    /// the runtime installed no channel — session-store emission still
    /// happens.
    sender: Option<AgentEventSender>,
    /// The parent's session event store, carrying the audit record.
    parent_store: Arc<EventStore>,
    parent_id: Uuid,
    child_id: Uuid,
    descriptor: SubagentDescriptor,
    started_at: DateTime<Utc>,
}

impl LifecycleEmitter {
    /// Build an emitter for one child. `started_at` is captured by the
    /// caller immediately before launch and shared by both phases.
    pub(super) fn new(
        sender: Option<AgentEventSender>,
        parent_store: Arc<EventStore>,
        parent_id: Uuid,
        child_id: Uuid,
        descriptor: SubagentDescriptor,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            sender,
            parent_store,
            parent_id,
            child_id,
            descriptor,
            started_at,
        }
    }

    /// Emit [`SubagentLifecycle::Started`] on both carriers. Called by
    /// the tool before the child task launches.
    pub(super) fn emit_started(&self) {
        self.emit(SubagentLifecycle::Started {
            parent_id: self.parent_id,
            child_id: self.child_id,
            descriptor: self.descriptor.clone(),
            started_at: self.started_at,
        });
    }

    /// Emit [`SubagentLifecycle::Completed`] on both carriers. Called
    /// from the child's wrapper task once the run reaches a terminal
    /// outcome — unconditionally, including when a subagent-stop hook
    /// suppressed the registry's terminal transition (the run itself
    /// did finish; the hook only blocks the registry state change).
    pub(super) fn emit_completed(&self, completion: SubagentCompletion) {
        self.emit(SubagentLifecycle::Completed {
            parent_id: self.parent_id,
            child_id: self.child_id,
            descriptor: self.descriptor.clone(),
            started_at: self.started_at,
            completed_at: Utc::now(),
            usage: completion.usage,
            subtree_usage: completion.subtree_usage,
            succeeded: completion.succeeded,
            error: completion.error,
            stop: completion.stop,
        });
    }

    /// Broadcast on the live channel and append the audit record to the
    /// parent's store.
    fn emit(&self, event: SubagentLifecycle) {
        let event_type = event.session_event_type();
        let serialized = serde_json::to_value(&event);
        if let Some(sender) = self.sender.as_ref() {
            sender.send_subagent(event);
        }
        match serialized {
            Ok(data) => {
                let session_event = SessionEvent::Custom {
                    base: EventBase::new(self.parent_store.last_event_id()),
                    event_type: event_type.to_owned(),
                    data,
                };
                if let Err(e) = self.parent_store.append(session_event) {
                    tracing::warn!(
                        child_id = %self.child_id,
                        event_type,
                        error = %e,
                        "subagent lifecycle: failed to append audit event to parent store",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    child_id = %self.child_id,
                    event_type,
                    error = %e,
                    "subagent lifecycle: failed to serialize lifecycle event",
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use tokio::sync::broadcast;

    use super::*;
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
        SubagentKind,
    };

    fn emitter(
        sender: Option<AgentEventSender>,
        parent_store: Arc<EventStore>,
    ) -> (LifecycleEmitter, Uuid, Uuid) {
        let parent_id = Uuid::from_u128(1);
        let child_id = Uuid::from_u128(2);
        let descriptor = SubagentDescriptor {
            kind: SubagentKind::Spawn,
            role: "worker".to_owned(),
            model: "haiku".to_owned(),
            profile: None,
        };
        let emitter = LifecycleEmitter::new(
            sender,
            parent_store,
            parent_id,
            child_id,
            descriptor,
            Utc::now(),
        );
        (emitter, parent_id, child_id)
    }

    #[test]
    fn emits_on_channel_and_parent_store() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let store = Arc::new(EventStore::new());
        let root = AgentEventSender::new(tx, Uuid::from_u128(1), "root".to_owned());
        let child_sender = root.for_child(Uuid::from_u128(2), "spawn/haiku".to_owned());
        let (emitter, parent_id, child_id) = emitter(Some(child_sender), Arc::clone(&store));

        emitter.emit_started();
        emitter.emit_completed(SubagentCompletion {
            usage: Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
            subtree_usage: Usage {
                input_tokens: 12,
                output_tokens: 5,
                ..Usage::default()
            },
            succeeded: true,
            error: None,
            stop: None,
        });

        // Live carrier: child-tagged Started then Completed.
        let first = rx.try_recv().expect("started on channel");
        assert_eq!(first.agent_id, child_id);
        match first.event {
            AgentEventKind::Subagent(SubagentLifecycle::Started {
                parent_id: p,
                child_id: c,
                ..
            }) => {
                assert_eq!(p, parent_id);
                assert_eq!(c, child_id);
            }
            other => panic!("expected started lifecycle, got {other:?}"),
        }
        let second = rx.try_recv().expect("completed on channel");
        match second.event {
            AgentEventKind::Subagent(SubagentLifecycle::Completed {
                usage,
                subtree_usage,
                succeeded,
                started_at,
                completed_at,
                ..
            }) => {
                assert!(succeeded);
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(
                    subtree_usage.input_tokens, 12,
                    "the wrapper-computed subtree total rides on the lifecycle event",
                );
                assert!(completed_at >= started_at, "timestamps must be ordered");
            }
            other => panic!("expected completed lifecycle, got {other:?}"),
        }

        // Audit carrier: two Custom events on the parent store.
        let events = store.events();
        assert_eq!(events.len(), 2);
        match &events[0] {
            SessionEvent::Custom {
                event_type, data, ..
            } => {
                assert_eq!(event_type, SUBAGENT_STARTED_EVENT_TYPE);
                assert_eq!(data["phase"], "started");
                assert_eq!(data["child_id"], child_id.to_string());
            }
            other => panic!("expected Custom started event, got {other:?}"),
        }
        match &events[1] {
            SessionEvent::Custom {
                event_type, data, ..
            } => {
                assert_eq!(event_type, SUBAGENT_COMPLETED_EVENT_TYPE);
                assert_eq!(data["phase"], "completed");
                assert_eq!(data["succeeded"], true);
                assert_eq!(data["usage"]["input_tokens"], 7);
                assert_eq!(data["subtree_usage"]["input_tokens"], 12);
            }
            other => panic!("expected Custom completed event, got {other:?}"),
        }
    }

    #[test]
    fn emits_to_store_when_no_channel_installed() {
        let store = Arc::new(EventStore::new());
        let (emitter, _, _) = emitter(None, Arc::clone(&store));
        emitter.emit_started();
        emitter.emit_completed(SubagentCompletion {
            usage: Usage::default(),
            subtree_usage: Usage::default(),
            succeeded: false,
            error: Some("provider exploded".to_owned()),
            stop: None,
        });
        let events = store.events();
        assert_eq!(events.len(), 2, "audit record must not depend on channel");
        match &events[1] {
            SessionEvent::Custom { data, .. } => {
                assert_eq!(data["succeeded"], false);
                assert_eq!(data["error"], "provider exploded");
            }
            other => panic!("expected Custom completed event, got {other:?}"),
        }
    }
}
