//! The concrete [`ProcessNotifier`] that delivers a background process's
//! completion notice and its watches' match/error alerts (NP-002) over the
//! durable injected-message path.
//!
//! This lives in `crate::agent` — not in `crate::process` — precisely so the
//! process module stays agent-agnostic (it depends only on the notifier trait).
//! One object serves both notice kinds because they share one durable delivery
//! algorithm — the same path the schedule executor uses
//! ([`crate::schedule::executor`]) and cron's wake tool contract — differing
//! only in payload and reserved harness sender identity
//! (`norn:process-manager` for completions, `norn:watch` for watch alerts):
//!
//! - **(a)** an idle spawned child (registry status
//!   [`AgentStatus::Idle`](crate::agent::registry::AgentStatus)) is queued
//!   durably then woken through the [`AgentWakeRegistry`] — a parked child does
//!   not drain its inbound channel;
//! - **(b)** otherwise a live [`InboundSender`] `try_send` delivers a `Steer`,
//!   waking a lingering agent at its would-stop boundary via the steer wake set;
//! - **(c)** with no live channel (or a full/closed one) the notice is queued as
//!   a [`PendingAgentMessage`] with its `agent_message.queued` audit, drained by
//!   the next step's pending flush.
//!
//! A total failure — closed channel *and* failed durable queue — is logged at
//! error level with the process id. The completion notice is never written into
//! the tool envelope (message-injection ruling, INTERNAL-AGENTS §2).

use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::pending_messages::{PendingAgentMessage, PendingAgentMessages};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::r#loop::inbound::{ChannelMessage, InboundSender, InboundTrySendError, MessageKind};
use crate::process::{ProcessCompletion, ProcessNotifier, WatchAlert, WatchAlertKind};
use crate::session::store::EventStore;
use crate::tools::agent::{AgentWakeRegistry, WakeRequestOutcome};

/// Reserved sender identity stamped on every background-process completion
/// notice. The sender id is [`Uuid::nil`].
pub const PROCESS_MANAGER_SENDER_LABEL: &str = "norn:process-manager";

/// Reserved sender identity stamped on every watch match/error alert (NP-002).
/// The sender id is [`Uuid::nil`].
pub const WATCH_SENDER_LABEL: &str = "norn:watch";

/// Delivers a process's completion notice and its watches' alerts to their
/// owning agent over the durable injected-message path. Every handle is a cheap
/// `Arc`/clone captured at assembly.
pub struct ProcessMessageDelivery {
    /// The owning agent — recipient of every completion notice.
    pub agent_id: Uuid,
    /// The agent's live inbound sender, when it has one.
    pub inbound: Option<InboundSender>,
    /// The session-tree-shared durable pending store, keyed by recipient.
    /// `None` only on assembly shapes carrying no durable store — a notice that
    /// cannot be sent live then fails loudly instead of queueing into a store
    /// nothing reads.
    pub pending: Option<Arc<PendingAgentMessages>>,
    /// The owning agent's event store (queue-audit target).
    pub event_store: Arc<EventStore>,
    /// The agent registry, used to detect an idle spawned child.
    pub registry: Option<Arc<RwLock<AgentRegistry>>>,
    /// The wake registry, used to resume an idle spawned child.
    pub wake_registry: Option<Arc<AgentWakeRegistry>>,
}

impl ProcessMessageDelivery {
    /// Build the injected `ChannelMessage` for a watch alert (NP-002 R3/R4): a
    /// `Steer`, unsequenced, from the `norn:watch` identity with a nil sender
    /// id, carrying the structured match/error payload — a shape a future
    /// cheap-model watcher agent can consume unchanged.
    fn build_watch_message(&self, alert: &WatchAlert) -> ChannelMessage {
        let spool_range = serde_json::json!({
            "start": alert.spool_start,
            "end": alert.spool_end,
        });
        let content = match &alert.kind {
            WatchAlertKind::Match {
                excerpt,
                matched_at,
            } => serde_json::json!({
                "type": "watch_match",
                "watch_id": alert.watch_id,
                "process_id": alert.process_id,
                "brief": alert.brief,
                "excerpt": excerpt,
                "spool_range": spool_range,
                "matched_at": matched_at,
                "hint": format!(
                    "Watch {} on process {} matched. The excerpt is the filter's output for the \
                     examined region; fetch fuller output with the process tool (op=output, \
                     id={}), or stop watching (op=unwatch, watch_id={}).",
                    alert.watch_id, alert.process_id, alert.process_id, alert.watch_id,
                ),
            }),
            WatchAlertKind::Error { error } => serde_json::json!({
                "type": "watch_error",
                "watch_id": alert.watch_id,
                "process_id": alert.process_id,
                "brief": alert.brief,
                "error": error,
                "spool_range": spool_range,
                "hint": format!(
                    "Watch {} on process {} could not run its filter. The watch is still \
                     attached and its cursor advanced past the failed region — fix the filter, \
                     or stop watching (op=unwatch, watch_id={}).",
                    alert.watch_id, alert.process_id, alert.watch_id,
                ),
            }),
        }
        .to_string();
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::nil(),
            from: WATCH_SENDER_LABEL.to_string(),
            role: None,
            to_id: self.agent_id,
            content,
            kind: MessageKind::Steer,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    /// Build the injected `ChannelMessage` for a completion: a `Steer`,
    /// unsequenced, from the `norn:process-manager` identity with a nil sender
    /// id, carrying the structured completion payload.
    fn build_message(&self, completion: &ProcessCompletion) -> ChannelMessage {
        let disposition = if completion.killed {
            "killed".to_owned()
        } else {
            match completion.exit_code {
                Some(code) => format!("exit code {code}"),
                None => "terminated".to_owned(),
            }
        };
        let content = serde_json::json!({
            "process_id": completion.process_label,
            "command": completion.command,
            "exit_code": completion.exit_code,
            "killed": completion.killed,
            "started_at": completion.started_at,
            "exited_at": completion.exited_at,
            "spool_path": completion.spool_path,
            "hint": format!(
                "Background process {} finished ({disposition}). Fetch its unread output with \
                 the process tool (op=output, id={}).",
                completion.process_label, completion.process_label,
            ),
        })
        .to_string();
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::nil(),
            from: PROCESS_MANAGER_SENDER_LABEL.to_string(),
            role: None,
            to_id: self.agent_id,
            content,
            kind: MessageKind::Steer,
            seq: None,
            timestamp: completion.exited_at,
        }
    }

