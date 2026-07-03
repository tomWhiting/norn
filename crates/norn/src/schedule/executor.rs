//! The live, per-agent schedule executor: a tokio timer task that fires due
//! schedules and delivers each as a durable injected message.
//!
//! One executor is armed per library-launched agent at assembly. It sleeps
//! until the earliest `next_fire` in the agent's [`ScheduleStore`],
//! re-evaluating whenever the store changes (the tool operations signal a
//! [`tokio::sync::Notify`] the store owns). WHEN a schedule fires it builds a
//! [`ChannelMessage`] — a `Steer`, unsequenced (`seq: None`), from the
//! `norn:cron` identity with a nil sender id — and delivers it over the
//! durable path:
//!
//! - **(a)** an idle spawned child (registry status [`AgentStatus::Idle`]) is
//!   queued durably then woken through the [`AgentWakeRegistry`] — the
//!   wake tool's pending-gated contract, because a parked child does not
//!   drain its inbound channel;
//! - **(b)** otherwise a live [`InboundSender`] `try_send` delivers the steer,
//!   waking a lingering agent at its would-stop boundary via
//!   [`InboundChannel::steer_ready`](crate::r#loop::inbound::InboundChannel::steer_ready);
//! - **(c)** with no live channel (or a full/closed one) the message is queued
//!   as a [`PendingAgentMessage`] with its `agent_message.queued` audit, so
//!   the next step's pending flush injects it.
//!
//! AFTER a successful delivery the fire persists (`schedule.fired`) and the
//! schedule re-arms (recurring) or completes (one-shot). A total delivery
//! failure — closed channel *and* failed durable queue — is logged at error
//! level and the schedule advanced so it never silently drops or spins. The
//! executor makes no model/provider calls and never touches the tool
//! envelope; its lifetime is bound to the agent instance through
//! [`ScheduleExecutorGuard`], which aborts the task on drop.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::agent::pending_messages::{
    PendingAgentMessage, PendingAgentMessages, append_pending_message_audit,
};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, InboundSender, InboundTrySendError, MessageKind};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tools::agent::{AgentWakeRegistry, WakeRequestOutcome};

use super::entry::ScheduleRecord;
use super::events::{ScheduleLifecycle, append_schedule_event};
use super::store::ScheduleStore;

/// Sender attribution label stamped on every fired-schedule injection
/// (RULED-AS-FLAGGED, DECISIONS §0). The sender id is [`Uuid::nil`].
pub const CRON_SENDER_LABEL: &str = "norn:cron";

/// Tool-context extension carrying an agent's schedule store and the
/// persistence identity the `cron` tool writes lifecycle events with.
///
/// Installed by [`arm_schedule_executor`] and resolved by the `cron` tool;
/// its presence is exactly the signal that scheduling is wired for this
/// agent (the tool is registered only alongside the arming, and errors with
/// a typed `MissingExtension` if a registry were assembled without it).
pub struct ScheduleHandle {
    /// The agent's shared schedule store.
    pub store: Arc<ScheduleStore>,
    /// The owning agent's id, stamped on every created record.
    pub agent_id: Uuid,
    /// The owning agent's event store, target of every `schedule.*` event.
    pub event_store: Arc<EventStore>,
}

/// The delivery handles the executor uses to inject a fired message.
///
/// Every handle is a cheap `Arc`/clone captured at arming; the executor
/// holds no state that outlives the runtime (persistence is the store's and
/// the event log's job), so killing the runtime aborts it cleanly.
pub struct ScheduleDelivery {
    /// The owning agent — the recipient of every fired message.
    pub agent_id: Uuid,
    /// The agent's live inbound sender, when it has one.
    pub inbound: Option<InboundSender>,
    /// The session-tree-shared pending-message store, keyed by recipient.
    /// `None` only on assembly shapes that carry no durable pending store;
    /// a fire that cannot be sent live then fails loudly instead of
    /// queueing into a store nothing reads.
    pub pending: Option<Arc<PendingAgentMessages>>,
    /// The owning agent's event store (fire/queue audit target).
    pub event_store: Arc<EventStore>,
    /// The agent registry, used to detect an idle spawned child.
    pub registry: Option<Arc<RwLock<AgentRegistry>>>,
    /// The wake registry, used to resume an idle spawned child.
    pub wake_registry: Option<Arc<AgentWakeRegistry>>,
}

/// A guard binding the executor task's lifetime to the agent instance.
///
/// Dropping it aborts the executor task — so a dropped or shut-down agent
/// leaves no timer thread behind, and nothing fires after teardown. Held on
/// the agent's [`LoopContext`](crate::r#loop::loop_context::LoopContext),
/// which drops with the agent (root) or the controller task (child).
///
/// When [`arm_schedule_executor`] runs outside a Tokio runtime (assembly can
/// build an agent before `block_on`), the executor cannot spawn at arm time.
/// The armable inputs are parked in `pending_arm` and the first runner step —
/// which always runs inside a runtime — spawns the executor via
/// [`Self::ensure_armed`]. So build-time-before-runtime no longer silently
/// loses live scheduling.
pub struct ScheduleExecutorGuard {
    handle: Option<JoinHandle<()>>,
    /// Armable inputs parked when the executor could not spawn at arm time
    /// (no current runtime). Taken and spawned by the first
    /// [`Self::ensure_armed`] call inside a runtime; `None` once the executor
    /// is live or was armed eagerly.
    pending_arm: Option<(Arc<ScheduleStore>, ScheduleDelivery)>,
}

