//! Internal pipeline used by [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the helpers that build the fork's per-child
//! [`ToolContext`](crate::tool::context::ToolContext), branch the
//! [`SessionTree`](crate::session::tree::SessionTree) when one is published
//! (R4), filter the parent registry's tool definitions through the per-fork
//! allow-list (R8), and drive the `tokio::spawn` launch / completion
//! transitions (R1, R4). The child-store seeding step (R2) lives in
//! [`super::fork_seed`]. Lives next to the public tool surface so
//! [`crate::tools::agent::fork_tool::ForkTool::execute`] reads top-to-bottom
//! while staying inside the per-file 500-line production-code limit (CO5).

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::{AgentHandle, AgentHandles, ChildBranchMetadata, SharedSessionTree};
use super::infra::{AgentToolInfra, ParentGrant, SubAgentExecutor};
use super::lifecycle::{LifecycleEmitter, SubagentCompletion};
use super::reclaim::{
    ReclaimHandshake, log_terminal_transition_violation, reclaim_delivered_child,
};
use super::spawn_context::wire_child_action_log;
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::fork::{ContextFilter, ParentSystemInstruction};
use crate::agent::message_router::MessageRouter;
use crate::agent::output::AgentStopReason;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::config::permissions::PermissionPolicy;
use crate::error::{NornError, SessionError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::internal::extraction::SharedProvider;
use crate::r#loop::inbound::{InboundChannel, InboundSender};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentLoopConfig, AgentStepRequest, AgentStepResult, run_agent_step};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;
use crate::session::tree::{BranchConfig, SessionId, SessionMetadata, SessionStatus};
use crate::tool::catalog::SharedToolCatalog;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::scheduling::ToolEffectIndex;
use crate::tools::task::SharedTaskStore;

/// Construct the per-fork [`ToolContext`](crate::tool::context::ToolContext) (R3).
///
/// Fresh [`AgentToolInfra`] carrying the child's own `agent_id` / `parent_id`
/// and its own [`EventStore`], plus a fresh [`AgentHandles`] so the fork can
/// spawn grandchildren in turn. Shared infrastructure is forwarded from the
/// parent context so tasks, tool discovery, the parent's base system
/// instruction, and any orchestrator-published [`SharedSessionTree`] stay
/// reachable from inside the fork.
///
/// The consent-boundary [`PermissionPolicy`] and the scheduling
/// [`ToolEffectIndex`] are likewise forwarded: the fork's agent loop
/// resolves both from *its own* executor's shared context, so omitting
/// them here would let a fork evade every deny/ask rule the parent is
/// subject to (and lose effect-based batch scheduling).
///
/// The parent's workspace-confinement root (a plain [`ToolContext`] field,
/// not an extension) is forwarded via
/// [`ToolContext::confine_to_workspace`] for the same reason: the fork's
/// file tools check confinement against the *fork's* dispatch context, so
/// dropping the root would let a confined parent escape its sandbox simply
/// by forking. The fork's working dir is its **own** [`SharedWorkingDir`]
/// handle seeded from the parent's *current* working dir — snapshot
/// semantics, matching [`SharedWorkingDir`]'s documented fork contract:
/// forks run concurrently with the parent, so sharing the live handle
/// would let a fork's bash `cd` move the parent's (and every sibling's)
/// working dir mid-turn.
///
/// The parent's shared
/// [`HookRegistry`](crate::integration::hooks::HookRegistry) extension is
/// forwarded so the fork's own spawn/fork sites (grandchildren) observe
/// the same operator hooks; [`ForkTool::execute`] separately installs the
/// registry on the fork's `LoopContext` so pre/post-tool hooks fire for
/// the fork's own calls.
///
/// `child_policy` is the [`ChildPolicy`] the parent grants this fork (read
/// from the parent's [`CoordinationEnvelope`] by the fork tool): it is
/// stamped on the fork's [`AgentToolInfra`] together with the parent's
/// event store, so `send_message` enforces the granted messaging scope and
/// writes the dual-store `Sent` audit from ground truth carried on the
/// fork's own context. The parent's [`CoordinationEnvelope`] extension is
/// forwarded so the fork's own spawn sites can read policy at any depth.
///
/// [`ForkTool::execute`]: crate::tools::agent::fork_tool::ForkTool
pub(super) fn build_fork_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
    child_policy: ChildPolicy,
) -> Arc<ToolContext> {
    let child_log_store = Arc::clone(&child_store);
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        router: Arc::clone(&parent_infra.router),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
        grant: Some(ParentGrant {
            policy: child_policy,
            parent_store: Arc::clone(&parent_infra.event_store),
        }),
        tool_registry: parent_infra.tool_registry.as_ref().map(Arc::clone),
    };

    let mut child_ctx =
        ToolContext::with_working_dir(SharedWorkingDir::new(parent_ctx.working_dir()));
    if let Some(root) = parent_ctx.workspace_root() {
        child_ctx.confine_to_workspace(root.to_path_buf());
    }
    child_ctx.insert_extension(Arc::new(child_infra));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    if let Some(task_store) = parent_ctx.get_extension::<SharedTaskStore>() {
        child_ctx.insert_extension(task_store);
    }
    if let Some(catalog) = parent_ctx.get_extension::<SharedToolCatalog>() {
        child_ctx.insert_extension(catalog);
    }
    if let Some(diagnostics) = parent_ctx.get_extension::<DiagnosticCollector>() {
        child_ctx.insert_extension(diagnostics);
    }
    if let Some(sp) = parent_ctx.get_extension::<SharedProvider>() {
        child_ctx.insert_extension(sp);
    }
    if let Some(parent_base) = parent_ctx.get_extension::<ParentSystemInstruction>() {
        child_ctx.insert_extension(parent_base);
    }
    if let Some(policy) = parent_ctx.get_extension::<PermissionPolicy>() {
        child_ctx.insert_extension(policy);
    }
    if let Some(effects) = parent_ctx.get_extension::<ToolEffectIndex>() {
        child_ctx.insert_extension(effects);
    }
    if let Some(hooks) = parent_ctx.get_extension::<HookRegistry>() {
        child_ctx.insert_extension(hooks);
    }
    if let Some(ch) =
        parent_ctx.get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
    {
        child_ctx.insert_extension(ch);
    }
    if let Some(envelope) = parent_ctx.get_extension::<CoordinationEnvelope>() {
        child_ctx.insert_extension(envelope);
    }
    if let Some(tree) = child_tree {
        child_ctx.insert_extension(Arc::new(tree));
    }
    // Per-agent action log + session log-tree registration: the fork's
    // log starts empty at the fork point (its seeded conversation is its
    // memory; its action log records what *it* did). See
    // [`wire_child_action_log`].
    wire_child_action_log(
        parent_infra,
        parent_ctx,
        child_id,
        child_log_store,
        &child_ctx,
    );
    Arc::new(child_ctx)
}

