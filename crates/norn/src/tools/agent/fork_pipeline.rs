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

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::{AgentHandle, AgentHandles, ChildBranchMetadata, SharedSessionTree};
use super::infra::{AgentToolInfra, SubAgentExecutor};
use super::reclaim::reclaim_delivered_child;
use crate::agent::fork::{ContextFilter, ParentSystemInstruction};
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::config::permissions::PermissionPolicy;
use crate::error::{NornError, SessionError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
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
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::ToolEffectIndex;
use crate::tools::task::SharedTaskStore;
use crate::tools::tool_search::SharedToolCatalog;

/// Bounded capacity of the fork's inbound steering channel — mirrors the
/// value used by [`super::spawn`] so the two surfaces behave identically.
pub(super) const FORK_INBOUND_BUFFER: usize = 32;

/// Project the parent registry's tools through the optional allow-list (R8).
pub(super) fn build_fork_tool_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<ToolDefinition> {
    let allow_set: Option<HashSet<&str>> =
        allow_list.map(|names| names.iter().map(String::as_str).collect());
    registry
        .names()
        .filter(|name| allow_set.as_ref().is_none_or(|set| set.contains(name)))
        .filter_map(|name| {
            registry.get(name).map(|tool| ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: crate::tool::wrap_schema_with_envelope(tool.input_schema()),
            })
        })
        .collect()
}

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
/// [`ForkTool::execute`]: crate::tools::agent::fork_tool::ForkTool
pub(super) fn build_fork_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
) -> Arc<ToolContext> {
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        mailbox: Arc::clone(&parent_infra.mailbox),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
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
    if let Some(tree) = child_tree {
        child_ctx.insert_extension(Arc::new(tree));
    }
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
}

/// Mark the fork's terminal registry status and project the agent loop's
/// result into a transport-friendly payload.
///
/// Only [`AgentStepResult::Completed`] is a success. `SchemaUnreachable`,
/// `MaxIterationsReached`, `TimedOut`, and `Cancelled` children surface as
/// failures with an explanatory `error_message` — the parent must never read
/// a bailed-out fork as a completed one. Partial output (best schema attempt,
/// pre-timeout text) is preserved on `result_summary` for the parent's
/// `ForkComplete` audit event.
pub(super) fn finish_fork(
    registry: &RwLock<AgentRegistry>,
    fork_id: Uuid,
    outcome: Result<AgentStepResult, NornError>,
    started: Instant,
) -> ForkOutcome {
    let duration = started.elapsed();
    let (status, result_summary, usage, error_message) = match outcome {
        Ok(result) => classify_step_result(result),
        Err(err) => (
            AgentStatus::Failed,
            serde_json::Value::Null,
            Usage::default(),
            Some(err.to_string()),
        ),
    };

    {
        let mut reg = registry.write();
        if status == AgentStatus::Completed {
            if let Err(e) = reg.mark_completing(fork_id) {
                tracing::warn!(fork_id = %fork_id, error = %e, "fork: mark_completing failed");
            }
            if let Err(e) = reg.mark_completed(fork_id) {
                tracing::warn!(fork_id = %fork_id, error = %e, "fork: mark_completed failed");
            }
        } else if let Err(e) = reg.mark_failed(fork_id) {
            tracing::warn!(fork_id = %fork_id, error = %e, "fork: mark_failed failed");
        }
    }

    ForkOutcome {
        status,
        result_summary,
        usage,
        duration,
        error_message,
    }
}