impl ScheduleExecutorGuard {
    /// Spawn the deferred executor if it was parked at arm time (built outside
    /// a Tokio runtime). Idempotent: a no-op once the executor is live. Called
    /// at the top of every runner step ([`run_agent_step`](crate::r#loop::runner::run_agent_step)),
    /// which always runs inside a runtime, so a build-then-`block_on` agent
    /// arms its executor on the first step instead of never firing live.
    pub fn ensure_armed(&mut self) {
        if self.handle.is_some() {
            return;
        }
        let Some((store, delivery)) = self.pending_arm.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                let agent_id = delivery.agent_id;
                self.handle = Some(tokio::spawn(run_executor(store, delivery)));
                tracing::debug!(
                    %agent_id,
                    "schedule executor armed lazily on the first runner step",
                );
            }
            Err(error) => {
                // Still no runtime — re-park the inputs so a later step can
                // retry rather than dropping the armable state. This is not a
                // reachable path from the runner (steps run inside a runtime),
                // so it is logged as the anomaly it would be.
                let agent_id = delivery.agent_id;
                self.pending_arm = Some((store, delivery));
                tracing::error!(
                    %error,
                    %agent_id,
                    "schedule executor still cannot spawn: ensure_armed ran outside a \
                     Tokio runtime; schedules persist and are restored on resume, but \
                     live firing remains deferred",
                );
            }
        }
    }

    /// Whether the live executor task has been spawned (eagerly at arm time or
    /// lazily on the first step).
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for ScheduleExecutorGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Install the [`ScheduleHandle`] extension on `ctx` and spawn the live
/// executor, returning a [`ScheduleExecutorGuard`] the caller binds to the
/// agent instance's lifetime.
///
/// This is the single shared mechanism every library-launched agent uses to
/// arm scheduling (root at build, spawned children and forks at their launch
/// tasks) — the same coverage pattern as
/// [`arm_auto_compaction`](crate::agent::arming::arm_auto_compaction). An
/// embedder that hand-rolls
/// [`run_agent_step`](crate::r#loop::runner::run_agent_step) without going
/// through assembly simply never calls this and therefore has no executor and
/// no `cron` tool — a discoverable, documented contract, not a silent gap.
///
/// Prefer a Tokio runtime context: the executor spawns eagerly when one is
/// current. Built outside a runtime (assembly can construct an agent before
/// `block_on`), the [`ScheduleHandle`] is still installed (so the tool
/// resolves and persists) and the armable inputs are parked on the guard —
/// [`ScheduleExecutorGuard::ensure_armed`], called by the first runner step,
/// spawns the executor then. Live scheduling is never silently lost.
#[must_use]
pub fn arm_schedule_executor(
    ctx: &ToolContext,
    store: Arc<ScheduleStore>,
    delivery: ScheduleDelivery,
) -> ScheduleExecutorGuard {
    ctx.insert_extension(Arc::new(ScheduleHandle {
        store: Arc::clone(&store),
        agent_id: delivery.agent_id,
        event_store: Arc::clone(&delivery.event_store),
    }));
    if tokio::runtime::Handle::try_current().is_ok() {
        ScheduleExecutorGuard {
            handle: Some(tokio::spawn(run_executor(store, delivery))),
            pending_arm: None,
        }
    } else {
        // No current runtime: park the armable inputs and defer the spawn to
        // the first runner step (which always runs inside a runtime).
        // Deferred, not lost — logged at debug because this is a supported
        // build-before-block_on path, not a fault.
        tracing::debug!(
            agent_id = %delivery.agent_id,
            "schedule executor arm deferred: no current Tokio runtime at build; \
             it will spawn on the first runner step",
        );
        ScheduleExecutorGuard {
            handle: None,
            pending_arm: Some((store, delivery)),
        }
    }
}

/// The executor loop: sleep until the earliest fire, then fire the due
/// batch, re-evaluating whenever the store signals a change.
async fn run_executor(store: Arc<ScheduleStore>, delivery: ScheduleDelivery) {
    loop {
        // Register interest before reading `next_fire` so a change racing the
        // read is captured by the Notify permit, never lost.
        let notified = store.notified();
        tokio::pin!(notified);
        match store.next_fire() {
            None => notified.await,
            Some(next_time) => {
                let sleep_for = next_time
                    .signed_duration_since(Utc::now())
                    .to_std()
                    .unwrap_or(Duration::ZERO);
                tokio::select! {
                    () = tokio::time::sleep(sleep_for) => {
                        fire_due(&store, next_time, &delivery);
                    }
                    () = &mut notified => {}
                }
            }
        }
    }
}

