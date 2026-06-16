//! Internal pipeline used by [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the helpers that build the fork's per-child
//! [`ToolContext`](crate::tool::context::ToolContext), branch the
//! [`SessionTree`](crate::session::tree::SessionTree) when one is published
//! (R4), filter the parent registry's tool definitions through the per-fork
//! allow-list (R8), and project the run's outcome (R1, R4). The
//! `tokio::spawn` launch / completion wrapper lives in
//! [`super::fork_launch`]; the child-store seeding step (R2) lives in
//! [`super::fork_seed`]. Lives next to the public tool surface so
//! [`crate::tools::agent::fork_tool::ForkTool::execute`] reads top-to-bottom
//! while staying inside the per-file 500-line production-code limit (CO5).

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use parking_lot::RwLock;
use uuid::Uuid;

use super::handle::{AgentHandles, AgentWakeRegistry, SharedSessionTree};
use super::infra::{AgentCancellation, AgentToolInfra, ParentGrant};
use super::reclaim::log_terminal_transition_violation;
use super::spawn_context::wire_child_action_log;
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::fork::{ContextFilter, ParentSystemInstruction};
use crate::agent::output::AgentStopReason;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::config::permissions::PermissionPolicy;
use crate::error::{NornError, SessionError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
use crate::r#loop::runner::AgentStepResult;
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
/// `child_policy` is the [`ChildPolicy`] the parent grants this fork —
/// computed by the fork tool from the parent's own grant (narrowed or
/// inherit-with-decrement, W3.4): it is stamped on the fork's
/// [`AgentToolInfra`] together with the parent's event store, so
/// `signal_agent` enforces the granted messaging scope, the dual-store
/// `Sent` audit writes from ground truth, and the fork's own spawn/fork
/// sites read *their* budget from the grant. The parent's
/// [`CoordinationEnvelope`] extension is forwarded for the envelope-wide
/// `child_result_capacity`; the
/// [`ReclaimOnResultDelivery`](super::reclaim::ReclaimOnResultDelivery)
/// marker is forwarded so the fork's own children are reclaimed at every
/// level exactly as depth-1 children are.
///
/// `child_cancel` is the fork's own run-cancellation token — created by
/// the fork tool as a child of the forker's published
/// [`AgentCancellation`] (or free-standing when the forker publishes
/// none; see [`AgentCancellation`] for the root boundary) — published on
/// the fork's context here so the fork's own spawn/fork sites chain
/// grandchild tokens under it (W3.5 cancellation cascade).
///
/// [`ForkTool::execute`]: crate::tools::agent::fork_tool::ForkTool
pub(super) fn build_fork_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
    child_policy: ChildPolicy,
    child_cancel: tokio_util::sync::CancellationToken,
) -> Arc<ToolContext> {
    let child_log_store = Arc::clone(&child_store);
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        router: Arc::clone(&parent_infra.router),
        pending_messages: Arc::clone(&parent_infra.pending_messages),
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
    child_ctx.insert_extension(Arc::new(AgentCancellation(child_cancel)));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    if let Some(wake_registry) = parent_ctx.get_extension::<AgentWakeRegistry>() {
        child_ctx.insert_extension(wake_registry);
    }
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
    if let Some(marker) = parent_ctx.get_extension::<super::reclaim::ReclaimOnResultDelivery>() {
        child_ctx.insert_extension(marker);
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
    /// Summed `subtree_usage` of every grandchild result the fork's loop
    /// delivered (W3.6 usage rollup) — from the
    /// [`AgentStepResult`] arm on every loop outcome, and from the
    /// wrapper's shared
    /// [`ChildrenUsage`](crate::r#loop::children_usage::ChildrenUsage)
    /// snapshot on the hard-error and panic paths. Disjoint from
    /// [`Self::usage`]: `usage + children_usage` is the fork's subtree
    /// total with each agent counted exactly once.
    pub(crate) children_usage: Usage,
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
///
/// `delivered_children_usage` is the wrapper's snapshot of the fork's
/// shared [`ChildrenUsage`](crate::r#loop::children_usage::ChildrenUsage)
/// accumulator, used only on the hard-error arm where no step result
/// exists to carry `children_usage` out of the loop (W3.6); every `Ok`
/// arm reads the authoritative value from the step result.
pub(super) fn project_fork_outcome(
    outcome: Result<AgentStepResult, NornError>,
    started: Instant,
    delivered_children_usage: Usage,
) -> ForkOutcome {
    let duration = started.elapsed();
    match outcome {
        Ok(result) => classify_step_result(result, duration),
        // Hard error: `run_agent_step`'s `Err` path carries no usage, so
        // tokens the fork itself consumed before a mid-run error are
        // unrecoverable here — `Usage::default()` means "unknown", not
        // "none consumed" (same limitation as `extract_outcome_summary`
        // on the spawn side; the `ForkComplete` event and lifecycle
        // `Completed` inherit it). Delivered grandchild subtrees survive
        // on the shared accumulator and are folded in (W3.6).
        Err(err) => ForkOutcome {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage: Usage::default(),
            children_usage: delivered_children_usage,
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
/// see a dangling `Started`. Own usage is [`Usage::default`] (unknown —
/// the panicked task took its accumulated usage with it), while
/// `delivered_children_usage` — the wrapper's snapshot of the shared
/// [`ChildrenUsage`](crate::r#loop::children_usage::ChildrenUsage)
/// accumulator, which survives the unwound task — still carries every
/// grandchild subtree the fork's loop delivered before the panic (W3.6).
pub(super) fn panicked_fork_outcome(
    join_error: &tokio::task::JoinError,
    duration: std::time::Duration,
    delivered_children_usage: Usage,
) -> ForkOutcome {
    ForkOutcome {
        status: AgentStatus::Failed,
        result_summary: serde_json::Value::Null,
        usage: Usage::default(),
        children_usage: delivered_children_usage,
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
    struct Projection {
        status: AgentStatus,
        result_summary: serde_json::Value,
        usage: Usage,
        children_usage: Usage,
        error_message: Option<String>,
        stop: Option<AgentStopReason>,
    }
    let project = |p: Projection| ForkOutcome {
        status: p.status,
        result_summary: p.result_summary,
        usage: p.usage,
        children_usage: p.children_usage,
        duration,
        error_message: p.error_message,
        stop: p.stop,
    };
    match result {
        AgentStepResult::Completed {
            output,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Completed,
            result_summary: output,
            usage,
            children_usage,
            error_message: None,
            stop: None,
        }),
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: best_attempt.unwrap_or(serde_json::Value::Null),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            )),
            stop: Some(AgentStopReason::SchemaUnreachable {
                validation_errors,
                attempts,
            }),
        }),
        AgentStepResult::MaxIterationsReached {
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage,
            children_usage,
            error_message: Some(
                "fork reached its max-iterations cap before completing its task".to_owned(),
            ),
            stop: Some(AgentStopReason::MaxIterationsReached),
        }),
        AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: partial_output.unwrap_or(serde_json::Value::Null),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork timed out after {:.1}s ({iterations} iterations completed); any partial \
                 output is recorded on the fork's session branch",
                elapsed.as_secs_f64(),
            )),
            stop: Some(AgentStopReason::TimedOut {
                elapsed,
                iterations,
            }),
        }),
        AgentStepResult::Cancelled {
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage,
            children_usage,
            error_message: Some("fork was cancelled before completing its task".to_owned()),
            stop: Some(AgentStopReason::Cancelled),
        }),
        AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: partial_text.map_or(serde_json::Value::Null, serde_json::Value::String),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork output was truncated ({}) before it completed its task; the partial \
                 output is recorded on the fork's session branch",
                kind.as_str(),
            )),
            stop: Some(AgentStopReason::Truncated { kind, iterations }),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::fork::format_fork_outcome;
    use crate::provider::traits::Provider;
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
            loop_config: None,
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
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
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
            tokio_util::sync::CancellationToken::new(),
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
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
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
            tokio_util::sync::CancellationToken::new(),
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
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
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
            tokio_util::sync::CancellationToken::new(),
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
            test_policy(),
            None,
        )
        .map_err(|e| format!("reserve: {e}"))?;
        let id = guard.id();
        guard.confirm().map_err(|e| format!("confirm: {e}"))?;
        Ok((registry, id))
    }

    fn finish(result: AgentStepResult) -> Result<ForkOutcome, String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = project_fork_outcome(Ok(result), Instant::now(), Usage::default());
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage {
                input_tokens: 6,
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
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
        let outcome = panicked_fork_outcome(
            &join_error,
            std::time::Duration::from_millis(7),
            Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
        );
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