/// Map an [`AgentStepResult`] onto the fork's terminal `(status, summary,
/// usage, error)` projection. Pure — no registry side effects — so every
/// variant's mapping is unit-testable.
fn classify_step_result(
    result: AgentStepResult,
) -> (AgentStatus, serde_json::Value, Usage, Option<String>) {
    match result {
        AgentStepResult::Completed { output, usage } => {
            (AgentStatus::Completed, output, usage, None)
        }
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
        } => (
            AgentStatus::Failed,
            best_attempt.unwrap_or(serde_json::Value::Null),
            usage,
            Some(format!(
                "fork could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            )),
        ),
        AgentStepResult::MaxIterationsReached { usage } => (
            AgentStatus::Failed,
            serde_json::Value::Null,
            usage,
            Some("fork reached its max-iterations cap before completing its task".to_owned()),
        ),
        AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
        } => (
            AgentStatus::Failed,
            partial_output.unwrap_or(serde_json::Value::Null),
            Usage::default(),
            Some(format!(
                "fork timed out after {:.1}s ({iterations} iterations completed); any partial \
                 output is recorded on the fork's session branch",
                elapsed.as_secs_f64(),
            )),
        ),
        AgentStepResult::Cancelled { usage } => (
            AgentStatus::Failed,
            serde_json::Value::Null,
            usage,
            Some("fork was cancelled before completing its task".to_owned()),
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
    /// and a result channel exists: after delivering the fork's result
    /// the wrapper reclaims the registry entry and drops the
    /// parent-held handle (see [`super::reclaim`]).
    pub(super) reclaim_handles: Option<Arc<AgentHandles>>,
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
        reclaim_handles,
    } = launch;

    let handle_store = Arc::clone(&child_store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let agent_role = format!("fork/{model}");

    let join_handle = tokio::spawn(async move {
        let started = Instant::now();
        let step_result = run_agent_step(AgentStepRequest {
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
            cancel: None,
        })
        .await;

        if let Err(ref e) = step_result {
            tracing::error!(
                fork_id = %fork_id,
                model = %model,
                error = %e,
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "fork: run_agent_step failed",
            );
        }

        let outcome = finish_fork(&agent_registry, fork_id, step_result, started);
        append_fork_complete(parent_store.as_ref(), forked_session_id, &outcome, fork_id);

        if let Some(sender) = result_sender {
            let (succeeded, formatted_message, error) =
                crate::agent::fork::format_fork_outcome(fork_id, &outcome, &requirement_names);
            let result = ChildAgentResult {
                agent_id: fork_id,
                agent_role,
                succeeded,
                formatted_message,
                error,
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
        // entry and the parent-held handle can go. Runs after the
        // terminal status broadcast so the fork tool's post-insert check
        // observes a consistent order. A failed result send means the
        // receiver is gone — reclaiming is still correct.
        if let Some(parent_handles) = reclaim_handles {
            reclaim_delivered_child(&agent_registry, &parent_handles, fork_id);
        }
    });

    AgentHandle {
        agent_id: fork_id,
        status_rx,
        inbound_tx,
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

    /// Permission-escape regression (blocker): the consent-boundary
    /// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must
    /// be forwarded from the parent's context into the fork's context —
    /// the fork loop resolves both from its own executor's shared
    /// context, so a missing forward disables enforcement entirely.
    #[tokio::test]
    async fn fork_context_forwards_permission_policy_and_effect_index() -> Result<(), String> {
        use crate::agent::mailbox::Mailbox;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
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

        use crate::agent::mailbox::Mailbox;
        use crate::provider::mock::MockProvider;
        use crate::tool::context::SharedWorkingDir;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
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
        use crate::agent::mailbox::Mailbox;
        use crate::integration::hooks::HookRegistry;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
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
        let outcome = finish_fork(&registry, fork_id, Ok(result), Instant::now());
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
    /// output.
    #[test]
    fn finish_fork_timed_out_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_secs(30),
            iterations: 4,
            partial_output: Some(serde_json::json!("partial text")),
        })?;
        assert_failure(&outcome, "timed out")?;
        if outcome.result_summary != serde_json::json!("partial text") {
            return Err("partial output must be preserved on the result summary".to_owned());
        }
        Ok(())
    }

    /// Fix 6: `Cancelled` surfaces as non-success.
    #[test]
    fn finish_fork_cancelled_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::Cancelled {
            usage: Usage::default(),
        })?;
        assert_failure(&outcome, "cancelled")
    }

    /// A loop error keeps the pre-existing failure mapping.
    #[test]
    fn finish_fork_loop_error_is_failure() -> Result<(), String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = finish_fork(
            &registry,
            fork_id,
            Err(NornError::Session(SessionError::StorageError {
                reason: "disk gone".to_owned(),
            })),
            Instant::now(),
        );
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
}
