//! Launch/completion wrapper for [`super::spawn::SpawnAgentTool`].
//!
//! Hoisted from `spawn.rs` (W3.4) so each file stays inside the per-file
//! 500-line production-code limit: the tool's `execute` (argument
//! validation, policy granting, reservation, context assembly) stays in
//! `spawn.rs`; the `tokio::spawn` wrapper that owns the child's terminal
//! sequence — registry mark, lifecycle `Completed`, result delivery,
//! status broadcast, reclamation — lives here.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::FutureExt;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::handle::{AgentHandle, AgentWakeRegistry, ChildBranchMetadata};
use super::infra::SubAgentExecutor;
use super::lifecycle::{LifecycleEmitter, SubagentCompletion};
use super::reclaim::{ReclaimHandshake, reclaim_delivered_child};
use super::spawn_outcome::{
    extract_outcome_summary, mark_terminal_in_registry, panic_outcome_summary,
};
use crate::agent::PendingAgentMessages;
use crate::agent::child_policy::ChildLoopConfig;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::config::ToolExecutor;
use crate::r#loop::inbound::{InboundChannel, inbound_channel};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{
    AgentMessageStepRequest, AgentStepRequest, run_agent_step, run_agent_step_from_messages,
};
use crate::r#loop::{UndeliveredWindow, requeue_undelivered_inbound};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;

/// Resources moved into a spawned child's `tokio` task.
pub(super) struct ChildLaunch {
    /// Provider shared with the parent — children use the same provider.
    pub(super) provider: Arc<dyn Provider>,
    /// The child's tool executor, owning its per-child
    /// [`ToolContext`](crate::tool::context::ToolContext).
    pub(super) executor: SubAgentExecutor,
    /// The child's own session event store.
    pub(super) store: Arc<EventStore>,
    /// The child's loop context (profile-derived or task-aware default).
    /// When the child's granted delegation budget lets it spawn,
    /// `child_result_rx` is already wired here so the child's loop drains
    /// its own children's results at step boundaries (W3.4).
    pub(super) loop_ctx: LoopContext,
    /// Tool definitions the child model is shown.
    pub(super) tool_defs: Vec<ToolDefinition>,
    /// The self-contained task string the child runs.
    pub(super) task: String,
    /// Optional JSON Schema the child's final output must validate
    /// against (the spawn caller's explicit `output_schema` argument).
    pub(super) output_schema: Option<serde_json::Value>,
    /// Model identifier for the child's provider calls.
    pub(super) model: String,
    /// Shared agent registry — the spawn wrapper marks terminal status here.
    pub(super) agent_registry: Arc<RwLock<AgentRegistry>>,
    /// Channel sender for delivering results to the spawning agent's loop
    /// (the root's channel at depth 1; the mid-tree parent's own channel
    /// deeper — results bubble one hop at a time).
    pub(super) result_sender: Option<ChildResultSender>,
    /// The child's registry id.
    pub(super) child_id: Uuid,
    /// Provenance metadata stored on the child's [`AgentHandle`] (NA-008 R3).
    pub(super) branch_metadata: ChildBranchMetadata,
    /// Shared hook registry retrieved from the parent's
    /// [`ToolContext`](crate::tool::context::ToolContext). When present,
    /// the child task fires
    /// [`SubagentHook::on_subagent_stop`](crate::integration::hooks::SubagentHook::on_subagent_stop)
    /// after [`run_agent_step`] returns; a Block suppresses the
    /// registry's Completed/Failed transition (NH-006 R5).
    pub(super) hooks: Option<Arc<HookRegistry>>,
    /// Role label used as the matcher input for sub-agent hooks
    /// (profile name when supplied, otherwise the role argument). Kept
    /// on the launch so the child task does not have to recompute it.
    pub(super) role_label: String,
    /// Tagged event sender for real-time observability. When `Some`,
    /// the child's [`run_agent_step`] broadcasts every `ProviderEvent`
    /// on the shared channel so the TUI activity panel shows child
    /// tool calls in real time.
    pub(super) event_sender: Option<AgentEventSender>,
    /// `Some` when the runtime declared
    /// [`ReclaimOnResultDelivery`](super::reclaim::ReclaimOnResultDelivery)
    /// and a result channel exists: after delivering the child's result —
    /// and after the tool's handle-installed ack — the wrapper reclaims
    /// the registry entry and drops the parent-held handle (see
    /// [`super::reclaim`] for the ownership rule). `None` leaves both
    /// for an external observer or the handle holder.
    pub(super) reclaim: Option<ReclaimHandshake>,
    /// Typed lifecycle emitter — `Started` was already emitted by the
    /// tool before launch; the wrapper emits `Completed` once the run
    /// reaches a terminal outcome.
    pub(super) lifecycle: LifecycleEmitter,
    /// Workspace-shared message router. The launch path registers the
    /// child's inbound sender under its id before the task starts; the
    /// completion wrapper deregisters at the run's end — single
    /// ownership, mirroring the registry entry.
    pub(super) router: Arc<MessageRouter>,
    /// Bounded capacity of the child's inbound channel, from the granted
    /// [`ChildPolicy::inbound_capacity`](crate::agent::child_policy::ChildPolicy::inbound_capacity)
    /// (DECISION M4 — never a hardcoded library value).
    pub(super) inbound_capacity: usize,
    /// The granted per-child loop overrides, from the granted
    /// [`ChildPolicy::loop_config`](crate::agent::child_policy::ChildPolicy::loop_config)
    /// (R5 closure). The wrapper resolves it via
    /// [`ChildLoopConfig::resolve`]: `None` → the child runs
    /// [`AgentLoopConfig::default()`](crate::agent_loop::runner::AgentLoopConfig)
    /// exactly as before R5; `Some` applies the granted subset
    /// (`step_timeout`, `linger`) onto that default —
    /// a granted linger lets this child wait at its stop boundaries for
    /// its own children's late results.
    pub(super) loop_config: Option<ChildLoopConfig>,
    /// The child's run-cancellation token (W3.5 cancellation cascade):
    /// created by the spawn tool as a child of the spawner's published
    /// [`AgentCancellation`](super::infra::AgentCancellation) token —
    /// or free-standing when the spawner publishes none (token-less
    /// embedder roots; see [`AgentCancellation`](super::infra::AgentCancellation))
    /// — and already published on the child's own [`ToolContext`](crate::tool::context::ToolContext)
    /// so grandchild tokens chain under it. The trigger also lives on
    /// the parent-held [`AgentHandle`]; a clone rides into the inner
    /// run's [`AgentStepRequest`].
    pub(super) cancel: tokio_util::sync::CancellationToken,
    /// Shared wake registry. Spawn registers this child when the handle is
    /// installed, and the controller removes it before exiting.
    pub(super) wake_registry: Option<Arc<AgentWakeRegistry>>,
    /// Whether natural completion parks the child in Idle for future wakes.
    ///
    /// Children spawned inside forks are one-shot because the fork
    /// reintegrates into its parent and drops its own handle map.
    pub(super) persistent: bool,
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_owned()
}

