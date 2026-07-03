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

use crate::agent::pending_messages::{
    PendingAgentMessage, PendingAgentMessages, append_pending_message_audit,
};
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
        let record =
            PendingAgentMessage::new(message.clone(), self.agent_id.to_string(), Utc::now());
        if let Some(event) = pending.queue(record) {
            append_pending_message_audit(&self.event_store, &event)?;
        }
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
mod tests {
    use super::*;
    use crate::r#loop::inbound::inbound_channel;

    fn completion(label: &str, exit_code: Option<i32>, killed: bool) -> ProcessCompletion {
        ProcessCompletion {
            process_label: label.to_owned(),
            command: "cargo test".to_owned(),
            exit_code,
            killed,
            started_at: Utc::now(),
            exited_at: Utc::now(),
            spool_path: "~/.norn/outputs/sess/processes/p1.log".to_owned(),
        }
    }

    fn delivery(
        agent_id: Uuid,
        inbound: Option<InboundSender>,
        pending: Arc<PendingAgentMessages>,
        event_store: Arc<EventStore>,
    ) -> ProcessMessageDelivery {
        ProcessMessageDelivery {
            agent_id,
            inbound,
            pending: Some(pending),
            event_store,
            registry: None,
            wake_registry: None,
        }
    }

    #[test]
    fn delivers_over_a_live_inbound_channel_as_a_nil_sender_steer() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let pending = Arc::new(PendingAgentMessages::new());
        let sink = delivery(
            agent_id,
            Some(tx),
            Arc::clone(&pending),
            Arc::new(EventStore::new()),
        );
        sink.deliver_completion(completion("p1", Some(0), false));