/// Resolved child store, optional tree handle, and optional session id.
pub(super) type ForkStoreResolution = (
    Arc<EventStore>,
    Option<SharedSessionTree>,
    Option<SessionId>,
);

/// Resolve the fork's child [`EventStore`] and (when published) the child's
/// own [`SharedSessionTree`] handle (R4).
///
/// When a [`SharedSessionTree`] is present, `tree.branch()` seeds the child
/// store with the full parent context, so the caller must NOT re-seed
/// those events. The returned `tree_seeded` flag signals this to
/// [`super::fork_seed::seed_fork_events`].
pub(super) fn resolve_fork_store(
    parent_ctx: &ToolContext,
    model: &str,
) -> Result<(ForkStoreResolution, bool), NornError> {
    let Some(parent_tree) = parent_ctx.get_extension::<SharedSessionTree>() else {
        return Ok(((Arc::new(EventStore::new()), None, None), false));
    };

    let branch_config = BranchConfig {
        context_filter: ContextFilter::default(),
        metadata: SessionMetadata {
            created_at: Utc::now(),
            model: model.to_owned(),
            role: Some(format!("fork/{model}")),
            status: SessionStatus::Active,
        },
    };
    let child_session_id = parent_tree
        .tree
        .branch(parent_tree.session_id, branch_config)?;
    let store = parent_tree
        .tree
        .get_store(child_session_id)
        .ok_or_else(|| {
            NornError::Session(SessionError::StorageError {
                reason: "fork: branched session id missing from session tree".to_owned(),
            })
        })?;
    let child_tree = SharedSessionTree {
        tree: Arc::clone(&parent_tree.tree),
        session_id: child_session_id,
    };
    Ok(((store, Some(child_tree), Some(child_session_id)), true))
}