/// Sweep every message still buffered in the child's inbound channel into
/// the child's durable pending store, with one `agent_message.queued`
/// audit per message.
///
/// The router's acceptance contract — "a message the router accepts is a
/// message some loop will drain" — is what this protects. The step
/// wrapper's own exit sweep covers messages accepted *during* a step, but
/// two windows sit outside any step: messages the router enqueued between
/// the step's exit sweep and [`MessageRouter::deregister`], and messages a
/// parent-held [`AgentHandle`] pushed while the child was parked Idle
/// (`recv` park arm). Both land here; the pending store is exactly what
/// the next wake step's pending drain reads, so nothing acknowledged is
/// ever stranded in a channel no loop owns.
pub(crate) fn requeue_stranded_inbound(
    store: &EventStore,
    child_id: Uuid,
    pending: Option<&PendingAgentMessages>,
    inbound_rx: &mut InboundChannel,
    window: UndeliveredWindow,
) {
    let mut stranded = inbound_rx.drain();
    if stranded.is_empty() {
        return;
    }
    // The controller task has no caller left to fail: the queued-audit
    // error is typed at the source (session-fidelity Gap 10) and logged
    // at error level here — the child's terminal flow (result delivery,
    // registry reclamation) must still complete, and the in-memory
    // pending record remains redeliverable while the process lives.
    if let Err(error) =
        requeue_undelivered_inbound(store, Some(child_id), pending, &mut stranded, window)
    {
        tracing::error!(
            child_id = %child_id,
            %error,
            "failed to persist queued audit event(s) while sweeping the \
             child's stranded inbound channel; affected messages will not \
             survive a restart",
        );
    }
}

