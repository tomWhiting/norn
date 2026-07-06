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
//! wrapper task once the run reaches a terminal outcome.
//!
//! Store-append failures are **typed at the source** (session-fidelity
//! inventory, Gap 10): every emitter returns the append error. What the
//! caller does with it depends on where it stands: `emit_started` runs
//! inside the spawning tool call and propagates (the launch is aborted
//! before the child exists); `emit_completed` runs in the child's
//! detached wrapper after the run already finished, where the child's
//! result — the primary content — must still be delivered to the
//! parent, so the wrapper logs the failure at error level instead of
//! aborting delivery (the result's own injection into the parent store
//! rides the primary write-through contract and fails the parent's run
//! typed under a persistent sink fault).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agent::output::AgentStopReason;
use crate::error::SessionError;
use crate::provider::agent_event::{
    AgentEventSender, AgentMessageLifecycle, SubagentDescriptor, SubagentLifecycle,
};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Append an [`AgentMessageLifecycle`] audit record to `store` as a
/// [`SessionEvent::Custom`] (`agent_message.sent` /
/// `agent_message.delivered`).
///
/// Failures are typed, never swallowed (session-fidelity inventory,
/// Gap 10): the audit event joins the primary write-through contract.
/// Callers on a tool-call path propagate the error to the model —
/// with wording that states whether the underlying message was already
/// delivered, so a typed audit failure never provokes a duplicate send.
///
/// # Errors
///
/// [`SessionError::StorageError`] when the lifecycle event cannot be
/// serialized, and the store's own append error when the write-through
/// persist fails.
pub(crate) fn append_message_audit(
    store: &EventStore,
    event: &AgentMessageLifecycle,
) -> Result<(), SessionError> {
    let event_type = event.session_event_type();
    let data = serde_json::to_value(event).map_err(|e| SessionError::StorageError {
        reason: format!(
            "failed to serialize {event_type} audit event for message {}: {e}",
            event.message_id(),
        ),
    })?;
    store.append(SessionEvent::Custom {
        base: EventBase::new(store.last_event_id()),
        event_type: event_type.to_owned(),
        data,
    })?;
    Ok(())
}

/// Terminal outcome projection handed to
/// [`LifecycleEmitter::emit_completed`].
pub(crate) struct SubagentCompletion {
    /// Accumulated token usage across every provider call the child made.
    ///
    /// Honest limitation: when the child's run ended in a hard
    /// [`NornError`](crate::error::NornError) (the runner's `Err` path,
    /// which carries no usage) — or when the child's wrapper task itself
    /// panicked — this is [`Usage::default`] (all zeros), meaning
    /// "unknown", not "no tokens consumed". Every early-stop
    /// [`AgentStepResult`](crate::agent_loop::runner::AgentStepResult) arm
    /// (timeout, cancellation, truncation, schema exhaustion, max
    /// iterations) does carry real accumulated usage.
    pub(crate) usage: Usage,
    /// Aggregated usage of the child's entire delegation subtree (W3.6):
    /// [`Self::usage`] plus the summed `subtree_usage` of every result
    /// the child's own loop delivered. Computed by the wrapper as
    /// `usage + children_usage`; on the panic/hard-error paths the own
    /// component is unknown-zeros while delivered descendant subtrees
    /// are still folded in (read from the shared
    /// [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
    /// accumulator, which survives the unwound task).
    pub(crate) subtree_usage: Usage,
    /// Whether the child's run completed successfully.
    pub(crate) succeeded: bool,
    /// Explanatory error when `succeeded` is `false`.
    pub(crate) error: Option<String>,
    /// Typed stop reason when the child stopped early; `None` on
    /// success or hard error.
    pub(crate) stop: Option<AgentStopReason>,
}