/// Outcome bundle the fork's `tokio::spawn` task hands back to the parent's
/// timeline and result channel.
pub(crate) struct ForkOutcome {
    pub(crate) status: AgentStatus,
    pub(crate) result_summary: serde_json::Value,
    pub(crate) usage: Usage,
    pub(crate) duration: std::time::Duration,
    pub(crate) error_message: Option<String>,
    /// Typed stop reason when the fork's run stopped early without
    /// completing; `None` on completion or hard error.
    pub(crate) stop: Option<AgentStopReason>,
}

/// Project the agent loop's result into a transport-friendly payload.
///
/// Only [`AgentStepResult::Completed`] is a success. `SchemaUnreachable`,
/// `MaxIterationsReached`, `TimedOut`, `Cancelled`, and `Truncated`
/// children surface as failures with an explanatory `error_message` and the
/// typed [`AgentStopReason`] — the parent must never read a bailed-out fork
/// as a completed one. Partial output (best schema attempt, pre-timeout
/// text, pre-truncation text) is preserved on `result_summary` for the
/// parent's `ForkComplete` audit event.
///
/// Pure — the registry transition lives in [`mark_fork_terminal`] so the
/// wrapper can fire `SubagentHook::on_subagent_stop` between projection
/// and marking (a hook Block suppresses the transition, mirroring spawn).
pub(super) fn project_fork_outcome(
    outcome: Result<AgentStepResult, NornError>,
    started: Instant,
) -> ForkOutcome {
    let duration = started.elapsed();
    match outcome {
        Ok(result) => classify_step_result(result, duration),
        // Hard error: `run_agent_step`'s `Err` path carries no usage, so
        // tokens consumed before a mid-run error are unrecoverable here —
        // `Usage::default()` means "unknown", not "none consumed" (same
        // limitation as `extract_outcome_summary` on the spawn side; the
        // `ForkComplete` event and lifecycle `Completed` inherit it).
        Err(err) => ForkOutcome {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage: Usage::default(),
            duration,
            error_message: Some(err.to_string()),
            stop: None,
        },
    }
}

/// Apply the fork's terminal registry transition for `status`
/// (`Completed` walks Completing → Completed; anything else marks
/// `Failed`).
///
/// The wrapper is the sole owner of a live fork's terminal transition
/// (see [`super::reclaim`]), so a transition failure here is an
/// invariant violation: it is logged loudly via
/// [`log_terminal_transition_violation`] but never propagated — the
/// wrapper still owes result delivery.
pub(super) fn mark_fork_terminal(
    registry: &RwLock<AgentRegistry>,
    fork_id: Uuid,
    status: AgentStatus,
) {
    let mut reg = registry.write();
    if status == AgentStatus::Completed {
        if let Err(e) = reg.mark_completing(fork_id) {
            log_terminal_transition_violation(&reg, fork_id, "fork", &e);
        }
        if let Err(e) = reg.mark_completed(fork_id) {
            log_terminal_transition_violation(&reg, fork_id, "fork", &e);
        }
    } else if let Err(e) = reg.mark_failed(fork_id) {
        log_terminal_transition_violation(&reg, fork_id, "fork", &e);
    }
}

/// Project a fork task that never produced an outcome — its inner `tokio`
/// task panicked or was aborted, surfacing as a
/// [`tokio::task::JoinError`] — onto the failure payload the wrapper
/// delivers. Mirrors `panicked_outcome_summary` on the spawn side: the
/// wrapper still appends `ForkComplete`, emits the lifecycle `Completed`,
/// delivers the result, and transitions the registry, so observers never
/// see a dangling `Started`. Usage is [`Usage::default`] (unknown — the
/// panicked task took its accumulated usage with it).
fn panicked_fork_outcome(
    join_error: &tokio::task::JoinError,
    duration: std::time::Duration,
) -> ForkOutcome {
    ForkOutcome {
        status: AgentStatus::Failed,
        result_summary: serde_json::Value::Null,
        usage: Usage::default(),
        duration,
        error_message: Some(format!(
            "fork task terminated without an outcome (panicked or aborted): {join_error}"
        )),
        stop: None,
    }
}