    /// Whether the owning agent is a registry-Idle spawned child.
    fn is_idle_child(&self) -> bool {
        self.registry.as_ref().is_some_and(|registry| {
            registry
                .read()
                .get(self.agent_id)
                .is_some_and(|entry| entry.status == AgentStatus::Idle)
        })
    }

    /// Queue the message into the durable pending store with its
    /// `agent_message.queued` audit.
    fn queue_durable(&self, message: &ChannelMessage) -> Result<(), crate::error::SessionError> {
        let Some(pending) = self.pending.as_ref() else {
            return Err(crate::error::SessionError::StorageError {
                reason: format!(
                    "no durable pending-message store is wired for agent {}; the \
                     background-process completion notice has no consumer",
                    self.agent_id
                ),
            });
        };
        let mut record =
            PendingAgentMessage::new(message.clone(), self.agent_id.to_string(), Utc::now());
        pending.persist_for_registered_store(&self.event_store, &mut record)?;
        Ok(())
    }

    /// Deliver a built message over the durable path. Returns `Err` only when
    /// the message could neither be sent live nor durably queued.
    fn deliver_message(
        &self,
        message: &ChannelMessage,
        process_label: &str,
    ) -> Result<(), crate::error::SessionError> {
        // (a) An idle spawned child does not drain its inbound channel while
        // parked: queue durably and request a wake.
        if self.is_idle_child() {
            self.queue_durable(message)?;
            if let Some(wake) = &self.wake_registry {
                match wake.request_wake(self.agent_id) {
                    WakeRequestOutcome::Queued
                    | WakeRequestOutcome::AlreadyQueued
                    | WakeRequestOutcome::AlreadyActive(_) => {
                        tracing::trace!(
                            agent_id = %self.agent_id,
                            process = process_label,
                            "process completion durably queued for an idle child; wake requested",
                        );
                    }
                    WakeRequestOutcome::Terminal(status) => tracing::warn!(
                        agent_id = %self.agent_id,
                        process = process_label,
                        ?status,
                        "process completion is durably queued but its idle child reached a \
                         terminal status before the wake landed; it will not wake to drain it",
                    ),
                    WakeRequestOutcome::NotRegistered => tracing::warn!(
                        agent_id = %self.agent_id,
                        process = process_label,
                        "process completion is durably queued but the idle child has no wake \
                         controller registered; it survives and delivers on the next wake",
                    ),
                    WakeRequestOutcome::ChannelClosed => tracing::warn!(
                        agent_id = %self.agent_id,
                        process = process_label,
                        "process completion is durably queued but the idle child's wake \
                         controller channel is closed; it survives and delivers on the next wake",
                    ),
                }
            }
            return Ok(());
        }
        // (b) A live inbound channel: a Steer wakes a lingering agent at its
        // would-stop boundary. A full or closed channel falls through to (c).
        if let Some(inbound) = &self.inbound {
            match inbound.try_send(message.clone()) {
                Ok(()) => return Ok(()),
                Err(InboundTrySendError::Full | InboundTrySendError::Closed) => {}
            }
        }
        // (c) No live delivery: queue durably for the next step's pending flush.
        self.queue_durable(message)
    }
}

impl ProcessNotifier for ProcessMessageDelivery {
    fn deliver_completion(&self, completion: ProcessCompletion) {
        let message = self.build_message(&completion);
        if let Err(error) = self.deliver_message(&message, &completion.process_label) {
            tracing::error!(
                agent_id = %self.agent_id,
                process = %completion.process_label,
                %error,
                "background-process completion could not be delivered (channel closed and \
                 durable queue failed); the notice is lost — this is never expected",
            );
        }
    }

    fn deliver_watch_alert(&self, alert: WatchAlert) {
        let message = self.build_watch_message(&alert);
        if let Err(error) = self.deliver_message(&message, &alert.process_id) {
            tracing::error!(
                agent_id = %self.agent_id,
                watch = %alert.watch_id,
                process = %alert.process_id,
                %error,
                "watch alert could not be delivered (channel closed and durable queue failed); \
                 the alert is lost — this is never expected",
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
#[path = "process_delivery_tests/mod.rs"]
mod tests;