/// Fire every schedule due at or before `at`, delivering each then
/// persisting the fire and re-arming or completing it.
fn fire_due(store: &ScheduleStore, at: chrono::DateTime<Utc>, delivery: &ScheduleDelivery) {
    for record in store.due_at(at) {
        let fired_at = Utc::now();
        let late = record.late;
        let message = build_injection(&record, fired_at, late, delivery.agent_id);
        match deliver(&message, &record, delivery) {
            Ok(()) => {
                if let Err(error) = append_schedule_event(
                    &delivery.event_store,
                    &ScheduleLifecycle::Fired {
                        id: record.id,
                        fired_at,
                        late,
                    },
                ) {
                    // The message is already delivered; a failed fire audit
                    // is an observability gap, not a lost wake — log it
                    // rather than re-delivering (which would duplicate the
                    // injected message) or dropping it silently.
                    tracing::error!(
                        schedule_id = %record.id,
                        %error,
                        "failed to persist schedule.fired audit after its message was delivered",
                    );
                }
                store.complete_fire(record.id, at, Utc::now());
            }
            Err(error) => {
                tracing::error!(
                    schedule_id = %record.id,
                    agent_id = %delivery.agent_id,
                    %error,
                    "schedule fire could not be delivered (channel closed and durable \
                     queue failed); advancing the schedule rather than dropping silently",
                );
                // Advance so the executor does not spin on the same due
                // record: a recurring schedule re-arms for its next fire, a
                // one-shot is terminal-with-error-logged.
                store.complete_fire(record.id, at, Utc::now());
            }
        }
    }
}

/// Build the injected `ChannelMessage` for a fired schedule: a `Steer`,
/// unsequenced, from the `norn:cron` identity with a nil sender id, carrying
/// the structured fire payload as its content.
fn build_injection(
    record: &ScheduleRecord,
    fired_at: chrono::DateTime<Utc>,
    late: bool,
    agent_id: Uuid,
) -> ChannelMessage {
    let content = serde_json::json!({
        "schedule_id": record.id,
        "kind": record.spec.kind_label(),
        "fired_at": fired_at,
        "late": late,
        "message": record.message,
    })
    .to_string();
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::nil(),
        from: CRON_SENDER_LABEL.to_string(),
        role: None,
        to_id: agent_id,
        content,
        kind: MessageKind::Steer,
        seq: None,
        timestamp: fired_at,
    }
}

/// Deliver a fired message over the durable path. Returns `Err` only when the
/// message could neither be sent live nor durably queued.
fn deliver(
    message: &ChannelMessage,
    record: &ScheduleRecord,
    delivery: &ScheduleDelivery,
) -> Result<(), SessionError> {
    // (a) An idle spawned child does not drain its inbound channel while
    // parked, so a live `try_send` would strand the message: queue it durably
    // and request a wake through the registry (the wake tool's contract).
    if is_idle_child(delivery) {
        queue_durable(message, delivery)?;
        if let Some(wake) = &delivery.wake_registry {
            // The message is already durably queued (above): the wake only
            // decides whether the idle child resumes *now* to drain it or on
            // its next natural wake. Match the outcome so a failed wake is
            // never silent — the fire is safe either way, but a child that
            // cannot be woken is an observability signal, not a lost message.
            match wake.request_wake(delivery.agent_id) {
                // Benign: the child will drain the queued fire — either woken
                // now (`Queued`), a wake is already pending (`AlreadyQueued`),
                // or it is already running and drains at its next step
                // (`AlreadyActive`).
                WakeRequestOutcome::Queued
                | WakeRequestOutcome::AlreadyQueued
                | WakeRequestOutcome::AlreadyActive(_) => {
                    tracing::trace!(
                        agent_id = %delivery.agent_id,
                        schedule_id = %record.id,
                        "fired schedule durably queued for an idle child; wake requested",
                    );
                }
                // The child raced to a terminal status between the idle check
                // and the wake: it will never resume, so the queued fire will
                // not deliver to it. Warn honestly — the record persists but
                // its recipient is gone.
                WakeRequestOutcome::Terminal(status) => {
                    tracing::warn!(
                        agent_id = %delivery.agent_id,
                        schedule_id = %record.id,
                        ?status,
                        "fired schedule is durably queued but its idle child reached a \
                         terminal status before the wake landed; it will not wake to \
                         drain the queued message",
                    );
                }
                // No wake controller registered for this agent. The fire stays
                // durably queued and delivers on the next wake (or the next
                // step's pending flush) — it is not lost, but nothing resumes
                // the child right now.
                WakeRequestOutcome::NotRegistered => {
                    tracing::warn!(
                        agent_id = %delivery.agent_id,
                        schedule_id = %record.id,
                        "fired schedule is durably queued but the idle child has no wake \
                         controller registered; the message survives and delivers on the \
                         next wake, but no resume was triggered now",
                    );
                }
                // The controller channel closed before it could accept the
                // wake. Same durability guarantee as above: queued, delivers
                // on the next wake.
                WakeRequestOutcome::ChannelClosed => {
                    tracing::warn!(
                        agent_id = %delivery.agent_id,
                        schedule_id = %record.id,
                        "fired schedule is durably queued but the idle child's wake \
                         controller channel is closed; the message survives and delivers \
                         on the next wake, but no resume was triggered now",
                    );
                }
            }
        }
        return Ok(());
    }
    // (b) A live inbound channel: a Steer wakes a lingering agent at its
    // would-stop boundary. A full or closed channel falls through to the
    // durable queue.
    if let Some(inbound) = &delivery.inbound {
        match inbound.try_send(message.clone()) {
            Ok(()) => return Ok(()),
            Err(InboundTrySendError::Full | InboundTrySendError::Closed) => {}
        }
    }
    // (c) No live delivery: queue durably for the next step's pending flush.
    queue_durable(message, delivery)
}