fn mark_idle_in_registry(registry: &RwLock<AgentRegistry>, child_id: Uuid) {
    let mut reg = registry.write();
    if let Err(e) = reg.mark_idle(child_id) {
        super::reclaim::log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
    }
}

fn mark_active_in_registry(registry: &RwLock<AgentRegistry>, child_id: Uuid) {
    let mut reg = registry.write();
    if let Err(e) = reg.mark_active(child_id) {
        super::reclaim::log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
    }
}

fn mark_closed_in_registry(registry: &RwLock<AgentRegistry>, child_id: Uuid) {
    let mut reg = registry.write();
    if let Err(e) = reg.mark_closed(child_id) {
        super::reclaim::log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
    }
}

async fn deliver_step_result(
    result_sender: Option<&ChildResultSender>,
    child_id: Uuid,
    agent_role: &str,
    summary: &super::spawn_outcome::ChildOutcomeSummary,
) {
    let succeeded = summary.status == AgentStatus::Completed;
    let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
    if let Some(sender) = result_sender {
        let formatted_message = if succeeded {
            crate::agent::fork::format_spawn_result(
                child_id,
                agent_role,
                summary.output_text.as_deref().unwrap_or("(no output)"),
            )
        } else {
            crate::agent::fork::format_spawn_failure(
                child_id,
                agent_role,
                summary.error.as_deref().unwrap_or("unknown error"),
            )
        };
        let result = ChildAgentResult {
            agent_id: child_id,
            agent_role: agent_role.to_owned(),
            succeeded,
            formatted_message,
            error: summary.error.clone(),
            stop: summary.stop.clone(),
            usage: summary.usage.clone(),
            subtree_usage,
        };
        if let Err(e) = sender.0.send(result).await {
            tracing::error!(
                child_id = %child_id,
                error = %e,
                "spawn_agent: failed to send result through child result channel",
            );
        }
    } else {
        tracing::error!(
            child_id = %child_id,
            "spawn_agent: no child-result channel on the spawning context; \
             the child's result cannot be delivered",
        );
    }
}

async fn reclaim_after_result_delivery(
    reclaim: &mut Option<ReclaimHandshake>,
    registry: &RwLock<AgentRegistry>,
    child_id: Uuid,
) {
    if let Some(handshake) = reclaim.take() {
        if handshake.handle_installed.await.is_err() {
            tracing::warn!(
                child_id = %child_id,
                "spawn_agent: handle-installed ack dropped before launch completed; \
                 reclaiming without a stored handle",
            );
        }
        reclaim_delivered_child(registry, &handshake.handles, child_id);
    }
}

