//! Launch/completion wrapper for [`super::spawn::SpawnAgentTool`].
//!
//! Hoisted from `spawn.rs` (W3.4) so each file stays inside the per-file
//! 500-line production-code limit: the tool's `execute` (argument
//! validation, policy granting, reservation, context assembly) stays in
//! `spawn.rs`; the `tokio::spawn` wrapper that owns the child's terminal
//! sequence — registry mark, lifecycle `Completed`, result delivery,
//! status broadcast, reclamation — lives here.

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::{AgentHandle, ChildBranchMetadata};
use super::infra::SubAgentExecutor;
use super::lifecycle::{LifecycleEmitter, SubagentCompletion};
use super::reclaim::{ReclaimHandshake, reclaim_delivered_child};
use super::spawn_outcome::{
    extract_outcome_summary, mark_terminal_in_registry, panicked_outcome_summary,
};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::inbound::inbound_channel;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentLoopConfig, AgentStepRequest, run_agent_step};
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
}

/// Launch the child on its own `tokio` task and return the [`AgentHandle`]
/// the parent keeps.
///
/// The spawned task runs [`run_agent_step`] to completion, marks the
/// child's terminal registry status, sends the formatted result through
/// the child result channel, and updates the status watch channel.
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
    } = launch;

    let handle_store = Arc::clone(&store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, mut inbound_rx) = inbound_channel(inbound_capacity);
    // Route registration ownership (Wave 3 §Routing): the launch path
    // registers the child's inbound sender the moment the channel exists,
    // so `send_message` can reach the child for its entire run; the
    // completion wrapper below deregisters — the same single ownership as
    // the registry entry, never two actors.
    router.register(child_id, inbound_tx.clone());
    let agent_role = format!("spawn/{model}");
    // Cooperative cancellation: the trigger lives on the parent-held
    // AgentHandle and a clone rides into the inner run's AgentStepRequest,
    // so `close_agent` can terminate the run itself — not just the wrapper
    // task. The loop observes the token at the top of every iteration and
    // races it (cancel-priority) against the in-flight provider call,
    // returning `AgentStepResult::Cancelled`, which the wrapper records as
    // the run's real outcome through its normal terminal sequence below.
    let cancel = tokio_util::sync::CancellationToken::new();
    let run_cancel = cancel.clone();

    let join_handle = tokio::spawn(async move {
        // Panic isolation: the agent step runs on its own inner task so a
        // panic inside a tool or provider (workspace code denies panics,
        // but a dependency inside the child's task can still unwind)
        // surfaces here as a `JoinError` instead of killing the wrapper.
        // The wrapper then completes every obligation of the normal
        // failure path — stop hook, lifecycle `Completed`, result-channel
        // delivery, status broadcast, registry transition, reclamation —
        // so observers never see a dangling `Started`.
        let inner = tokio::spawn(async move {
            run_agent_step(AgentStepRequest {
                provider: provider.as_ref(),
                executor: &executor,
                store: store.as_ref(),
                user_prompt: &task,
                tools: &tool_defs,
                output_schema: output_schema.as_ref(),
                model: &model,
                config: &AgentLoopConfig::default(),
                event_tx: event_sender.as_ref(),
                inbound: Some(&mut inbound_rx),
                loop_context: &mut loop_ctx,
                cancel: Some(run_cancel),
            })
            .await
        });
        let outcome = inner.await;

        // The child's loop has ended: nothing will ever drain its inbound
        // channel again, so the route is removed now — unconditionally,
        // even when a stop hook below suppresses the registry's terminal
        // transition (route ownership follows the loop's life, i.e.
        // delivery possibility; the registry entry tracks observability).
        // Later sends fail fast as NotRouted instead of enqueueing into a
        // buffer nothing reads.
        router.deregister(child_id);

        // NH-006 R5 / C57: fire SubagentHook::on_subagent_stop before
        // marking the registry's terminal status. A Block suppresses
        // the Completed/Failed transition (the agent stays in its
        // pre-terminal registry state) while the result-channel
        // summary still surfaces so the parent observes the child's
        // outcome. Hooks is None → marking happens unconditionally,
        // matching pre-NH-006 behaviour. Fires on the panic path too —
        // the child stopped either way.
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
            Ok(step_outcome) => extract_outcome_summary(step_outcome),
            Err(join_error) => {
                tracing::error!(
                    child_id = %child_id,
                    error = %join_error,
                    "spawn_agent: child task panicked or was aborted before completing",
                );
                panicked_outcome_summary(&join_error)
            }
        };
        let terminal_status = summary.status;
        let succeeded = terminal_status == AgentStatus::Completed;

        if !stop_blocked {
            mark_terminal_in_registry(&agent_registry, child_id, terminal_status);
        }

        // Typed lifecycle: the run itself finished, so `Completed` is
        // emitted unconditionally — a stop hook's Block only suppresses
        // the registry transition above, not the observable outcome.
        lifecycle.emit_completed(SubagentCompletion {
            usage: summary.usage.clone(),
            succeeded,
            error: summary.error.clone(),
            stop: summary.stop.clone(),
        });

        if let Some(sender) = result_sender {
            let formatted_message = if succeeded {
                crate::agent::fork::format_spawn_result(
                    child_id,
                    &agent_role,
                    summary.output_text.as_deref().unwrap_or("(no output)"),
                )
            } else {
                crate::agent::fork::format_spawn_failure(
                    child_id,
                    &agent_role,
                    summary.error.as_deref().unwrap_or("unknown error"),
                )
            };
            let result = ChildAgentResult {
                agent_id: child_id,
                agent_role,
                succeeded,
                formatted_message,
                error: summary.error,
                stop: summary.stop,
                usage: summary.usage,
            };
            // A send into a dropped receiver is the R5 orphaned-result
            // gap: the parent's run ended before this child finished
            // (children run default loop limits and a non-root parent
            // cannot linger until R5 closes). Error-logged, never silent;
            // reclamation below still runs — nothing can observe the
            // entry through the channel anymore.
            if let Err(e) = sender.0.send(result).await {
                tracing::error!(
                    child_id = %child_id,
                    error = %e,
                    "spawn_agent: failed to send result through child result channel",
                );
            }
        } else {
            // Only reachable on embedder contexts assembled without
            // install_agent_infra: a spawner that passed the budget gate
            // has a channel by construction. The result is undeliverable
            // — say so, never drop it silently.
            tracing::error!(
                child_id = %child_id,
                "spawn_agent: no child-result channel on the spawning context; \
                 the child's result cannot be delivered",
            );
        }

        let _ = status_tx.send_replace(terminal_status);

        // Delivery-anchored reclamation (embedded/headless runtimes):
        // the parent's record of this child is now the delivered result,
        // so the registry entry and the parent-held handle can go.
        // Skipped when a stop hook suppressed the terminal transition
        // (the child is then deliberately left observable and
        // non-terminal). A failed result send means the receiver is gone
        // — reclaiming is still correct, nothing can observe the entry
        // through the channel anymore.
        //
        // The wrapper is the sole reclaimer (see super::reclaim): it
        // first awaits the tool's handle-installed ack so a child that
        // finished before `AgentHandles::insert` ran is still reclaimed
        // with the handle present — no second actor ever reclaims
        // concurrently, and nothing infers state from registry-entry
        // absence. This holds at every depth: a grandchild's wrapper
        // reclaims its entry from the mid-tree parent's handle map the
        // same way (the MERIDIAN-HANDOFF §6 grandchild-leak gap is
        // closed by the per-agent result channel plus this pass).
        if !stop_blocked && let Some(handshake) = reclaim {
            if handshake.handle_installed.await.is_err() {
                // The tool's execute was torn down between launching the
                // child and storing the handle (e.g. the parent task was
                // cancelled mid-launch): there is no handle to drop, but
                // the registry entry still must not leak.
                tracing::warn!(
                    child_id = %child_id,
                    "spawn_agent: handle-installed ack dropped before launch completed; \
                     reclaiming without a stored handle",
                );
            }
            reclaim_delivered_child(&agent_registry, &handshake.handles, child_id);
        }
    });

    AgentHandle {
        agent_id: child_id,
        status_rx,
        inbound_tx,
        cancel,
        join_handle,
        event_store: handle_store,
        branch_metadata,
    }
}