/// Map an [`AgentStepResult`] onto the fork's terminal [`ForkOutcome`]
/// projection. Pure — no registry side effects — so every variant's mapping
/// is unit-testable.
fn classify_step_result(result: AgentStepResult, duration: std::time::Duration) -> ForkOutcome {
    let project = |status: AgentStatus,
                   result_summary: serde_json::Value,
                   usage: Usage,
                   error_message: Option<String>,
                   stop: Option<AgentStopReason>| ForkOutcome {
        status,
        result_summary,
        usage,
        duration,
        error_message,
        stop,
    };
    match result {
        AgentStepResult::Completed { output, usage } => {
            project(AgentStatus::Completed, output, usage, None, None)
        }
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
        } => project(
            AgentStatus::Failed,
            best_attempt.unwrap_or(serde_json::Value::Null),
            usage,
            Some(format!(
                "fork could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            )),
            Some(AgentStopReason::SchemaUnreachable {
                validation_errors,
                attempts,
            }),
        ),
        AgentStepResult::MaxIterationsReached { usage } => project(
            AgentStatus::Failed,
            serde_json::Value::Null,
            usage,
            Some("fork reached its max-iterations cap before completing its task".to_owned()),
            Some(AgentStopReason::MaxIterationsReached),
        ),
        AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
        } => project(
            AgentStatus::Failed,
            partial_output.unwrap_or(serde_json::Value::Null),
            usage,
            Some(format!(
                "fork timed out after {:.1}s ({iterations} iterations completed); any partial \
                 output is recorded on the fork's session branch",
                elapsed.as_secs_f64(),
            )),
            Some(AgentStopReason::TimedOut {
                elapsed,
                iterations,
            }),
        ),
        AgentStepResult::Cancelled { usage } => project(
            AgentStatus::Failed,
            serde_json::Value::Null,
            usage,
            Some("fork was cancelled before completing its task".to_owned()),
            Some(AgentStopReason::Cancelled),
        ),
        AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
        } => project(
            AgentStatus::Failed,
            partial_text.map_or(serde_json::Value::Null, serde_json::Value::String),
            usage,
            Some(format!(
                "fork output was truncated ({}) before it completed its task; the partial \
                 output is recorded on the fork's session branch",
                kind.as_str(),
            )),
            Some(AgentStopReason::Truncated { kind, iterations }),
        ),
    }
}