        let drained = rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].from, PROCESS_MANAGER_SENDER_LABEL);
        assert_eq!(drained[0].sender_id, Uuid::nil());
        assert_eq!(drained[0].kind, MessageKind::Steer);
        assert!(drained[0].seq.is_none());
        let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
        assert_eq!(payload["process_id"], "p1");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["killed"], false);
        assert_eq!(pending.pending_for(agent_id), 0, "not durably queued");
    }

    #[test]
    fn queues_durably_without_a_live_channel_with_a_queued_audit() {
        let agent_id = Uuid::new_v4();
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let sink = delivery(agent_id, None, Arc::clone(&pending), Arc::clone(&store));
        sink.deliver_completion(completion("p2", None, true));

        assert_eq!(
            pending.pending_for(agent_id),
            1,
            "queued for the next flush"
        );
        let queued = store
            .events()
            .iter()
            .filter(|e| {
                matches!(e, crate::session::events::SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::pending_messages::AGENT_MESSAGE_QUEUED_EVENT_TYPE)
            })
            .count();
        assert_eq!(queued, 1, "one agent_message.queued audit persisted");
    }

    #[test]
    fn killed_disposition_is_distinct_from_a_normal_exit() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let sink = delivery(
            agent_id,
            Some(tx),
            Arc::new(PendingAgentMessages::new()),
            Arc::new(EventStore::new()),
        );
        sink.deliver_completion(completion("p3", None, true));
        let drained = rx.drain();
        let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
        assert_eq!(payload["killed"], true);
        assert_eq!(payload["exit_code"], serde_json::Value::Null);
        assert!(payload["hint"].as_str().unwrap().contains("killed"));
    }

    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial]`; no concurrent reader.
            unsafe { std::env::set_var("NORN_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("NORN_HOME", v) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    /// C17 end-to-end: a real manager-owned process, wired to a live inbound
    /// channel, delivers its completion as a `norn:process-manager` steer when
    /// it exits — the supervisor → sink path proven with a real subprocess.
    #[tokio::test]
    #[serial_test::serial]
    async fn real_process_completion_delivers_a_steer_over_a_live_channel() {
        use crate::process::ProcessManager;

        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let sink: Arc<dyn ProcessNotifier> = Arc::new(delivery(
            agent_id,
            Some(tx),
            Arc::new(PendingAgentMessages::new()),
            Arc::new(EventStore::new()),
        ));
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
        let cwd = std::env::current_dir().unwrap();
        let handle = manager.spawn("echo hi", &cwd, None).await.unwrap();

        // Wait for exit, then for the supervisor to run the sink.
        let mut delivered = Vec::new();
        for _ in 0..200 {
            delivered.extend(rx.drain());
            if !delivered.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(delivered.len(), 1, "one completion steer delivered");
        assert_eq!(delivered[0].from, PROCESS_MANAGER_SENDER_LABEL);
        assert_eq!(delivered[0].kind, MessageKind::Steer);
        let payload: serde_json::Value = serde_json::from_str(&delivered[0].content).unwrap();
        assert_eq!(payload["process_id"], handle.label());
        assert_eq!(payload["exit_code"], 0);
    }

    /// C17 idle-child path: with no live channel the completion is queued
    /// durably and a wake is requested through the registry.
    #[tokio::test]
    async fn idle_child_completion_queues_and_wakes() {
        use std::sync::atomic::AtomicBool;

        use tokio::sync::{mpsc, watch};
        use tokio_util::sync::CancellationToken;

        use crate::tools::agent::AgentHandle;
        use crate::tools::agent::coord::test_support::register_agent;

        let registry = AgentRegistry::shared();
        let child = register_agent(&registry, "/root/child", None);
        registry.write().mark_idle(child).expect("mark idle");

        let wake = Arc::new(AgentWakeRegistry::new());
        let (_status_tx, status_rx) = watch::channel(AgentStatus::Idle);
        let (inbound_tx, _inbound_rx) = inbound_channel(8);
        let (wake_tx, mut wake_rx) = mpsc::channel(1);
        let handle = AgentHandle {
            agent_id: child,
            status_rx,
            inbound_tx,
            wake_tx,
            wake_pending: Arc::new(AtomicBool::new(false)),
            cancel: CancellationToken::new(),
            join_handle: tokio::spawn(async {}),
            event_store: Arc::new(EventStore::new()),
            branch_metadata: crate::tools::agent::handle::ChildBranchMetadata {
                child_agent_id: child,
                parent_agent_id: Uuid::nil(),
                profile_name: None,
                spawned_at: Utc::now(),
            },
        };
        wake.insert(handle.wake_handle());

        let pending = Arc::new(PendingAgentMessages::new());
        let sink = ProcessMessageDelivery {
            agent_id: child,
            inbound: None,
            pending: Some(Arc::clone(&pending)),
            event_store: Arc::new(EventStore::new()),
            registry: Some(Arc::clone(&registry)),
            wake_registry: Some(Arc::clone(&wake)),
        };
        sink.deliver_completion(completion("p1", Some(0), false));

        assert_eq!(
            pending.pending_for(child),
            1,
            "queued durably for the idle child"
        );
        assert!(wake_rx.recv().await.is_some(), "a wake was requested");
    }

    /// C18 end-to-end: an agent lingering at its would-stop boundary is woken
    /// by a real background process's completion — the injected
    /// `<agent_message from="norn:process-manager" …>` frame appears in the
    /// conversation as a persisted `UserMessage` and the loop runs again.
    #[tokio::test]
    #[serial_test::serial]
    async fn completion_wakes_a_lingering_agent() {
        use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
        use crate::r#loop::linger::LingerPolicy;
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
        use crate::process::ProcessManager;
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::mock::MockProvider;
        use crate::provider::usage::Usage;

        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let agent_id = Uuid::new_v4();
        let session_store = Arc::new(EventStore::new());
        let (tx, mut inbound) = inbound_channel(8);
        let sink: Arc<dyn ProcessNotifier> = Arc::new(ProcessMessageDelivery {
            agent_id,
            inbound: Some(tx),
            pending: Some(Arc::new(PendingAgentMessages::new())),
            event_store: Arc::clone(&session_store),
            registry: None,
            wake_registry: None,
        });
        // A short-lived process (~0.2s): its completion steer arrives well
        // within the linger deadline. Real time is used deliberately — a real
        // subprocess cannot be driven under paused virtual time. The deadline
        // is kept small because a trailing linger with no further steer costs
        // one full deadline before the step returns.
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
        let cwd = std::env::current_dir().unwrap();
        manager
            .spawn("sleep 0.2; echo bg-done", &cwd, None)
            .await
            .unwrap();

        let text_turn = |text: &str| {
            vec![
                ProviderEvent::TextDelta {
                    text: text.to_string(),
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ]
        };
        let provider = MockProvider::new(vec![text_turn("first"), text_turn("after wake")]);
        let tool_executor = MockToolExecutor::empty();
        let mut loop_context = crate::r#loop::loop_context::LoopContext::new("system");
        let config = AgentLoopConfig {
            linger: Some(LingerPolicy {
                deadline: std::time::Duration::from_secs(3),
            }),
            ..AgentLoopConfig::default()
        };
        let result = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &tool_executor,
            store: session_store.as_ref(),
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: Some(&mut inbound),
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await
        .expect("run_agent_step");

        assert!(matches!(
            result,
            crate::r#loop::config::AgentStepResult::Completed { .. }
        ));
        assert_eq!(
            provider.call_count(),
            2,
            "the completion steer must wake the linger and drive another iteration",
        );
        let injected = session_store.events().iter().any(|e| {
            matches!(
                e,
                crate::session::events::SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_message from=\"norn:process-manager\"")
                        && content.contains("process_id")
            )
        });
        assert!(
            injected,
            "the injected norn:process-manager frame persists as a UserMessage"
        );
    }

    // ----- NP-002 watch alerts --------------------------------------------

    fn match_alert(watch_id: &str, process_id: &str) -> WatchAlert {
        WatchAlert {
            watch_id: watch_id.to_owned(),
            process_id: process_id.to_owned(),
            brief: "watch for errors".to_owned(),
            spool_start: 10,
            spool_end: 42,
            kind: WatchAlertKind::Match {
                excerpt: "ERROR: boom\n".to_owned(),
                matched_at: Utc::now(),
            },
        }
    }

    /// R3: the alert content parses as JSON with `watch_id`, `process_id`,
    /// `brief`, `excerpt`, `spool_range`, and `matched_at` — asserted
    /// field-by-field — carried on a `norn:watch` nil-sender unsequenced steer.
    /// The excerpt is the filter's stdout byte-equal (not re-derived).
    #[test]
    fn watch_match_alert_parses_field_by_field() {
        let agent_id = Uuid::new_v4();
        let sink = delivery(
            agent_id,
            None,
            Arc::new(PendingAgentMessages::new()),
            Arc::new(EventStore::new()),
        );
        let message = sink.build_watch_message(&match_alert("w1", "p1"));
        assert_eq!(message.from, WATCH_SENDER_LABEL);
        assert_eq!(message.sender_id, Uuid::nil());
        assert_eq!(message.kind, MessageKind::Steer);
        assert!(message.seq.is_none());
        assert_eq!(message.role, None);
        assert_eq!(message.to_id, agent_id);

        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["type"], "watch_match");
        assert_eq!(payload["watch_id"], "w1");
        assert_eq!(payload["process_id"], "p1");
        assert_eq!(payload["brief"], "watch for errors");
        assert_eq!(payload["excerpt"], "ERROR: boom\n");
        assert_eq!(payload["spool_range"]["start"], 10);
        assert_eq!(payload["spool_range"]["end"], 42);
        assert!(
            payload["matched_at"].as_str().is_some(),
            "matched_at is present as a timestamp",
        );
    }

    /// R4: a watch-error alert carries the error and the examined spool range,
    /// distinctly typed from a match, with no excerpt field.
    #[test]
    fn watch_error_alert_carries_error_and_range() {
        let agent_id = Uuid::new_v4();
        let sink = delivery(
            agent_id,
            None,
            Arc::new(PendingAgentMessages::new()),
            Arc::new(EventStore::new()),
        );
        let alert = WatchAlert {
            watch_id: "w2".to_owned(),
            process_id: "p3".to_owned(),
            brief: "b".to_owned(),
            spool_start: 0,
            spool_end: 12,
            kind: WatchAlertKind::Error {
                error: "filter is not runnable".to_owned(),
            },
        };
        let message = sink.build_watch_message(&alert);
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["type"], "watch_error");
        assert_eq!(payload["watch_id"], "w2");
        assert_eq!(payload["process_id"], "p3");
        assert_eq!(payload["error"], "filter is not runnable");
        assert_eq!(payload["spool_range"]["end"], 12);
        assert!(payload["excerpt"].is_null(), "an error carries no excerpt");
    }

    /// R3: with no live channel a watch alert lands in the durable pending
    /// store with an `agent_message.queued` audit, drained by the next step.
    #[test]
    fn watch_alert_queues_durably_with_a_queued_audit() {
        let agent_id = Uuid::new_v4();
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let sink = delivery(agent_id, None, Arc::clone(&pending), Arc::clone(&store));
        sink.deliver_watch_alert(match_alert("w1", "p1"));

        assert_eq!(
            pending.pending_for(agent_id),
            1,
            "queued for the next flush"
        );
        let queued = store
            .events()
            .iter()
            .filter(|e| {
                matches!(e, crate::session::events::SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::pending_messages::AGENT_MESSAGE_QUEUED_EVENT_TYPE)
            })
            .count();
        assert_eq!(queued, 1, "one agent_message.queued audit persisted");
    }

    /// R3 headline: an agent lingering at a would-stop boundary is woken by a
    /// real background process's watch match; the injected
    /// `<agent_message from="norn:watch" …>` frame appears in the conversation
    /// and persists as a `UserMessage` event.
    #[tokio::test]
    #[serial_test::serial]
    async fn watch_match_wakes_a_lingering_agent() {
        use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
        use crate::r#loop::linger::LingerPolicy;
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
        use crate::process::ProcessManager;
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::mock::MockProvider;
        use crate::provider::usage::Usage;

        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let agent_id = Uuid::new_v4();
        let session_store = Arc::new(EventStore::new());
        let (tx, mut inbound) = inbound_channel(8);
        let sink: Arc<dyn ProcessNotifier> = Arc::new(ProcessMessageDelivery {
            agent_id,
            inbound: Some(tx),
            pending: Some(Arc::new(PendingAgentMessages::new())),
            event_store: Arc::clone(&session_store),
            registry: None,
            wake_registry: None,
        });
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
        let cwd = std::env::current_dir().unwrap();
        let handle = manager
            .spawn("sleep 0.2; echo WATCHED-LINE", &cwd, None)
            .await
            .unwrap();
        manager
            .attach_watch(
                handle.label(),
                "watched output".to_owned(),
                "grep WATCHED-LINE".to_owned(),
                cwd,
                None,
            )
            .unwrap();

        let text_turn = |text: &str| {
            vec![
                ProviderEvent::TextDelta {
                    text: text.to_string(),
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ]
        };
        let provider = MockProvider::new(vec![text_turn("first"), text_turn("after wake")]);
        let tool_executor = MockToolExecutor::empty();
        let mut loop_context = crate::r#loop::loop_context::LoopContext::new("system");
        let config = AgentLoopConfig {
            linger: Some(LingerPolicy {
                deadline: std::time::Duration::from_secs(3),
            }),
            ..AgentLoopConfig::default()
        };
        let result = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &tool_executor,
            store: session_store.as_ref(),
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: Some(&mut inbound),
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await
        .expect("run_agent_step");

        assert!(matches!(
            result,
            crate::r#loop::config::AgentStepResult::Completed { .. }
        ));
        assert_eq!(
            provider.call_count(),
            2,
            "the watch match must wake the linger and drive another iteration",
        );
        let injected = session_store.events().iter().any(|e| {
            matches!(
                e,
                crate::session::events::SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_message from=\"norn:watch\"")
                        && content.contains("watch_match")
                        && content.contains("WATCHED-LINE")
            )
        });
        assert!(
            injected,
            "the injected norn:watch frame persists as a UserMessage carrying the excerpt",
        );
        manager.shutdown();
    }

    /// C17 durable path end-to-end: a completion queued with no live channel is
    /// injected as a framed `from="norn:process-manager"` `UserMessage` by the
    /// next step's pending flush.
    #[tokio::test]
    async fn queued_completion_is_injected_by_next_step_pending_flush() {
        use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::mock::MockProvider;
        use crate::provider::usage::Usage;

        let agent_id = Uuid::new_v4();
        let session_store = EventStore::new();
        let pending = Arc::new(PendingAgentMessages::new());
        let sink = delivery(
            agent_id,
            None,
            Arc::clone(&pending),
            Arc::new(EventStore::new()),
        );
        sink.deliver_completion(completion("p1", Some(0), false));
        assert_eq!(pending.pending_for(agent_id), 1, "queued, not delivered");

        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "ack".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]]);
        let executor = MockToolExecutor::empty();
        let mut loop_context = crate::r#loop::loop_context::LoopContext::new("system");
        loop_context.agent_id = Some(agent_id);
        loop_context.pending_agent_messages = Some(Arc::clone(&pending));
        let config = AgentLoopConfig::default();
        run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &session_store,
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await
        .expect("run_agent_step");

        let injected = session_store.events().iter().any(|e| {
            matches!(
                e,
                crate::session::events::SessionEvent::UserMessage { content, .. }
                    if content.contains("from=\"norn:process-manager\"")
                        && content.contains("process_id")
            )
        });
        assert!(
            injected,
            "the queued completion injects as a framed UserMessage"
        );
        assert_eq!(pending.pending_for(agent_id), 0, "the queue drained");
    }
}