/// Per-child emitter for the typed [`SubagentLifecycle`] events.
///
/// Holds the child-tagged event sender (when a broadcast channel is
/// installed), the parent's event store, and the child's identity /
/// provenance, so the spawn and fork wrappers emit identical, complete
/// events without recomputing any of it.
pub(crate) struct LifecycleEmitter {
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
    pub(crate) fn new(
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
    ///
    /// # Errors
    ///
    /// The parent-store audit append error. Callers run inside the
    /// spawning tool call, before anything irreversible happened, so
    /// they propagate it and abort the launch — the durable log never
    /// silently misses a child that did run.
    pub(crate) fn emit_started(&self) -> Result<(), SessionError> {
        self.emit(SubagentLifecycle::Started {
            parent_id: self.parent_id,
            child_id: self.child_id,
            descriptor: self.descriptor.clone(),
            started_at: self.started_at,
        })
    }

    /// Emit [`SubagentLifecycle::Completed`] on both carriers. Called
    /// from the child's wrapper task once the run reaches a terminal
    /// outcome — unconditionally, including when a subagent-stop hook
    /// suppressed the registry's terminal transition (the run itself
    /// did finish; the hook only blocks the registry state change).
    ///
    /// # Errors
    ///
    /// The parent-store audit append error. The wrapper handles it by
    /// logging at error level and continuing with result delivery: the
    /// child's result is the primary content and must still reach the
    /// parent (whose own injection of it propagates persist failures
    /// through the primary write-through contract).
    pub(crate) fn emit_completed(
        &self,
        completion: SubagentCompletion,
    ) -> Result<(), SessionError> {
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
        })
    }

    /// Broadcast on the live channel and append the audit record to the
    /// parent's store. The live broadcast happens regardless of the
    /// store outcome — a subscriber must not lose the event because the
    /// durable side failed — and the store error is returned typed.
    ///
    /// # Errors
    ///
    /// [`SessionError::StorageError`] when the lifecycle event cannot be
    /// serialized, and the parent store's own append error when the
    /// write-through persist fails.
    fn emit(&self, event: SubagentLifecycle) -> Result<(), SessionError> {
        let event_type = event.session_event_type();
        let serialized = serde_json::to_value(&event);
        if let Some(sender) = self.sender.as_ref() {
            sender.send_subagent(event);
        }
        let data = serialized.map_err(|e| SessionError::StorageError {
            reason: format!(
                "failed to serialize {event_type} lifecycle event for child {}: {e}",
                self.child_id,
            ),
        })?;
        self.parent_store.append(SessionEvent::Custom {
            base: EventBase::new(self.parent_store.last_event_id()),
            event_type: event_type.to_owned(),
            data,
        })?;
        Ok(())
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

        emitter.emit_started().expect("started audit appends");
        emitter
            .emit_completed(SubagentCompletion {
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
            })
            .expect("completed audit appends");

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
        emitter.emit_started().expect("started audit appends");
        emitter
            .emit_completed(SubagentCompletion {
                usage: Usage::default(),
                subtree_usage: Usage::default(),
                succeeded: false,
                error: Some("provider exploded".to_owned()),
                stop: None,
            })
            .expect("completed audit appends");
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

    /// A sink that fails every persist, standing in for a faulted
    /// session store (session-fidelity Gap 10).
    struct FailingSink;
    impl crate::session::store::PersistenceSink for FailingSink {
        fn persist(
            &mut self,
            _event: &SessionEvent,
        ) -> Result<(), crate::session::persistence::SessionPersistError> {
            Err(crate::session::persistence::SessionPersistError::Io(
                std::io::Error::other("disk full"),
            ))
        }
    }

    /// Gap 10: a failing sink on the parent store surfaces the lifecycle
    /// audit appends as typed errors — never a silent audit hole — while
    /// the live broadcast still reaches subscribers (a live listener must
    /// not lose the event because the durable side failed).
    #[test]
    fn emit_surfaces_sink_failure_typed_and_still_broadcasts() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let store = Arc::new(EventStore::with_sink(Box::new(FailingSink)));
        let root = AgentEventSender::new(tx, Uuid::from_u128(1), "root".to_owned());
        let child_sender = root.for_child(Uuid::from_u128(2), "spawn/haiku".to_owned());
        let (emitter, _, child_id) = emitter(Some(child_sender), Arc::clone(&store));

        let err = emitter
            .emit_started()
            .expect_err("the sink failure must surface typed");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert!(store.is_empty(), "the failed audit never reaches memory");

        let live = rx.try_recv().expect("live broadcast still delivered");
        assert_eq!(live.agent_id, child_id);

        let err = emitter
            .emit_completed(SubagentCompletion {
                usage: Usage::default(),
                subtree_usage: Usage::default(),
                succeeded: true,
                error: None,
                stop: None,
            })
            .expect_err("completed shares the same typed contract");
        assert!(matches!(err, SessionError::StorageError { .. }));
    }

    /// Gap 10: the inter-agent message audit append surfaces sink
    /// failures typed to its caller instead of logging and continuing.
    #[test]
    fn append_message_audit_surfaces_sink_failure_typed() {
        let store = EventStore::with_sink(Box::new(FailingSink));
        let sent = AgentMessageLifecycle::Sent {
            message_id: Uuid::from_u128(9),
            from_id: Uuid::from_u128(1),
            from: "root".to_owned(),
            to_id: Uuid::from_u128(2),
            to: "worker".to_owned(),
            kind: crate::r#loop::inbound::MessageKind::Update,
            seq: 1,
            content: "status".to_owned(),
            sent_at: Utc::now(),
        };

        let err =
            append_message_audit(&store, &sent).expect_err("the sink failure must surface typed");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert!(store.is_empty(), "the failed audit never reaches memory");
    }
}