/// Append a [`SessionEvent::ForkComplete`] to the parent's store (R4).
///
/// Best-effort: a failure here is logged but does not propagate. The fork's
/// own audit trail already lives on its branch — this event is the
/// completion reference on the parent's timeline.
pub(super) fn append_fork_complete(
    parent_store: &EventStore,
    forked_session_id: Option<SessionId>,
    outcome: &ForkOutcome,
    fork_id: Uuid,
) {
    let event = SessionEvent::ForkComplete {
        base: EventBase::new(parent_store.last_event_id()),
        forked_session_id: forked_session_id
            .map_or_else(|| fork_id.to_string(), |id| id.to_string()),
        result_summary: outcome.result_summary.clone(),
        usage: EventUsage {
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cache_read_tokens: outcome.usage.cache_read_tokens,
            cache_write_tokens: outcome.usage.cache_write_tokens,
            cost_usd: outcome.usage.cost_usd,
        },
        duration_ms: u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
    };
    if let Err(e) = parent_store.append(event) {
        tracing::warn!(
            fork_id = %fork_id,
            error = %e,
            "fork: failed to append ForkComplete event to parent store",
        );
    }
}

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
    } = launch;

    let handle_store = Arc::clone(&child_store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    // Route registration ownership (Wave 3 §Routing): registered before
    // the task starts so `send_message` can reach the fork for its entire
    // run; deregistered by the completion wrapper below — single
    // ownership, never two actors.
    router.register(fork_id, inbound_tx.clone());
    let agent_role = format!("fork/{model}");
    // Cooperative cancellation: the trigger lives on the parent-held
    // AgentHandle and a clone rides into the inner run's AgentStepRequest,
    // so `close_agent` can terminate the run itself — not just the wrapper
    // task. The loop observes the token at the top of every iteration and
    // races it (cancel-priority) against the in-flight provider call,
    // returning `AgentStepResult::Cancelled`, which the wrapper records as
    // the run's real outcome through its normal terminal sequence below.
    // Mirrors the spawn wrapper.
    let cancel = tokio_util::sync::CancellationToken::new();
    let run_cancel = cancel.clone();

    let join_handle = tokio::spawn(async move {
        let started = Instant::now();
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
            run_agent_step(AgentStepRequest {
                provider: provider.as_ref(),
                executor: &executor,
                store: child_store.as_ref(),
                user_prompt: &request,
                tools: &tool_defs,
                output_schema: Some(&output_schema),
                model: &model,
                config: &AgentLoopConfig::default(),
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
                project_fork_outcome(step_result, started)
            }
            Err(join_error) => {
                tracing::error!(
                    fork_id = %fork_id,
                    role = %agent_role,
                    error = %join_error,
                    "fork: child task panicked or was aborted before completing",
                );
                panicked_fork_outcome(&join_error, started.elapsed())
            }
        };

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
            };
            if let Err(e) = sender.0.send(result).await {
                tracing::error!(
                    fork_id = %fork_id,
                    error = %e,
                    "fork: failed to send result through child result channel",
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::fork::format_fork_outcome;
    use crate::tool::registry::ToolRegistry;

    /// Documented-proposal policy used by tests — a deliberate test-caller
    /// choice, never a library default.
    fn test_policy() -> ChildPolicy {
        use crate::agent::child_policy::{DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
        }
    }

    /// Permission-escape regression (blocker): the consent-boundary
    /// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must
    /// be forwarded from the parent's context into the fork's context —
    /// the fork loop resolves both from its own executor's shared
    /// context, so a missing forward disables enforcement entirely.
    #[tokio::test]
    async fn fork_context_forwards_permission_policy_and_effect_index() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
        };
        let parent_ctx = ToolContext::empty();
        let policy = Arc::new(PermissionPolicy::from_patterns(&["bash"], &[], &[]));
        let effects = Arc::new(ToolEffectIndex::new());
        parent_ctx.insert_extension(Arc::clone(&policy));
        parent_ctx.insert_extension(Arc::clone(&effects));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );

        let forwarded_policy = child_ctx
            .get_extension::<PermissionPolicy>()
            .ok_or("PermissionPolicy must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded_policy, &policy) {
            return Err("the fork must share the parent's policy instance".to_owned());
        }
        let forwarded_effects = child_ctx
            .get_extension::<ToolEffectIndex>()
            .ok_or("ToolEffectIndex must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded_effects, &effects) {
            return Err("the fork must share the parent's effect index instance".to_owned());
        }
        Ok(())
    }

    /// Confinement-escape regression (blocker): `workspace_root` is a
    /// plain field on [`ToolContext`] — not an extension — so
    /// `build_fork_context` must forward it explicitly, and the fork's
    /// working dir must be seeded from the parent's *current* working
    /// dir on the fork's own handle (snapshot semantics), never from
    /// the process CWD.
    #[test]
    fn fork_context_forwards_workspace_root_and_snapshots_working_dir() -> Result<(), String> {
        use std::path::{Path, PathBuf};

        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;
        use crate::tool::context::SharedWorkingDir;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
        };
        let mut parent_ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from(
            "/tmp/fork-parent-wd",
        )));
        parent_ctx.confine_to_workspace(PathBuf::from("/tmp/fork-workspace-root"));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );

        if child_ctx.workspace_root() != Some(Path::new("/tmp/fork-workspace-root")) {
            return Err(format!(
                "the fork must carry the parent's confinement root, got {:?}",
                child_ctx.workspace_root(),
            ));
        }
        if child_ctx.working_dir().as_path() != Path::new("/tmp/fork-parent-wd") {
            return Err(format!(
                "the fork's working dir must be seeded from the parent's current dir, got {}",
                child_ctx.working_dir().display(),
            ));
        }

        // Snapshot semantics: the fork owns its handle, so a fork-side
        // `cd` must not move the parent's working dir.
        child_ctx.set_working_dir(PathBuf::from("/tmp/fork-child-moved"));
        if parent_ctx.working_dir().as_path() != Path::new("/tmp/fork-parent-wd") {
            return Err("fork working-dir mutations must not propagate to the parent".to_owned());
        }
        Ok(())
    }

    /// Hook-coverage regression: the parent's shared
    /// [`HookRegistry`](crate::integration::hooks::HookRegistry)
    /// extension must be forwarded to the fork's context so the fork's
    /// own spawn/fork sites (grandchildren) can reach it.
    #[test]
    fn fork_context_forwards_hook_registry_extension() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::integration::hooks::HookRegistry;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
        };
        let parent_ctx = ToolContext::empty();
        let hooks = Arc::new(HookRegistry::new());
        parent_ctx.insert_extension(Arc::clone(&hooks));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );

        let forwarded = child_ctx
            .get_extension::<HookRegistry>()
            .ok_or("HookRegistry must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded, &hooks) {
            return Err("the fork must share the parent's hook registry instance".to_owned());
        }
        Ok(())
    }

    /// Reserve and confirm a fork entry, returning the shared registry and id.
    fn registry_with_fork() -> Result<(Arc<RwLock<AgentRegistry>>, Uuid), String> {
        let registry = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
            &registry,
            "/fork/test".to_owned(),
            "fork".to_owned(),
            "haiku".to_owned(),
            None,
        )
        .map_err(|e| format!("reserve: {e}"))?;
        let id = guard.id();
        guard.confirm().map_err(|e| format!("confirm: {e}"))?;
        Ok((registry, id))
    }

    fn finish(result: AgentStepResult) -> Result<ForkOutcome, String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = project_fork_outcome(Ok(result), Instant::now());
        mark_fork_terminal(&registry, fork_id, outcome.status);
        // Terminal transitions free the path and leave the entry observable
        // (terminal status) until an observer reclaims it (fix 10).
        let status = registry
            .read()
            .get(fork_id)
            .ok_or("terminal fork entry must stay observable until reclaimed")?
            .status;
        if !status.is_terminal() {
            return Err(format!("fork entry must be terminal, got {status:?}"));
        }
        if !registry.write().remove_terminal(fork_id) {
            return Err("terminal fork entry must be reclaimable".to_owned());
        }
        Ok(outcome)
    }

    /// Assert the outcome maps to a non-success the parent can see.
    fn assert_failure(outcome: &ForkOutcome, expected_fragment: &str) -> Result<(), String> {
        if outcome.status != AgentStatus::Failed {
            return Err(format!("expected Failed status, got {:?}", outcome.status));
        }
        let error = outcome
            .error_message
            .as_deref()
            .ok_or("failure outcome must carry an error message")?;
        if !error.contains(expected_fragment) {
            return Err(format!(
                "error '{error}' must mention '{expected_fragment}'"
            ));
        }
        let (succeeded, message, channel_error) = format_fork_outcome(Uuid::new_v4(), outcome, &[]);
        if succeeded {
            return Err("the result-channel projection must report non-success".to_owned());
        }
        if channel_error.is_none() {
            return Err("the result-channel projection must carry the error".to_owned());
        }
        if !message.contains("FORK FAILED") {
            return Err(format!(
                "parent-visible message must say FORK FAILED: {message}"
            ));
        }
        Ok(())
    }

    /// Fix 6: `Completed` is the only success.
    #[test]
    fn finish_fork_completed_is_success() -> Result<(), String> {
        let outcome = finish(AgentStepResult::Completed {
            output: serde_json::json!({"response": "done", "requirements": {}}),
            usage: Usage::default(),
        })?;
        if outcome.status != AgentStatus::Completed {
            return Err(format!("expected Completed, got {:?}", outcome.status));
        }
        if outcome.error_message.is_some() {
            return Err("success must not carry an error message".to_owned());
        }
        let (succeeded, _, error) = format_fork_outcome(Uuid::new_v4(), &outcome, &[]);
        if !succeeded || error.is_some() {
            return Err("completed fork must project as success".to_owned());
        }
        Ok(())
    }

    /// Fix 6: `MaxIterationsReached` surfaces as non-success.
    #[test]
    fn finish_fork_max_iterations_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
        })?;
        assert_failure(&outcome, "max-iterations")
    }

    /// Fix 6: `SchemaUnreachable` surfaces as non-success while preserving
    /// the best attempt for the parent's audit event.
    #[test]
    fn finish_fork_schema_unreachable_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::SchemaUnreachable {
            best_attempt: Some(serde_json::json!({"response": "almost"})),
            validation_errors: vec!["missing field `requirements`".to_owned()],
            attempts: 3,
            usage: Usage::default(),
        })?;
        assert_failure(&outcome, "schema-valid")?;
        if outcome.result_summary.get("response").is_none() {
            return Err("best attempt must be preserved on the result summary".to_owned());
        }
        let error = outcome.error_message.as_deref().unwrap_or_default();
        if !error.contains("missing field `requirements`") {
            return Err(format!("validation errors must surface: {error}"));
        }
        Ok(())
    }

    /// Fix 6: `TimedOut` surfaces as non-success while preserving partial
    /// output, accumulated usage, and the typed stop reason.
    #[test]
    fn finish_fork_timed_out_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_secs(30),
            iterations: 4,
            partial_output: Some(serde_json::json!("partial text")),
            usage: Usage {
                input_tokens: 50,
                ..Usage::default()
            },
        })?;
        assert_failure(&outcome, "timed out")?;
        if outcome.result_summary != serde_json::json!("partial text") {
            return Err("partial output must be preserved on the result summary".to_owned());
        }
        if outcome.usage.input_tokens != 50 {
            return Err("timed-out usage must be preserved on the fork outcome".to_owned());
        }
        if outcome.stop
            != Some(AgentStopReason::TimedOut {
                elapsed: std::time::Duration::from_secs(30),
                iterations: 4,
            })
        {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// Fix 6: `Cancelled` surfaces as non-success with the typed stop
    /// reason.
    #[test]
    fn finish_fork_cancelled_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::Cancelled {
            usage: Usage::default(),
        })?;
        assert_failure(&outcome, "cancelled")?;
        if outcome.stop != Some(AgentStopReason::Cancelled) {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// A truncated fork (max-tokens / content-filter stop) surfaces as
    /// non-success while preserving the partial text, usage, and the typed
    /// stop reason — never as a completed fork.
    #[test]
    fn finish_fork_truncated_is_failure() -> Result<(), String> {
        use crate::r#loop::config::TruncationKind;
        let outcome = finish(AgentStepResult::Truncated {
            kind: TruncationKind::ContentFilter,
            partial_text: Some("cut short".to_owned()),
            iterations: 2,
            usage: Usage {
                output_tokens: 9,
                ..Usage::default()
            },
        })?;
        assert_failure(&outcome, "truncated")?;
        if outcome.result_summary != serde_json::json!("cut short") {
            return Err("partial text must be preserved on the result summary".to_owned());
        }
        if outcome.usage.output_tokens != 9 {
            return Err("truncated usage must be preserved on the fork outcome".to_owned());
        }
        if outcome.stop
            != Some(AgentStopReason::Truncated {
                kind: TruncationKind::ContentFilter,
                iterations: 2,
            })
        {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// A loop error keeps the pre-existing failure mapping.
    #[test]
    fn finish_fork_loop_error_is_failure() -> Result<(), String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = project_fork_outcome(
            Err(NornError::Session(SessionError::StorageError {
                reason: "disk gone".to_owned(),
            })),
            Instant::now(),
        );
        mark_fork_terminal(&registry, fork_id, outcome.status);
        let status = registry
            .read()
            .get(fork_id)
            .ok_or("failed fork entry must stay observable until reclaimed")?
            .status;
        if status != AgentStatus::Failed {
            return Err(format!("fork entry must be Failed, got {status:?}"));
        }
        assert_failure(&outcome, "disk gone")
    }

    /// A panicked/aborted fork task projects onto an honest failure
    /// payload: Failed status, an error naming the missing outcome, no
    /// stop reason, and unknown (zero) usage — so the wrapper's
    /// `ForkComplete` / lifecycle / result-channel obligations are all
    /// satisfiable after a dependency panic.
    #[tokio::test]
    #[allow(clippy::panic, clippy::expect_used)]
    async fn panicked_fork_outcome_reports_honest_failure() -> Result<(), String> {
        let join_error = tokio::spawn(async { panic!("dependency exploded") })
            .await
            .expect_err("task must panic");
        let outcome = panicked_fork_outcome(&join_error, std::time::Duration::from_millis(7));
        if outcome.status != AgentStatus::Failed {
            return Err(format!("expected Failed, got {:?}", outcome.status));
        }
        let error = outcome
            .error_message
            .as_deref()
            .ok_or("panic outcome must carry an error message")?;
        if !error.contains("terminated without an outcome") {
            return Err(format!("error must name the missing outcome: {error}"));
        }
        if outcome.stop.is_some() {
            return Err("a panic is not a typed early stop".to_owned());
        }
        if outcome.usage.input_tokens != 0 || outcome.usage.output_tokens != 0 {
            return Err("usage is unknown after a panic — must be zeros".to_owned());
        }
        if outcome.result_summary != serde_json::Value::Null {
            return Err("no result summary exists after a panic".to_owned());
        }
        assert_failure(&outcome, "terminated without an outcome")
    }
}
