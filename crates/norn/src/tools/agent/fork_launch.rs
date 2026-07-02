//! Fork launch: the `tokio::spawn` task and completion wrapper for
//! [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Hoisted from [`super::fork_pipeline`] (which keeps context
//! construction, store resolution, and outcome projection) so both
//! modules stay inside the per-file 500-line production-code limit
//! (CO5) — mirroring the [`super::spawn_launch`] / spawn split.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::fork_pipeline::{
    append_fork_complete, mark_fork_terminal, panicked_fork_outcome, project_fork_outcome,
};
use super::handle::{AgentHandle, ChildBranchMetadata};
use super::infra::SubAgentExecutor;
use super::lifecycle::{LifecycleEmitter, SubagentCompletion};
use super::reclaim::{ReclaimHandshake, reclaim_delivered_child};
use crate::agent::child_policy::ChildLoopConfig;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::inbound::{InboundChannel, InboundSender};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::session::tree::SessionId;

/// Resources moved into a fork's `tokio::spawn` task.
pub(super) struct ForkLaunch {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) executor: SubAgentExecutor,
    pub(super) child_store: Arc<EventStore>,
    pub(super) parent_store: Arc<EventStore>,
    pub(super) loop_ctx: LoopContext,
    pub(super) tool_defs: Vec<ToolDefinition>,
    pub(super) output_schema: serde_json::Value,
    pub(super) inbound_rx: InboundChannel,
    pub(super) request: String,
    pub(super) model: String,
    pub(super) agent_registry: Arc<RwLock<AgentRegistry>>,
    pub(super) result_sender: Option<ChildResultSender>,
    pub(super) requirement_names: Vec<String>,
    pub(super) fork_id: Uuid,
    pub(super) parent_id: Uuid,
    pub(super) forked_session_id: Option<SessionId>,
    pub(super) event_sender: Option<AgentEventSender>,
    /// `Some` when the runtime declared
    /// [`ReclaimOnResultDelivery`](super::reclaim::ReclaimOnResultDelivery)
    /// and a result channel exists: after delivering the fork's result —
    /// and after the tool's handle-installed ack — the wrapper reclaims
    /// the registry entry and drops the parent-held handle (see
    /// [`super::reclaim`]).
    pub(super) reclaim: Option<ReclaimHandshake>,
    /// Typed lifecycle emitter — `Started` was already emitted by the
    /// tool before launch; the wrapper emits `Completed` once the run
    /// reaches a terminal outcome.
    pub(super) lifecycle: LifecycleEmitter,
    /// Shared hook registry retrieved from the parent's
    /// [`ToolContext`](crate::tool::context::ToolContext). When present,
    /// the wrapper fires
    /// [`SubagentHook::on_subagent_stop`](crate::integration::hooks::SubagentHook::on_subagent_stop)
    /// after the run finishes; a Block suppresses the registry's terminal
    /// transition (and delivery-anchored reclamation) while the outcome
    /// still surfaces — identical semantics to the spawn wrapper
    /// (NH-006 R5).
    pub(super) hooks: Option<Arc<HookRegistry>>,
    /// Workspace-shared message router. [`launch_fork`] registers the
    /// fork's inbound sender under its id before the task starts; the
    /// completion wrapper deregisters at the run's end — single
    /// ownership, mirroring the registry entry and the spawn wrapper.
    pub(super) router: Arc<MessageRouter>,
    /// The fork's run-cancellation token (W3.5 cancellation cascade):
    /// created by the fork tool as a child of the forker's published
    /// [`AgentCancellation`](super::infra::AgentCancellation) token — or
    /// free-standing when the forker publishes none — and already
    /// published on the fork's own
    /// [`ToolContext`](crate::tool::context::ToolContext) so grandchild
    /// tokens chain under it. The trigger also lives on the parent-held
    /// [`AgentHandle`]; a clone rides into the inner run's
    /// [`AgentStepRequest`]. Mirrors the spawn launch.
    pub(super) cancel: tokio_util::sync::CancellationToken,
    /// The granted per-fork loop overrides, from the granted
    /// [`ChildPolicy::loop_config`](crate::agent::child_policy::ChildPolicy::loop_config)
    /// (R5 closure). Resolved via [`ChildLoopConfig::resolve`]: `None` →
    /// the fork runs
    /// [`AgentLoopConfig::default()`](crate::agent_loop::runner::AgentLoopConfig)
    /// exactly as before R5; `Some` applies the granted subset
    /// (`max_iterations`, `step_timeout`, `linger`) onto that default.
    /// Mirrors the spawn launch.
    pub(super) loop_config: Option<ChildLoopConfig>,
}