/// Launch the child on its own controller task and return the [`AgentHandle`]
/// the parent keeps.
///
/// The controller runs the initial child step, delivers that result, then parks
/// in [`AgentStatus::Idle`] until `wake_agent` asks it to run another
/// mailbox-draining step or `close_agent` cancels it.
pub(super) fn launch_child(launch: ChildLaunch) -> AgentHandle {
    let ChildLaunch {
        provider,
        executor,
        store,
        mut loop_ctx,
        tool_defs,
        task,
        output_schema,
        model,
        agent_registry,
        result_sender,
        child_id,
        branch_metadata,
        hooks,
        role_label,
        event_sender,
        reclaim,
        lifecycle,
        router,
        inbound_capacity,
        loop_config,
        cancel,
        wake_registry,
        persistent,
    } = launch;

    let handle_store = Arc::clone(&store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, mut inbound_rx) = inbound_channel(inbound_capacity);
    let (wake_tx, mut wake_rx) = mpsc::channel::<()>(1);
    let wake_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    router.register(child_id, inbound_tx.clone());
    let agent_role = format!("spawn/{model}");
    let wake_pending_for_task = Arc::clone(&wake_pending);
    let inbound_tx_for_task = inbound_tx.clone();
    let run_cancel = cancel.clone();

    // In-session cron (N-026): arm the child's schedule executor on its own
    // tool context — the same shared mechanism the root builder uses — so
    // the `cron` tool resolves its `ScheduleHandle` for spawned children
    // too. The store starts empty (children never resume; their lifecycle
    // events persist to the child's own store). The guard rides on the
    // child's loop context, which the controller task owns, so the executor
    // aborts when the controller ends — no timer outlives the child. An
    // idle-parked child is woken through the shared wake registry when a
    // schedule fires (the wake tool's queue-then-wake contract).
    if let Some(child_ctx) = executor.shared_context() {
        loop_ctx.schedule_executor = Some(crate::schedule::arm_schedule_executor(
            child_ctx.as_ref(),
            Arc::new(crate::schedule::ScheduleStore::new()),
            crate::schedule::ScheduleDelivery {
                agent_id: child_id,
                inbound: Some(inbound_tx.clone()),
                pending: loop_ctx.pending_agent_messages.clone(),
                event_store: Arc::clone(&store),
                registry: Some(Arc::clone(&agent_registry)),
                wake_registry: child_ctx.get_extension::<AgentWakeRegistry>(),
            },
        ));
        // NP-001: arm the child's own background-process manager on its tool
        // context — the same shared mechanism the root builder uses — so the
        // `process` tool resolves and the child's background processes are
        // killed when its controller task ends.
        crate::agent::arming::arm_process_manager(
            child_ctx.as_ref(),
            &mut loop_ctx,
            &store,
            child_id,
            Some(inbound_tx.clone()),
            Some(Arc::clone(&agent_registry)),
        );
    } else {
        // Structurally unreachable: `SubAgentExecutor::shared_context`
        // always returns the child context it was constructed with. Say so
        // rather than silently launching an unschedulable child.
        tracing::error!(
            child_id = %child_id,
            "spawn launch: the child executor exposes no shared tool context; \
             the schedule executor cannot arm and the cron tool will not resolve",
        );
    }

    let join_handle = tokio::spawn(async move {
        let mut child_config = ChildLoopConfig::resolve(loop_config);
        // Arm auto-compaction on the child exactly as the root builder does
        // (the one shared mechanism): install the token estimator and the
        // context-edit tracker on the child's loop context and fill its
        // context window from the catalog for the child's own model, so a
        // long-running spawned child compacts instead of dying
        // ContextWindowExceeded. A non-catalog model keeps a None window,
        // leaving the trigger off. NOTE: the root additionally hard-errors
        // on a None/over-max window (2026-07-05 incident guard); per-model
        // child validation is owned by the child-persistence/agent-variants
        // units.
        crate::agent::arming::arm_auto_compaction(&mut loop_ctx, &mut child_config, &model);
        let delivered_children = loop_ctx.children_usage.clone();
        // Cheap handle to the child's durable pending store, captured
        // before `loop_ctx` is mutably lent to the step requests: the
        // deregistration and idle-park sweeps below queue stranded
        // channel messages here.
        let pending_messages = loop_ctx.pending_agent_messages.clone();
        let result_sender = result_sender;
        let mut reclaim = reclaim;
        let mut initial = Some(task);

        loop {
            let outcome = if let Some(task) = initial.take() {
                AssertUnwindSafe(run_agent_step(AgentStepRequest {
                    provider: provider.as_ref(),
                    executor: &executor,
                    store: store.as_ref(),
                    user_prompt: &task,
                    tools: &tool_defs,
                    output_schema: output_schema.as_ref(),
                    model: &model,
                    config: &child_config,
                    event_tx: event_sender.as_ref(),
                    inbound: Some(&mut inbound_rx),
                    loop_context: &mut loop_ctx,
                    cancel: Some(run_cancel.clone()),
                }))
                .catch_unwind()
                .await
            } else {
                AssertUnwindSafe(run_agent_step_from_messages(AgentMessageStepRequest {
                    provider: provider.as_ref(),
                    executor: &executor,
                    store: store.as_ref(),
                    tools: &tool_defs,
                    output_schema: output_schema.as_ref(),
                    model: &model,
                    config: &child_config,
                    event_tx: event_sender.as_ref(),
                    initial_messages: Vec::new(),
                    inbound: Some(&mut inbound_rx),
                    loop_context: &mut loop_ctx,
                    cancel: Some(run_cancel.clone()),
                }))
                .catch_unwind()
                .await
            };

            router.deregister(child_id);
            // Messages the router accepted between the step's exit sweep
            // and the deregister above have no loop left to drain them —
            // queue them durably now.
            requeue_stranded_inbound(
                store.as_ref(),
                child_id,
                pending_messages.as_deref(),
                &mut inbound_rx,
                UndeliveredWindow::Deregistration,
            );

            let stop_blocked = if let Some(hooks_arc) = hooks.as_ref() {
                matches!(
                    hooks_arc
                        .run_subagent_stop(&child_id.to_string(), &role_label)
                        .await,
                    HookOutcome::Block { .. },
                )
            } else {
                false
            };

            let summary = match outcome {
                Ok(step_outcome) => {
                    extract_outcome_summary(step_outcome, delivered_children.snapshot())
                }
                Err(payload) => {
                    let message = format!(
                        "sub-agent task panicked before completing: {}",
                        panic_payload_message(payload.as_ref()),
                    );
                    tracing::error!(child_id = %child_id, error = %message);
                    panic_outcome_summary(message, delivered_children.snapshot())
                }
            };
            let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
            let succeeded = summary.status == AgentStatus::Completed;

            // A Completed-audit persist failure is typed at the source and
            // handled here, not propagated: the child's result is the
            // primary content and must still reach the parent (aborting
            // delivery would convert an observability gap into content
            // loss — the same documented trade as the delivered-audit,
            // session-fidelity Gap 10). Under a persistent sink fault the
            // parent's own injection of the delivered result fails its run
            // typed through the primary write-through contract.
            if let Err(error) = lifecycle.emit_completed(SubagentCompletion {
                usage: summary.usage.clone(),
                subtree_usage,
                succeeded,
                error: summary.error.clone(),
                stop: summary.stop.clone(),
            }) {
                tracing::error!(
                    child_id = %child_id,
                    %error,
                    "failed to persist the subagent.completed audit event on \
                     the parent store; the child's result is still delivered",
                );
            }
            deliver_step_result(result_sender.as_ref(), child_id, &agent_role, &summary).await;

            if !persistent {
                if !stop_blocked {
                    mark_terminal_in_registry(&agent_registry, child_id, summary.status);
                }
                let _ = status_tx.send_replace(summary.status);
                if !stop_blocked {
                    reclaim_after_result_delivery(&mut reclaim, &agent_registry, child_id).await;
                }
                break;
            }

            if run_cancel.is_cancelled() {
                if !stop_blocked {
                    mark_terminal_in_registry(&agent_registry, child_id, summary.status);
                }
                let _ = status_tx.send_replace(summary.status);
                if !stop_blocked {
                    reclaim_after_result_delivery(&mut reclaim, &agent_registry, child_id).await;
                }
                break;
            }

            if summary.status == AgentStatus::Failed && summary.stop.is_none() {
                if !stop_blocked {
                    mark_terminal_in_registry(&agent_registry, child_id, summary.status);
                }
                let _ = status_tx.send_replace(summary.status);
                if !stop_blocked {
                    reclaim_after_result_delivery(&mut reclaim, &agent_registry, child_id).await;
                }
                break;
            }

            mark_idle_in_registry(&agent_registry, child_id);
            let _ = status_tx.send_replace(AgentStatus::Idle);

            // Park until a wake or cancellation. The inbound arm keeps the
            // park honest: a message pushed through the parent-held
            // `AgentHandle::inbound_tx` while the child is parked has no
            // loop to drain it, so it is routed straight into the durable
            // pending store — making it wake-eligible (`wake_agent` reads
            // that store) instead of sitting invisibly in the channel.
            // The arm never wakes the child by itself; it re-parks.
            //
            // `inbound_open` disables the arm once every sender is
            // dropped (`recv` returning `None` would otherwise resolve
            // instantly forever). Unreachable while this task holds
            // `inbound_tx_for_task`, but a select arm must not rely on
            // that invariant to terminate.
            let mut inbound_open = true;
            let closed = loop {
                tokio::select! {
                    biased;
                    () = run_cancel.cancelled() => break true,
                    wake = wake_rx.recv() => {
                        if wake.is_none() {
                            break true;
                        }
                        mark_active_in_registry(&agent_registry, child_id);
                        let _ = status_tx.send_replace(AgentStatus::Active);
                        wake_pending_for_task.store(false, Ordering::SeqCst);
                        router.register(child_id, inbound_tx_for_task.clone());
                        break false;
                    }
                    received = inbound_rx.recv(), if inbound_open => {
                        match received {
                            Some(message) => {
                                let mut stranded = vec![message];
                                stranded.extend(inbound_rx.drain());
                                // Same handling as `requeue_stranded_inbound`:
                                // typed at the source, error-logged here — the
                                // parked controller has no caller to fail and
                                // must keep serving wake/cancel (Gap 10).
                                if let Err(error) = requeue_undelivered_inbound(
                                    store.as_ref(),
                                    Some(child_id),
                                    pending_messages.as_deref(),
                                    &mut stranded,
                                    UndeliveredWindow::IdlePark,
                                ) {
                                    tracing::error!(
                                        child_id = %child_id,
                                        %error,
                                        "failed to persist queued audit event(s) \
                                         for messages received while parked; \
                                         affected messages will not survive a \
                                         restart",
                                    );
                                }
                            }
                            None => inbound_open = false,
                        }
                    }
                }
            };
            if closed {
                mark_closed_in_registry(&agent_registry, child_id);
                let _ = status_tx.send_replace(AgentStatus::Closed);
                break;
            }
        }

        router.deregister(child_id);
        // Terminal-exit sweep: whatever still sits in the channel after
        // the controller's last deregister (cancellation during park,
        // non-persistent completion, hard failure) is queued durably so
        // the acknowledged send leaves an audit trail instead of dying
        // with the channel.
        requeue_stranded_inbound(
            store.as_ref(),
            child_id,
            pending_messages.as_deref(),
            &mut inbound_rx,
            UndeliveredWindow::Deregistration,
        );
        if let Some(registry) = wake_registry {
            registry.remove(child_id);
        }
    });

    AgentHandle {
        agent_id: child_id,
        status_rx,
        inbound_tx,
        wake_tx,
        wake_pending,
        cancel,
        join_handle,
        event_store: handle_store,
        branch_metadata,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::agent::{AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessages};
    use crate::r#loop::inbound::{ChannelMessage, MessageKind};
    use crate::session::events::SessionEvent;

    fn message(to_id: Uuid, content: &str, kind: MessageKind) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "/root/parent".to_owned(),
            role: None,
            to_id,
            content: content.to_owned(),
            kind,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    /// The deregistration sweep drains everything the channel holds into
    /// the durable pending store, in order, with one queued audit per
    /// message — steer and update alike (a stranded message has no live
    /// loop, so the kinds' delivery-timing distinction does not apply).
    #[tokio::test]
    async fn requeue_stranded_inbound_queues_all_kinds_with_audits() {
        let child_id = Uuid::new_v4();
        let store = EventStore::new();
        let pending = PendingAgentMessages::new();
        let (tx, mut rx) = inbound_channel(8);
        tx.send(message(child_id, "steer me", MessageKind::Steer))
            .await
            .expect("send steer");
        tx.send(message(child_id, "fyi", MessageKind::Update))
            .await
            .expect("send update");

        requeue_stranded_inbound(
            &store,
            child_id,
            Some(&pending),
            &mut rx,
            UndeliveredWindow::Deregistration,
        );

        assert_eq!(pending.pending_for(child_id), 2);
        let (drained, _) = pending.drain_for(child_id);
        assert_eq!(
            drained
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["steer me", "fyi"],
            "FIFO order must survive the sweep",
        );
        let audits = store
            .events()
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SessionEvent::Custom { event_type, .. }
                        if event_type == AGENT_MESSAGE_QUEUED_EVENT_TYPE
                )
            })
            .count();
        assert_eq!(audits, 2, "one agent_message.queued audit per message");
        assert!(rx.drain().is_empty(), "the channel is left empty");
    }

    /// An empty channel sweeps to nothing: no pending records, no audit
    /// events — the hot path stays silent when there is nothing stranded.
    #[tokio::test]
    async fn requeue_stranded_inbound_is_a_no_op_on_an_empty_channel() {
        let child_id = Uuid::new_v4();
        let store = EventStore::new();
        let pending = PendingAgentMessages::new();
        let (_tx, mut rx) = inbound_channel(4);

        requeue_stranded_inbound(
            &store,
            child_id,
            Some(&pending),
            &mut rx,
            UndeliveredWindow::Deregistration,
        );

        assert!(pending.is_empty());
        assert!(store.is_empty());
    }
}