/// Whether the owning agent is a registry-Idle spawned child.
fn is_idle_child(delivery: &ScheduleDelivery) -> bool {
    delivery.registry.as_ref().is_some_and(|registry| {
        registry
            .read()
            .get(delivery.agent_id)
            .is_some_and(|entry| entry.status == AgentStatus::Idle)
    })
}

/// Queue the message into the durable pending store with its
/// `agent_message.queued` audit (content-first, like the re-queue sweep).
///
/// # Errors
///
/// Returns [`SessionError::StorageError`] when no pending store is wired
/// (nothing would ever read the queued message), or the audit append
/// failure from [`append_pending_message_audit`].
fn queue_durable(
    message: &ChannelMessage,
    delivery: &ScheduleDelivery,
) -> Result<(), SessionError> {
    let Some(pending) = delivery.pending.as_ref() else {
        return Err(SessionError::StorageError {
            reason: format!(
                "no durable pending-message store is wired for agent {}; \
                 the fired schedule message has no consumer",
                delivery.agent_id
            ),
        });
    };
    let record =
        PendingAgentMessage::new(message.clone(), delivery.agent_id.to_string(), Utc::now());
    if let Some(event) = pending.queue(record) {
        append_pending_message_audit(&delivery.event_store, &event)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use chrono::TimeZone;

    use super::*;
    use crate::agent::registry::AgentRegistry;
    use crate::r#loop::inbound::inbound_channel;
    use crate::schedule::entry::ScheduleSpec;
    use crate::session::events::SessionEvent;

    fn one_shot(agent_id: Uuid) -> ScheduleRecord {
        ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::In {
                duration: Duration::from_mins(1),
            },
            "check the build".to_string(),
            agent_id,
            Utc.with_ymd_and_hms(2026, 7, 3, 12, 0, 0).unwrap(),
        )
        .unwrap()
    }

    fn recurring(agent_id: Uuid) -> ScheduleRecord {
        ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            "triage".to_string(),
            agent_id,
            Utc.with_ymd_and_hms(2026, 7, 3, 12, 0, 0).unwrap(),
        )
        .unwrap()
    }

    fn delivery_with(
        agent_id: Uuid,
        inbound: Option<InboundSender>,
        pending: Arc<PendingAgentMessages>,
        event_store: Arc<EventStore>,
        registry: Option<Arc<RwLock<AgentRegistry>>>,
        wake: Option<Arc<AgentWakeRegistry>>,
    ) -> ScheduleDelivery {
        ScheduleDelivery {
            agent_id,
            inbound,
            pending: Some(pending),
            event_store,
            registry,
            wake_registry: wake,
        }
    }

    fn cron_frame_count(store: &EventStore) -> usize {
        store
            .events()
            .iter()
            .filter(|e| {
                matches!(e, SessionEvent::Custom { event_type, .. }
                    if event_type == super::super::events::SCHEDULE_FIRED_EVENT_TYPE)
            })
            .count()
    }

    // ----- delivery paths (no timers) --------------------------------------

    #[test]
    fn deliver_uses_live_inbound_channel() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            Arc::clone(&pending),
            Arc::clone(&store),
            None,
            None,
        );
        let record = one_shot(agent_id);
        let msg = build_injection(&record, Utc::now(), false, agent_id);
        deliver(&msg, &record, &delivery).unwrap();

        let drained = rx.drain();
        assert_eq!(drained.len(), 1, "the steer landed on the live channel");
        assert_eq!(drained[0].from, CRON_SENDER_LABEL);
        assert_eq!(drained[0].sender_id, Uuid::nil());
        assert_eq!(drained[0].kind, MessageKind::Steer);
        assert!(drained[0].seq.is_none(), "cron injections are unsequenced");
        assert_eq!(pending.pending_for(agent_id), 0, "not durably queued");
    }

    #[test]
    fn deliver_queues_durably_without_a_live_channel() {
        let agent_id = Uuid::new_v4();
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let delivery = delivery_with(
            agent_id,
            None,
            Arc::clone(&pending),
            Arc::clone(&store),
            None,
            None,
        );
        let record = one_shot(agent_id);
        let msg = build_injection(&record, Utc::now(), false, agent_id);
        deliver(&msg, &record, &delivery).unwrap();

        assert_eq!(
            pending.pending_for(agent_id),
            1,
            "queued for the next flush"
        );
        let queued = store
            .events()
            .iter()
            .filter(|e| {
                matches!(e, SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::pending_messages::AGENT_MESSAGE_QUEUED_EVENT_TYPE)
            })
            .count();
        assert_eq!(queued, 1, "one agent_message.queued audit persisted");
    }

    #[tokio::test]
    async fn deliver_to_idle_child_queues_and_wakes() {
        use crate::tools::agent::AgentHandle;
        use crate::tools::agent::coord::test_support::register_agent;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::{mpsc, watch};
        use tokio_util::sync::CancellationToken;

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

        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let delivery = delivery_with(
            child,
            None,
            Arc::clone(&pending),
            Arc::clone(&store),
            Some(Arc::clone(&registry)),
            Some(Arc::clone(&wake)),
        );
        let record = one_shot(child);
        let msg = build_injection(&record, Utc::now(), false, child);
        deliver(&msg, &record, &delivery).unwrap();

        assert_eq!(
            pending.pending_for(child),
            1,
            "an idle child's fire is queued durably (it does not drain its channel)",
        );
        assert!(wake_rx.recv().await.is_some(), "a wake was requested");
    }

    /// Finding 1: when the idle child's wake returns `NotRegistered` (no wake
    /// controller in the registry), the fire is still durably queued — the
    /// warn path is honest but the message survives to deliver on the next
    /// wake. No tracing-assertion harness exists in this crate, so the
    /// warn-path is verified by its behavioral outcome: the queued message
    /// persists and `deliver` still returns `Ok`.
    #[tokio::test]
    async fn deliver_to_idle_child_with_unregistered_wake_still_queues() {
        use crate::tools::agent::coord::test_support::register_agent;

        let registry = AgentRegistry::shared();
        let child = register_agent(&registry, "/root/child", None);
        registry.write().mark_idle(child).expect("mark idle");

        // A wake registry is present but the child was never inserted into it,
        // so request_wake resolves to NotRegistered.
        let wake = Arc::new(AgentWakeRegistry::new());
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let delivery = delivery_with(
            child,
            None,
            Arc::clone(&pending),
            Arc::clone(&store),
            Some(Arc::clone(&registry)),
            Some(Arc::clone(&wake)),
        );
        let record = one_shot(child);
        let msg = build_injection(&record, Utc::now(), false, child);

        deliver(&msg, &record, &delivery).expect("delivery still succeeds");

        assert_eq!(
            pending.pending_for(child),
            1,
            "an unregistered wake leaves the fire durably queued for the next wake",
        );
        assert_eq!(
            wake.request_wake(child),
            WakeRequestOutcome::NotRegistered,
            "the wake genuinely resolves NotRegistered on this setup",
        );
    }

    /// Finding 4: `arm_schedule_executor` called outside any Tokio runtime
    /// parks the armable inputs and installs the tool extension; the first
    /// runner step inside a runtime arms the executor lazily
    /// (`ensure_armed`), and the schedule then fires live. Proven end-to-end:
    /// build outside a runtime, then run inside one.
    #[test]
    fn lazy_arm_spawns_executor_inside_runtime_and_fires() {
        let agent_id = Uuid::new_v4();
        let ctx = ToolContext::empty();
        let store = Arc::new(ScheduleStore::new());
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let (tx, mut rx) = inbound_channel(8);
        store.insert(
            ScheduleRecord::new(
                Uuid::new_v4(),
                ScheduleSpec::In {
                    // Sub-second so the test fires quickly on the real clock;
                    // the programmatic constructor accepts any positive
                    // duration (only zero is rejected — finding 5).
                    duration: Duration::from_millis(50),
                },
                "lazy wake".to_string(),
                agent_id,
                Utc::now(),
            )
            .unwrap(),
        );

        // Arm OUTSIDE any Tokio runtime: the executor cannot spawn and parks.
        let mut guard = arm_schedule_executor(
            &ctx,
            Arc::clone(&store),
            delivery_with(
                agent_id,
                Some(tx),
                Arc::clone(&pending),
                Arc::clone(&event_store),
                None,
                None,
            ),
        );
        // The extension resolves even while parked — the cron tool still works
        // before a runtime exists.
        assert!(ctx.get_extension::<ScheduleHandle>().is_some());
        assert!(!guard.is_armed(), "no runtime at arm time: executor parked");

        // Now enter a runtime and run the first step: ensure_armed spawns it.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let drained = rt.block_on(async {
            guard.ensure_armed();
            assert!(guard.is_armed(), "first step inside a runtime armed it");
            let mut got = Vec::new();
            for _ in 0..100 {
                got.extend(rx.drain());
                if !got.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            got
        });
        drop(guard);

        assert_eq!(
            drained.len(),
            1,
            "the lazily-armed executor fired the In schedule once armed",
        );
        assert_eq!(drained[0].from, CRON_SENDER_LABEL);
    }

    /// Finding 2: a live `Every` schedule whose drift-anchored re-arm falls
    /// far in the past (a suspend froze timers for hours) collapses the
    /// missed backlog into a single next fire — exactly ONE delivery, then a
    /// `next_fire` strictly in the future — rather than replaying every
    /// missed occurrence one-by-one.
    #[tokio::test(start_paused = true)]
    async fn live_catch_up_collapses_missed_every_occurrences_into_one_fire() {
        let agent_id = Uuid::new_v4();
        // Generous channel capacity so a regression (per-occurrence burst)
        // would be visible as many deliveries rather than a send failure.
        let (tx, mut rx) = inbound_channel(1024);
        let store = Arc::new(ScheduleStore::new());
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        // An Every("1m") whose first fire was ~8h ago (≈ 480 missed marks).
        let created = Utc::now() - chrono::TimeDelta::hours(8);
        let record = ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::Every {
                duration: Duration::from_mins(1),
            },
            "triage".to_string(),
            agent_id,
            created,
        )
        .unwrap();
        let id = record.id;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );
        let executor = tokio::spawn(run_executor(Arc::clone(&store), delivery));
        // Yield generously: without catch-up the executor would burst hundreds
        // of fires across these rounds; with it, exactly one then it parks.
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        executor.abort();

        let drained = rx.drain();
        assert_eq!(
            drained.len(),
            1,
            "catch-up collapses the backlog into a single fire, got {}",
            drained.len(),
        );
        let rearmed = store.get(id).expect("recurring survives");
        assert!(
            rearmed.next_fire > Utc::now(),
            "re-armed strictly into the future after the single catch-up fire",
        );
    }

    // ----- fire_due: re-arm and completion (no timers) ---------------------

    #[tokio::test]
    async fn fire_due_completes_one_shot_and_persists_fired() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let store = ScheduleStore::new();
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let record = one_shot(agent_id);
        let fire = record.next_fire;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );

        fire_due(&store, fire, &delivery);

        assert_eq!(rx.drain().len(), 1, "the one-shot fired once");
        assert!(store.is_empty(), "and completed out of the store");
        assert_eq!(
            cron_frame_count(&event_store),
            1,
            "schedule.fired persisted"
        );
    }

    #[tokio::test]
    async fn fire_due_rearms_recurring_across_three_fires() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(16);
        let store = ScheduleStore::new();
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let record = recurring(agent_id);
        let id = record.id;
        let mut fire = record.next_fire;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );

        let mut fire_times = Vec::new();
        for _ in 0..3 {
            fire_due(&store, fire, &delivery);
            let rearmed = store.get(id).expect("recurring survives each fire");
            assert!(
                rearmed.next_fire > fire,
                "next_fire advances after each fire",
            );
            fire_times.push(fire);
            fire = rearmed.next_fire;
        }

        assert_eq!(rx.drain().len(), 3, "the Every schedule fired three times");
        assert_eq!(cron_frame_count(&event_store), 3, "three fires persisted");
        // Each fire is exactly one interval after the last — no drift.
        assert_eq!(
            fire_times[1] - fire_times[0],
            chrono::TimeDelta::seconds(60)
        );
        assert_eq!(
            fire_times[2] - fire_times[1],
            chrono::TimeDelta::seconds(60)
        );
    }

    #[tokio::test]
    async fn fire_due_carries_late_flag_into_payload_and_event() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let store = ScheduleStore::new();
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let mut record = one_shot(agent_id);
        record.late = true;
        let fire = record.next_fire;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );

        fire_due(&store, fire, &delivery);

        let drained = rx.drain();
        let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
        assert_eq!(payload["late"], true, "the injected payload marks it late");
        assert_eq!(payload["message"], "check the build");
        let fired = event_store.events().into_iter().find_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == super::super::events::SCHEDULE_FIRED_EVENT_TYPE => Some(data),
            _ => None,
        });
        assert_eq!(fired.expect("fired event")["late"], true);
    }

    // ----- the live timer loop ---------------------------------------------

    /// The full executor loop fires an `In` schedule once its time arrives
    /// and delivers the injected steer. Driven under paused virtual time: a
    /// single armed sleep, advanced past its target, resolves deterministically.
    #[tokio::test(start_paused = true)]
    async fn live_executor_fires_in_schedule_and_delivers() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let store = Arc::new(ScheduleStore::new());
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let record = ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::In {
                duration: Duration::from_secs(1),
            },
            "wake up".to_string(),
            agent_id,
            Utc::now(),
        )
        .unwrap();
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );
        let executor = tokio::spawn(run_executor(Arc::clone(&store), delivery));

        // Let the executor arm its sleep, then advance well past the 1s fire.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        // Give the woken executor a chance to deliver.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let drained = rx.drain();
        assert_eq!(drained.len(), 1, "the live executor fired the In schedule");
        assert_eq!(drained[0].from, CRON_SENDER_LABEL);
        assert!(store.is_empty(), "one-shot completed");
        executor.abort();
    }

    /// A live `Every("1s")` schedule fires at least three times through the
    /// real executor loop with `next_fire` advancing each round — re-arm
    /// proven end-to-end under virtual time (bounded advance rounds, no
    /// wall-clock sleeps).
    #[tokio::test(start_paused = true)]
    async fn live_executor_every_fires_three_times_with_rearm() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(32);
        let store = Arc::new(ScheduleStore::new());
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let record = ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::Every {
                duration: Duration::from_secs(1),
            },
            "tick".to_string(),
            agent_id,
            Utc::now(),
        )
        .unwrap();
        let id = record.id;
        let first_fire = record.next_fire;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&event_store),
            None,
            None,
        );
        let executor = tokio::spawn(run_executor(Arc::clone(&store), delivery));

        // Bounded advance rounds: each round yields (so the executor arms
        // its sleep against the stored next_fire) then advances one second
        // of virtual time. Twelve rounds cover three fires with margin —
        // deterministic, never wall-clock dependent.
        let mut delivered = Vec::new();
        for _ in 0..12 {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            delivered.extend(rx.drain());
            if delivered.len() >= 3 {
                break;
            }
        }
        executor.abort();

        assert!(
            delivered.len() >= 3,
            "the Every schedule must fire at least three times, got {}",
            delivered.len(),
        );
        let rearmed = store.get(id).expect("recurring schedule stays pending");
        assert!(
            rearmed.next_fire > first_fire,
            "next_fire advanced across fires",
        );
        assert!(
            cron_frame_count(&event_store) >= 3,
            "each fire persisted a schedule.fired event",
        );
    }

    /// R5 resume restore: a persisted one-shot whose fire time passed while
    /// no process was live fires immediately when the executor arms from
    /// the rebuilt store, with `late: true` in both the injected payload
    /// and the fired event.
    #[tokio::test(start_paused = true)]
    async fn resume_late_one_shot_fires_immediately_with_late_true() {
        let agent_id = Uuid::new_v4();
        let session_store = Arc::new(EventStore::new());

        // A 1s one-shot created two hours ago, persisted, never fired.
        let created_at = Utc::now() - chrono::TimeDelta::hours(2);
        let record = ScheduleRecord::new(
            Uuid::new_v4(),
            ScheduleSpec::In {
                duration: Duration::from_secs(1),
            },
            "missed check-in".to_string(),
            agent_id,
            created_at,
        )
        .unwrap();
        append_schedule_event(
            &session_store,
            &ScheduleLifecycle::Created {
                record: record.clone(),
            },
        )
        .unwrap();

        // Resume: rebuild at the same site assembly uses, then arm.
        let store = Arc::new(ScheduleStore::from_events(
            &session_store.events(),
            Utc::now(),
        ));
        let rebuilt = store.get(record.id).expect("past-due one-shot survives");
        assert!(rebuilt.late, "rebuild marks the past-due one-shot late");

        let (tx, mut rx) = inbound_channel(8);
        let pending = Arc::new(PendingAgentMessages::new());
        let delivery = delivery_with(
            agent_id,
            Some(tx),
            pending,
            Arc::clone(&session_store),
            None,
            None,
        );
        let executor = tokio::spawn(run_executor(Arc::clone(&store), delivery));
        // The fire is already due: a zero-length sleep resolves without any
        // time advance; yields alone let it deliver.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        executor.abort();

        let drained = rx.drain();
        assert_eq!(drained.len(), 1, "the late one-shot fired immediately");
        let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
        assert_eq!(payload["late"], true);
        assert_eq!(payload["message"], "missed check-in");
        assert!(
            store.is_empty(),
            "the one-shot completed after its late fire"
        );
        let fired_late = session_store.events().into_iter().any(|e| {
            matches!(
                &e,
                SessionEvent::Custom { event_type, data, .. }
                    if event_type == super::super::events::SCHEDULE_FIRED_EVENT_TYPE
                        && data["late"] == true
            )
        });
        assert!(fired_late, "the fired event records late: true");
    }

    /// Durable-queue ordering (R5): with no live channel, the fire lands in
    /// the pending store with its `agent_message.queued` audit **before**
    /// the `schedule.fired` event — fired-after-delivered, so a crash
    /// between the two replays the fire at worst, never loses the message.
    #[tokio::test]
    async fn queue_path_persists_queued_before_fired() {
        let agent_id = Uuid::new_v4();
        let store = ScheduleStore::new();
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let record = one_shot(agent_id);
        let fire = record.next_fire;
        store.insert(record);
        let delivery = delivery_with(
            agent_id,
            None,
            Arc::clone(&pending),
            Arc::clone(&event_store),
            None,
            None,
        );

        fire_due(&store, fire, &delivery);

        let events = event_store.events();
        let queued_idx = events.iter().position(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::pending_messages::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            )
        });
        let fired_idx = events.iter().position(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == super::super::events::SCHEDULE_FIRED_EVENT_TYPE
            )
        });
        let queued_idx = queued_idx.expect("agent_message.queued persisted");
        let fired_idx = fired_idx.expect("schedule.fired persisted");
        assert!(
            queued_idx < fired_idx,
            "the durable delivery must precede the fire audit",
        );
        assert_eq!(pending.pending_for(agent_id), 1);
    }

    /// A fired message queued durably (no live channel) is injected into
    /// the conversation by the next step's pending flush — the same
    /// machinery `signal_agent`'s dormant-recipient path uses.
    #[tokio::test]
    async fn queued_fire_is_injected_by_next_step_pending_flush() {
        use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::mock::MockProvider;
        use crate::provider::usage::Usage;

        let agent_id = Uuid::new_v4();
        let session_store = EventStore::new();
        let pending = Arc::new(PendingAgentMessages::new());
        let store = ScheduleStore::new();
        let record = one_shot(agent_id);
        let fire = record.next_fire;
        store.insert(record);
        let delivery = ScheduleDelivery {
            agent_id,
            inbound: None,
            pending: Some(Arc::clone(&pending)),
            event_store: Arc::new(EventStore::new()),
            registry: None,
            wake_registry: None,
        };
        fire_due(&store, fire, &delivery);
        assert_eq!(pending.pending_for(agent_id), 1, "queued, not delivered");

        // The next step flushes the pending store into the conversation.
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "acknowledged".to_string(),
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
        let result = run_agent_step(AgentStepRequest {
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
        assert!(matches!(
            result,
            crate::r#loop::config::AgentStepResult::Completed { .. }
        ));

        let injected = session_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("from=\"norn:cron\"")
                        && content.contains("check the build")
            )
        });
        assert!(
            injected,
            "the queued fire must inject as a framed norn:cron UserMessage",
        );
        assert_eq!(pending.pending_for(agent_id), 0, "the queue drained");
    }

    /// R4's headline acceptance: an agent lingering at its would-stop
    /// boundary is genuinely woken by a fired `In("1s")` schedule — the
    /// injected `<agent_message from="norn:cron" …>` frame appears in the
    /// conversation as a persisted `UserMessage` and the loop runs another
    /// iteration. Deterministic under paused time: the executor's 1s timer
    /// is the earliest sleeper, so auto-advance fires it long before the
    /// one-minute linger deadline.
    #[tokio::test(start_paused = true)]
    async fn fired_schedule_wakes_lingering_agent() {
        use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
        use crate::r#loop::linger::LingerPolicy;
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::mock::MockProvider;
        use crate::provider::usage::Usage;

        let agent_id = Uuid::new_v4();
        let session_store = Arc::new(EventStore::new());
        let (tx, mut inbound) = inbound_channel(8);
        let schedule_store = Arc::new(ScheduleStore::new());
        schedule_store.insert(
            ScheduleRecord::new(
                Uuid::new_v4(),
                ScheduleSpec::In {
                    duration: Duration::from_secs(1),
                },
                "wake up and check".to_string(),
                agent_id,
                Utc::now(),
            )
            .unwrap(),
        );
        let pending = Arc::new(PendingAgentMessages::new());
        let executor_task = tokio::spawn(run_executor(
            Arc::clone(&schedule_store),
            ScheduleDelivery {
                agent_id,
                inbound: Some(tx),
                pending: Some(pending),
                event_store: Arc::clone(&session_store),
                registry: None,
                wake_registry: None,
            },
        ));

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
                deadline: Duration::from_mins(1),
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
        executor_task.abort();

        assert!(matches!(
            result,
            crate::r#loop::config::AgentStepResult::Completed { .. }
        ));
        assert_eq!(
            provider.call_count(),
            2,
            "the fired steer must wake the linger and drive another iteration",
        );
        let injected = session_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_message from=\"norn:cron\"")
                        && content.contains("wake up and check")
            )
        });
        assert!(
            injected,
            "the injected norn:cron frame must persist as a UserMessage",
        );
        assert_eq!(
            cron_frame_count(&session_store),
            1,
            "exactly one schedule.fired persisted",
        );
    }

    /// Dropping the guard aborts the executor task: nothing fires afterward.
    #[tokio::test(start_paused = true)]
    async fn dropping_guard_aborts_executor() {
        let agent_id = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(8);
        let ctx = ToolContext::empty();
        let store = Arc::new(ScheduleStore::new());
        let event_store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        store.insert(
            ScheduleRecord::new(
                Uuid::new_v4(),
                ScheduleSpec::Every {
                    duration: Duration::from_secs(1),
                },
                "tick".to_string(),
                agent_id,
                Utc::now(),
            )
            .unwrap(),
        );
        let guard = arm_schedule_executor(
            &ctx,
            Arc::clone(&store),
            delivery_with(
                agent_id,
                Some(tx),
                pending,
                Arc::clone(&event_store),
                None,
                None,
            ),
        );
        // Extension is installed for the cron tool to resolve.
        assert!(ctx.get_extension::<ScheduleHandle>().is_some());

        drop(guard);
        // Advance well past several intervals; an aborted executor fires nothing.
        tokio::time::advance(Duration::from_secs(5)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(rx.drain().is_empty(), "no fire after the guard dropped");
    }
}