/// Launch the fork on its own `tokio::spawn` task and build the parent-side
/// [`AgentHandle`].
pub(super) fn launch_fork(launch: ForkLaunch, inbound_tx: InboundSender) -> AgentHandle {
    let ForkLaunch {
        provider,
        executor,
        child_store,
        parent_store,
        mut loop_ctx,
        tool_defs,
        output_schema,
        mut inbound_rx,
        request,
        model,
        agent_registry,
        result_sender,
        requirement_names,
        fork_id,
        parent_id,
        forked_session_id,
        event_sender,
        reclaim,
        lifecycle,
        hooks,
        router,
        cancel,
        loop_config,
    } = launch;

    let handle_store = Arc::clone(&child_store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    // Route registration ownership (Wave 3 §Routing): registered before
    // the task starts so `signal_agent` can reach the fork for its entire
    // run; deregistered by the completion wrapper below — single
    // ownership, never two actors.
    router.register(fork_id, inbound_tx.clone());
    let agent_role = format!("fork/{model}");
    // Cooperative cancellation: the trigger lives on the parent-held
    // AgentHandle and a clone rides into the inner run's AgentStepRequest,
    // so `close_agent` can terminate the run itself — not just the wrapper
    // task. The token arrives on the launch (W3.5): the fork tool created
    // it as a child of the forker's own token, so cancelling any ancestor
    // cascades here too. The loop observes the token at the top of every
    // iteration and races it (cancel-priority) against the in-flight
    // provider call, returning `AgentStepResult::Cancelled`, which the
    // wrapper records as the run's real outcome through its normal
    // terminal sequence below. Mirrors the spawn wrapper.
    let run_cancel = cancel.clone();

    let join_handle = tokio::spawn(async move {
        let started = Instant::now();
        // W3.6 usage rollup: cheap-clone handle to the fork's
        // children-usage accumulator, captured before `loop_ctx` moves
        // into the inner task — read below as the fallback for the
        // panic and hard-error paths, where no `AgentStepResult` exists
        // to carry the delivered grandchild subtrees out of the loop.
        let delivered_children = loop_ctx.children_usage.clone();
        // Panic isolation: the agent step runs on its own inner task so a
        // panic inside a tool or provider (workspace code denies panics,
        // but a dependency inside the fork's task can still unwind)
        // surfaces here as a `JoinError` instead of killing the wrapper.
        // The wrapper then completes every obligation of the normal
        // failure path — stop hook, `ForkComplete`, lifecycle
        // `Completed`, result delivery, status broadcast, registry
        // transition, reclamation — so observers never see a dangling
        // `Started`. Mirrors the spawn wrapper.
        let inner = tokio::spawn(async move {
            // R5: the fork's loop config is the granted ChildLoopConfig
            // applied onto AgentLoopConfig::default(); an absent grant is
            // byte-for-byte the default — the pre-R5 behavior.
            let mut fork_config = ChildLoopConfig::resolve(loop_config);
            // Arm auto-compaction on the fork exactly as the root builder
            // does (the one shared mechanism): install the token estimator
            // and the context-edit tracker on the fork's loop context and
            // fill its context window from the catalog for the fork's own
            // model, so a long-running fork compacts instead of dying
            // ContextWindowExceeded. A non-catalog model keeps a None
            // window, leaving the trigger off — matching the root behavior.
            crate::agent::assembly::arm_auto_compaction(&mut loop_ctx, &mut fork_config, &model);
            run_agent_step(AgentStepRequest {
                provider: provider.as_ref(),
                executor: &executor,
                store: child_store.as_ref(),
                user_prompt: &request,
                tools: &tool_defs,
                output_schema: Some(&output_schema),
                model: &model,
                config: &fork_config,
                event_tx: event_sender.as_ref(),
                inbound: Some(&mut inbound_rx),
                loop_context: &mut loop_ctx,
                cancel: Some(run_cancel),
            })
            .await
        });
        let outcome = match inner.await {
            Ok(step_result) => {
                if let Err(ref e) = step_result {
                    tracing::error!(
                        fork_id = %fork_id,
                        role = %agent_role,
                        error = %e,
                        elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "fork: run_agent_step failed",
                    );
                }
                project_fork_outcome(step_result, started, delivered_children.snapshot())
            }
            Err(join_error) => {
                tracing::error!(
                    fork_id = %fork_id,
                    role = %agent_role,
                    error = %join_error,
                    "fork: child task panicked or was aborted before completing",
                );
                panicked_fork_outcome(
                    &join_error,
                    started.elapsed(),
                    delivered_children.snapshot(),
                )
            }
        };
        // W3.6: the fork's subtree total — its own provider spend plus
        // everything its descendants delivered. Own usage stays
        // own-calls-only on `outcome.usage`; the aggregation is explicit
        // here and computed exactly once per fork.
        let subtree_usage = outcome.usage.clone() + outcome.children_usage.clone();

        // The fork's loop has ended: nothing will ever drain its inbound
        // channel again, so the route is removed now — unconditionally,
        // even when a stop hook below suppresses the registry's terminal
        // transition (route ownership follows the loop's life; the
        // registry entry tracks observability). Later sends fail fast as
        // NotRouted instead of enqueueing into a buffer nothing reads.
        router.deregister(fork_id);

        // NH-006 R5 parity with spawn: fire SubagentHook::on_subagent_stop
        // before the registry's terminal transition. A Block suppresses
        // the transition (and reclamation below) while the outcome still
        // surfaces — the parent always observes the fork's result.
        let stop_blocked = if let Some(hooks_arc) = hooks.as_ref() {
            matches!(
                hooks_arc
                    .run_subagent_stop(&fork_id.to_string(), "fork")
                    .await,
                HookOutcome::Block { .. },
            )
        } else {
            false
        };
        if !stop_blocked {
            mark_fork_terminal(&agent_registry, fork_id, outcome.status);
        }

        append_fork_complete(parent_store.as_ref(), forked_session_id, &outcome, fork_id);

        // Typed lifecycle: emit `Completed` with the fork's accumulated
        // usage, terminal outcome, and typed stop reason.
        lifecycle.emit_completed(SubagentCompletion {
            usage: outcome.usage.clone(),
            subtree_usage: subtree_usage.clone(),
            succeeded: outcome.status == AgentStatus::Completed,
            error: outcome.error_message.clone(),
            stop: outcome.stop.clone(),
        });

        if let Some(sender) = result_sender {
            let (succeeded, formatted_message, error) =
                crate::agent::fork::format_fork_outcome(fork_id, &outcome, &requirement_names);
            let result = ChildAgentResult {
                agent_id: fork_id,
                agent_role,
                succeeded,
                formatted_message,
                error,
                stop: outcome.stop.clone(),
                usage: outcome.usage.clone(),
                subtree_usage,
            };
            // A send into a dropped receiver means the parent's run
            // ended before this fork finished. Since R5 closed, a parent
            // at any depth can be granted a linger
            // (child_policy.loop_config) to wait for exactly this result;
            // a parent that was granted none — or whose linger deadline
            // expired — still loses it. A cascaded cancel (W3.5) hits
            // this path by design — a cancelled mid-tree parent's loop
            // ends and drops its receiver while this fork's own cancelled
            // run is still wrapping up. Error-logged, never silent;
            // reclamation below still runs.
            if let Err(e) = sender.0.send(result).await {
                tracing::error!(
                    fork_id = %fork_id,
                    error = %e,
                    "fork: failed to send result through child result channel",
                );
            }
        } else {
            // Only reachable on embedder contexts assembled without
            // install_agent_infra: a forker that passed the budget gate
            // has a channel by construction. The result is undeliverable
            // — say so, never drop it silently.
            tracing::error!(
                fork_id = %fork_id,
                "fork: no child-result channel on the forking context; \
                 the fork's result cannot be delivered",
            );
        }

        let _ = status_tx.send_replace(outcome.status);

        // Delivery-anchored reclamation (embedded/headless runtimes):
        // the parent's record of this fork is now the delivered result
        // plus the ForkComplete event on its timeline, so the registry
        // entry and the parent-held handle can go. Skipped when a stop
        // hook suppressed the terminal transition (the fork is then
        // deliberately left observable and non-terminal — mirrors
        // spawn). A failed result send means the receiver is gone —
        // reclaiming is still correct.
        //
        // The wrapper is the sole reclaimer (see super::reclaim): it
        // first awaits the tool's handle-installed ack so a fork that
        // finished before `AgentHandles::insert` ran is still reclaimed
        // with the handle present — no second actor ever reclaims
        // concurrently, and nothing infers state from registry-entry
        // absence.
        if !stop_blocked && let Some(handshake) = reclaim {
            if handshake.handle_installed.await.is_err() {
                // The tool's execute was torn down between launching the
                // wrapper and storing the handle (e.g. the parent task
                // was cancelled mid-launch): there is no handle to drop,
                // but the registry entry still must not leak.
                tracing::warn!(
                    fork_id = %fork_id,
                    "fork: handle-installed ack dropped before launch completed; \
                     reclaiming without a stored handle",
                );
            }
            reclaim_delivered_child(&agent_registry, &handshake.handles, fork_id);
        }
    });

    AgentHandle {
        agent_id: fork_id,
        status_rx,
        inbound_tx,
        wake_tx: mpsc::channel(1).0,
        wake_pending: Arc::new(AtomicBool::new(false)),
        cancel,
        join_handle,
        event_store: handle_store,
        branch_metadata: ChildBranchMetadata {
            child_agent_id: fork_id,
            parent_agent_id: parent_id,
            profile_name: None,
            spawned_at: Utc::now(),
        },
    }
}
