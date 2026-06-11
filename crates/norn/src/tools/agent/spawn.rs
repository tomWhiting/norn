//! `SpawnAgentTool` (NA-006) — launches a sub-agent asynchronously.
//!
//! Spawn reserves a child slot in the agent registry, builds a per-child
//! [`ToolContext`] carrying the child's own identity, resolves an optional
//! profile into the child's [`LoopContext`], filters the parent registry's
//! tool definitions through the allow-list so the child model can see its
//! tools, then launches the child via [`tokio::spawn`] and returns
//! immediately. When the child reaches a terminal status the spawn wrapper
//! marks the registry, sends a `trigger_turn` notification to the parent's
//! mailbox, and updates the status watch channel that backs reactive waits.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::watch;
use uuid::Uuid;

use super::handle::{AgentHandle, AgentHandles, ChildBranchMetadata, SharedSessionTree};
use super::infra::{AgentToolInfra, SubAgentExecutor, infra_from};
use super::reclaim::{
    ReclaimOnResultDelivery, entry_terminal_or_reclaimed, reclaim_delivered_child,
};
use super::spawn_outcome::{extract_outcome_summary, mark_terminal_in_registry};
use crate::agent::fork::ContextFilter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::ChildResultSender;
use crate::config::permissions::PermissionPolicy;
use crate::error::ToolError;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::internal::extraction::SharedProvider;
use crate::r#loop::inbound::inbound_channel;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentLoopConfig, AgentStepRequest, run_agent_step};
use crate::profile::{default_scan_dirs, from_profile, resolve_profile};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::session::tree::{BranchConfig, SessionMetadata, SessionStatus};
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::{ToolEffect, ToolEffectIndex};
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::task::SharedTaskStore;
use crate::tools::tool_search::SharedToolCatalog;

/// Bounded capacity of a child's inbound steering channel.
///
/// This is a `tokio::sync::mpsc` backpressure buffer, *not* a cap on how
/// many messages a child may ever receive: when the buffer is full a
/// sender simply `await`s for space (CO1 — no hardcoded limits). 32 is the
/// standard tokio buffer size for a low-traffic control channel.
const SPAWN_INBOUND_BUFFER: usize = 32;

/// Spawns a sub-agent that runs asynchronously on its own `tokio` task.
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpawnAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SpawnAgentArgs {
    task: String,
    model: String,
    role: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    path: Option<String>,
}

/// Build the child's [`LoopContext`] and the profile-derived tool list.
///
/// When `profile_name` is `Some`, the named profile is resolved through the
/// scanner over `scan_dirs`; its system instructions, reasoning config, and
/// prompt commands flow into the returned [`LoopContext`] via
/// [`from_profile`]. The gated [`ToolRegistry`] `from_profile` produces is
/// discarded — the child shares the parent's registry — but the profile's
/// resolved tool list is returned so the caller can use it as the per-child
/// allow-list. When `profile_name` is `None`, a minimal context is built
/// with the task embedded as the system instruction.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when a named profile cannot be
/// resolved — spawn never silently falls back to a default profile.
fn build_child_loop_context(
    profile_name: Option<&str>,
    task: &str,
    scan_dirs: &[PathBuf],
) -> Result<(LoopContext, Option<Vec<String>>), ToolError> {
    if let Some(name) = profile_name {
        let profile = resolve_profile(name, scan_dirs).map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn_agent: profile '{name}' could not be resolved: {e}"),
        })?;
        let resolved_tools = profile.resolved_tools();
        let (loop_ctx, _gated) = from_profile(&profile, ToolRegistry::new(), None, None);
        Ok((loop_ctx, resolved_tools))
    } else {
        let base = format!("You are a sub-agent. Task: {task}\n\nComplete the task and stop.");
        Ok((LoopContext::new(base), None))
    }
}

/// Build the [`ToolDefinition`] slice the child model sees.
///
/// Iterates the parent registry's currently-available tools, filters them
/// through `allow_list` (the same list that gates the child's
/// [`SubAgentExecutor`]), and projects each surviving tool into a
/// [`ToolDefinition`]. When `allow_list` is `None` every available parent
/// tool is included.
fn build_tool_definitions(
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

/// Construct the per-child [`ToolContext`].
///
/// The child gets a *fresh* [`AgentToolInfra`] carrying its own
/// `agent_id` / `parent_id` and its own [`EventStore`], plus a *fresh*
/// (empty) [`AgentHandles`] so it can spawn grandchildren. The shared
/// infrastructure — [`SharedTaskStore`], [`SharedToolCatalog`],
/// [`DiagnosticCollector`] — is forwarded from the parent context so tasks
/// and tool discovery stay global across the agent tree. The
/// [`crate::agent::mailbox::Mailbox`] is shared by design, so a child's
/// send to its `parent_id` routes back to the same mailbox.
///
/// The consent-boundary [`PermissionPolicy`] and the scheduling
/// [`ToolEffectIndex`] are likewise forwarded: the child's agent loop
/// resolves both from *its own* executor's shared context, so omitting
/// them here would let a child evade every deny/ask rule the parent is
/// subject to (and lose effect-based batch scheduling).
///
/// The parent's workspace-confinement root (a plain [`ToolContext`] field,
/// not an extension) is forwarded via
/// [`ToolContext::confine_to_workspace`] for the same reason: the child's
/// file tools check confinement against the *child's* dispatch context, so
/// dropping the root would let a confined parent escape its sandbox simply
/// by spawning a child. The child's working dir is its **own**
/// [`SharedWorkingDir`] handle seeded from the parent's *current* working
/// dir — snapshot semantics, matching [`SharedWorkingDir`]'s documented
/// fork contract: children run concurrently with the parent, so sharing
/// the live handle would let a child's bash `cd` move the parent's (and
/// every sibling's) working dir mid-turn.
///
/// The parent's shared [`HookRegistry`] extension is forwarded so the
/// child's own spawn/fork sites (grandchildren) observe the same operator
/// hooks; the caller separately installs the registry on the child's
/// [`LoopContext`] so pre/post-tool hooks fire for the child's own calls.
///
/// When `child_tree` is `Some` — i.e. an orchestrator published a
/// [`SharedSessionTree`] on the parent context — it is installed on the
/// child context keyed to the *child's* `SessionId`, so a grandchild spawn
/// branches under the child's session in turn (NA-008 R3).
fn build_child_context(
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

/// Resolve the child's [`EventStore`] and, when an orchestrator published a
/// [`SharedSessionTree`], the child's own tree handle (NA-008 R3).
///
/// When `parent_ctx` carries a [`SharedSessionTree`], the child's store is
/// created as a named branch under the parent's session via
/// [`crate::session::tree::SessionTree::branch`] — which also appends a
/// `Fork` event to the parent's store — and the returned
/// [`SharedSessionTree`] is keyed to the *child's* `SessionId` so
/// grandchildren branch correctly. The branch metadata records the child's
/// model and a `spawned/{profile-or-role}` role label.
///
/// When the extension is absent (standalone mode) the child gets a fresh,
/// disconnected [`EventStore`] and no tree handle — exactly as before
/// NA-008.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when the session-tree branch
/// fails or the branched session id is unexpectedly absent from the tree.
fn resolve_child_store(
    parent_ctx: &ToolContext,
    model: &str,
    role_label: &str,
) -> Result<(Arc<EventStore>, Option<SharedSessionTree>), ToolError> {
    let Some(parent_tree) = parent_ctx.get_extension::<SharedSessionTree>() else {
        return Ok((Arc::new(EventStore::new()), None));
    };

    let branch_config = BranchConfig {
        context_filter: ContextFilter::default(),
        metadata: SessionMetadata {
            created_at: Utc::now(),
            model: model.to_owned(),
            role: Some(format!("spawned/{role_label}")),
            status: SessionStatus::Active,
        },
    };
    let child_session_id = parent_tree
        .tree
        .branch(parent_tree.session_id, branch_config)
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn_agent: session-tree branch failed: {e}"),
        })?;
    let store = parent_tree
        .tree
        .get_store(child_session_id)
        .ok_or_else(|| ToolError::ExecutionFailed {
            reason: "spawn_agent: branched session id missing from session tree".to_owned(),
        })?;
    let child_tree = SharedSessionTree {
        tree: Arc::clone(&parent_tree.tree),
        session_id: child_session_id,
    };
    Ok((store, Some(child_tree)))
}

/// Resources moved into a spawned child's `tokio` task.
struct ChildLaunch {
    /// Provider shared with the parent — children use the same provider.
    provider: Arc<dyn Provider>,
    /// The child's tool executor, owning its per-child [`ToolContext`].
    executor: SubAgentExecutor,
    /// The child's own session event store.
    store: Arc<EventStore>,
    /// The child's loop context (profile-derived or task-aware default).
    loop_ctx: LoopContext,
    /// Tool definitions the child model is shown.
    tool_defs: Vec<ToolDefinition>,
    /// The self-contained task string the child runs.
    task: String,
    /// Model identifier for the child's provider calls.
    model: String,
    /// Shared agent registry — the spawn wrapper marks terminal status here.
    agent_registry: Arc<RwLock<AgentRegistry>>,
    /// Channel sender for delivering results to the orchestrator.
    result_sender: Option<ChildResultSender>,
    /// The child's registry id.
    child_id: Uuid,
    /// Provenance metadata stored on the child's [`AgentHandle`] (NA-008 R3).
    branch_metadata: ChildBranchMetadata,
    /// Shared hook registry retrieved from the parent's
    /// [`ToolContext`]. When present, the child task fires
    /// [`SubagentHook::on_subagent_stop`](crate::integration::hooks::SubagentHook::on_subagent_stop)
    /// after [`run_agent_step`] returns; a Block suppresses the
    /// registry's Completed/Failed transition (NH-006 R5).
    hooks: Option<Arc<HookRegistry>>,
    /// Role label used as the matcher input for sub-agent hooks
    /// (profile name when supplied, otherwise the role argument). Kept
    /// on the launch so the child task does not have to recompute it.
    role_label: String,
    /// Tagged event sender for real-time observability. When `Some`,
    /// the child's [`run_agent_step`] broadcasts every `ProviderEvent`
    /// on the shared channel so the TUI activity panel shows child
    /// tool calls in real time.
    event_sender: Option<AgentEventSender>,
    /// `Some` when the runtime declared [`ReclaimOnResultDelivery`] and
    /// a result channel exists: after delivering the child's result the
    /// wrapper reclaims the registry entry and drops the parent-held
    /// handle (see [`super::reclaim`] for the ownership rule). `None`
    /// leaves both for an external observer or the handle holder.
    reclaim_handles: Option<Arc<AgentHandles>>,
}

/// Launch the child on its own `tokio` task and return the [`AgentHandle`]
/// the parent keeps.
///
/// The spawned task runs [`run_agent_step`] to completion, marks the
/// child's terminal registry status, sends the formatted result through
/// the child result channel, and updates the status watch channel.
fn launch_child(launch: ChildLaunch) -> AgentHandle {
    let ChildLaunch {
        provider,
        executor,
        store,
        mut loop_ctx,
        tool_defs,
        task,
        model,
        agent_registry,
        result_sender,
        child_id,
        branch_metadata,
        hooks,
        role_label,
        event_sender,
        reclaim_handles,
    } = launch;

    let handle_store = Arc::clone(&store);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, mut inbound_rx) = inbound_channel(SPAWN_INBOUND_BUFFER);
    let agent_role = format!("spawn/{model}");

    let join_handle = tokio::spawn(async move {
        let outcome = run_agent_step(AgentStepRequest {
            provider: provider.as_ref(),
            executor: &executor,
            store: store.as_ref(),
            user_prompt: &task,
            tools: &tool_defs,
            output_schema: None,
            model: &model,
            config: &AgentLoopConfig::default(),
            event_tx: event_sender.as_ref(),
            inbound: Some(&mut inbound_rx),
            loop_context: &mut loop_ctx,
            cancel: None,
        })
        .await;

        // NH-006 R5 / C57: fire SubagentHook::on_subagent_stop before
        // marking the registry's terminal status. A Block suppresses
        // the Completed/Failed transition (the agent stays in its
        // pre-terminal registry state) while the result-channel
        // summary still surfaces so the parent observes the child's
        // outcome. Hooks is None → marking happens unconditionally,
        // matching pre-NH-006 behaviour.
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

        let (terminal_status, output_text, error) = extract_outcome_summary(outcome);

        if !stop_blocked {
            mark_terminal_in_registry(&agent_registry, child_id, terminal_status);
        }

        if let Some(sender) = result_sender {
            let succeeded = terminal_status == AgentStatus::Completed;
            let formatted_message = if succeeded {
                crate::agent::fork::format_spawn_result(
                    child_id,
                    &agent_role,
                    output_text.as_deref().unwrap_or("(no output)"),
                )
            } else {
                crate::agent::fork::format_spawn_failure(
                    child_id,
                    &agent_role,
                    error.as_deref().unwrap_or("unknown error"),
                )
            };
            let result = crate::agent::result_channel::ChildAgentResult {
                agent_id: child_id,
                agent_role,
                succeeded,
                formatted_message,
                error,
            };
            if let Err(e) = sender.0.send(result).await {
                tracing::error!(
                    child_id = %child_id,
                    error = %e,
                    "spawn_agent: failed to send result through child result channel",
                );
            }
        }

        let _ = status_tx.send_replace(terminal_status);

        // Delivery-anchored reclamation (embedded/headless runtimes):
        // the parent's record of this child is now the delivered result,
        // so the registry entry and the parent-held handle can go. Runs
        // after the terminal status broadcast so the spawning tool's
        // post-insert check (`entry_terminal_or_reclaimed`) observes a
        // consistent order; skipped when a stop hook suppressed the
        // terminal transition (the child is then deliberately left
        // observable and non-terminal). A failed result send means the
        // receiver is gone — reclaiming is still correct, nothing can
        // observe the entry through the channel anymore.
        if !stop_blocked && let Some(parent_handles) = reclaim_handles {
            reclaim_delivered_child(&agent_registry, &parent_handles, child_id);
        }
    });

    AgentHandle {
        agent_id: child_id,
        status_rx,
        inbound_tx,
        join_handle,
        event_store: handle_store,
        branch_metadata,
    }
}

/// Public tool name for the Norn spawn delegation tool.
pub const SPAWN_TOOL_NAME: &str = "spawn_agent";

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &'static str {
        SPAWN_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/spawn_agent.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/spawn_agent.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["task", "model", "role"],
            "additionalProperties": false,
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Self-contained task description for the sub-agent. The sub-agent sees only this string — it has no access to the parent's conversation history."
                },
                "model": {
                    "type": "string",
                    "description": "Model identifier for the sub-agent (e.g. \"gpt-5.5\", \"claude-sonnet-4-5-20250514\")."
                },
                "role": {
                    "type": "string",
                    "description": "Role label recorded in the agent registry for observability (e.g. \"researcher\", \"code-reviewer\")."
                },
                "profile": {
                    "type": "string",
                    "description": "Optional bare profile name (e.g. \"developer\", \"code-reviewer\") resolved as a markdown profile from $WORKSPACE/.norn/profiles, $WORKSPACE/.meridian/profiles, or ~/.norn/profiles. Supplies the child's system instructions, tool allow-list, and reasoning config. Omit for a minimal default."
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional allow-list of tool names the sub-agent may call. Takes precedence over the profile's tool list. Omit to inherit the profile's tools, or the full parent registry when no profile is given."
                },
                "path": {
                    "type": "string",
                    "description": "Hierarchical registry path for the sub-agent (e.g. \"/workers/phase-1\"). Not a file path. Omit to auto-generate under /spawn/."
                }
            }
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let args: SpawnAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;
        let infra = infra_from(ctx)?;

        let parent_registry = infra.tool_registry.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed {
                reason: "spawn_agent requires AgentToolInfra.tool_registry to be configured; \
                         orchestrator must provide a ToolRegistry so the sub-agent has tools available"
                    .to_string(),
            }
        })?;

        // The spawning agent's `AgentHandles` extension must be installed
        // before we launch a child — otherwise the child would run
        // unobservable, with no status channel and no steering channel.
        let handles =
            ctx.get_extension::<AgentHandles>()
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason: "spawn_agent requires the AgentHandles extension on the tool context; \
                         build_runtime installs it during runtime construction"
                        .to_string(),
                })?;

        // Build the child's loop context and resolve the profile's tool
        // list. The profile scanner walks the same directories the CLI uses
        // for top-level agents, rooted at the parent agent's working
        // directory (not the process CWD, which can diverge from the
        // agent's working dir in embedded runtimes).
        let scan_dirs = default_scan_dirs(&ctx.working_dir());
        let (mut child_loop_ctx, profile_tools) =
            build_child_loop_context(args.profile.as_deref(), &args.task, &scan_dirs)?;

        // The explicit `tools` argument wins; otherwise fall back to the
        // profile's resolved tool list. The chosen list gates both the
        // child's tool execution (via `SubAgentExecutor`) and the tool
        // definitions the child model is shown.
        let allow_list: Option<Vec<String>> = args.tools.clone().or(profile_tools);
        let tool_defs = build_tool_definitions(parent_registry, allow_list.as_deref());

        let path = args
            .path
            .unwrap_or_else(|| format!("/spawn/{}", Uuid::new_v4()));

        // The session-tree role label and the audit-trail provenance both
        // prefer the profile name, falling back to the role argument when no
        // profile was given. Computed before `reserve` consumes `args.role`.
        let role_label = args.profile.clone().unwrap_or_else(|| args.role.clone());

        let guard = AgentRegistry::reserve(
            &infra.registry,
            path.clone(),
            args.role,
            args.model.clone(),
            Some(infra.agent_id),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn reservation failed: {e}"),
        })?;
        let child_id = guard.id();
        guard.confirm().map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn confirm failed: {e}"),
        })?;

        // Resolve the child's event store: a named branch under the parent's
        // session when an orchestrator published a SessionTree, otherwise a
        // standalone store (NA-008 R3). In tree mode `child_tree` carries the
        // child's own SessionId for grandchild branching.
        let (child_store, child_tree) = resolve_child_store(ctx, &args.model, &role_label)?;

        // Provenance recorded on the child's AgentHandle so the parent can
        // attribute the child's audit trail (NA-008 R3).
        let branch_metadata = ChildBranchMetadata {
            child_agent_id: child_id,
            parent_agent_id: infra.agent_id,
            profile_name: args.profile.clone(),
            spawned_at: Utc::now(),
        };

        // Per-child ToolContext: fresh identity, fresh AgentHandles, shared
        // infrastructure forwarded from the parent.
        let child_ctx =
            build_child_context(&infra, child_id, Arc::clone(&child_store), ctx, child_tree);
        let child_executor = SubAgentExecutor::new(
            Arc::clone(parent_registry),
            allow_list,
            Arc::clone(&child_ctx),
        );

        // Launch the child on its own task and register the handle so the
        // parent can observe and steer it.
        let result_sender = ctx.get_extension::<ChildResultSender>();

        // Delivery-anchored reclamation is enabled only when the runtime
        // declared it (no external status observer) AND a result channel
        // exists to anchor "delivered" to. See `super::reclaim`.
        let reclaim_on_delivery =
            result_sender.is_some() && ctx.get_extension::<ReclaimOnResultDelivery>().is_some();

        // NH-006 R5 / C56: fire SubagentHook::on_subagent_start before
        // launching the child. The hook is observational — Block has no
        // semantics on start (the trait method returns `()`). The shared
        // hook registry is published by the CLI's runtime builder onto
        // the orchestrator's ToolContext as an Arc<HookRegistry>
        // extension, so spawn sites without a LoopContext reference can
        // retrieve it here. Absent → no hook to fire.
        let hooks = ctx.get_extension::<HookRegistry>();
        if let Some(hooks_arc) = hooks.as_ref() {
            hooks_arc
                .run_subagent_start(&child_id.to_string(), &role_label)
                .await;
        }

        // Hook coverage (parent → child): the child's loop dispatches
        // pre/post-tool hooks from its *own* LoopContext, so the parent's
        // shared registry must be installed here — otherwise operator
        // policy/observability hooks silently never see child tool calls.
        child_loop_ctx.hooks = hooks.as_ref().map(Arc::clone);
        // The child's loop and its ToolContext share one working-dir
        // handle (seeded from the parent's current dir by
        // `build_child_context`), so the child's bash `cd` moves its
        // loop-level command execution and its tool path resolution
        // together — mirroring the fork pipeline and `build_runtime`.
        child_loop_ctx.working_dir = child_ctx.shared_working_dir();

        let child_event_sender = ctx
            .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
            .map(|ch| {
                AgentEventSender::new(ch.0.clone(), child_id, format!("spawn/{}", args.model))
            });

        let handle = launch_child(ChildLaunch {
            provider: Arc::clone(&infra.provider),
            executor: child_executor,
            store: child_store,
            loop_ctx: child_loop_ctx,
            tool_defs,
            task: args.task,
            model: args.model,
            agent_registry: Arc::clone(&infra.registry),
            result_sender: result_sender.map(|s| (*s).clone()),
            child_id,
            branch_metadata,
            hooks,
            role_label,
            event_sender: child_event_sender,
            reclaim_handles: reclaim_on_delivery.then(|| Arc::clone(&handles)),
        });
        handles.insert(handle);

        // Close the insert/finish race in reclaim-on-delivery mode: a
        // fast child may have finished — and the wrapper's reclamation
        // may have run — before the insert above stored the handle. The
        // wrapper marks the registry terminal before it reclaims, so
        // "terminal or already reclaimed" here means the wrapper's
        // reclamation pass cannot still be ahead of us with the handle
        // unstored; both sides reclaim idempotently. A hook-suppressed
        // (still Active) child never satisfies the check.
        if reclaim_on_delivery && entry_terminal_or_reclaimed(&infra.registry, child_id) {
            reclaim_delivered_child(&infra.registry, &handles, child_id);
        }

        Ok(ToolOutput {
            content: serde_json::json!({
                "agent_id": child_id.to_string(),
                "path": path,
                "status": "active",
            }),
            is_error: false,
            duration: started.elapsed(),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use futures_util::{StreamExt, stream};
    use serde_json::json;

    use super::*;
    use crate::agent::mailbox::Mailbox;
    use crate::error::ProviderError;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::request::ProviderRequest;
    use crate::provider::tools::ProviderToolDefinition;
    use crate::provider::traits::ProviderStream;
    use crate::provider::usage::Usage;
    use crate::session::events::SessionEvent;
    use crate::session::tree::SessionTree;
    use crate::tool::envelope::RuntimeInputs;
    use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "spawn_agent".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        }
    }

    fn done_event() -> ProviderEvent {
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
            response_id: None,
        }
    }

    fn done_event_tool_use() -> ProviderEvent {
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
            response_id: None,
        }
    }

    /// Builds a parent [`ToolContext`] with [`AgentToolInfra`] + an empty
    /// [`AgentHandles`] — the minimum a spawning agent needs.
    fn parent_ctx(
        provider: Arc<dyn Provider>,
        parent_id: Uuid,
        agent_registry: &Arc<RwLock<AgentRegistry>>,
        tool_registry: Arc<ToolRegistry>,
        mailbox: Arc<Mailbox>,
    ) -> ToolContext {
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(agent_registry),
            mailbox,
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: parent_id,
            parent_id: None,
            tool_registry: Some(tool_registry),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx
    }

    /// Drives a spawn to completion: runs the tool, then takes the child's
    /// handle out of the parent's `AgentHandles` and awaits the join handle.
    async fn spawn_and_join(
        tool: &SpawnAgentTool,
        ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Uuid {
        let out = tool.execute(&envelope_for(args), ctx).await.expect("spawn");
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content["status"], "active");
        assert!(
            out.content.get("result_summary").is_none(),
            "immediate return carries no result"
        );
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("agent_id")).expect("uuid");
        let handle = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed")
            .remove(child_id)
            .expect("handle stored for child");
        handle.join_handle.await.expect("child task joins");
        child_id
    }

    /// Stub tool that records the [`AgentToolInfra`] identity it sees, so a
    /// test can prove the child dispatched against its own context.
    struct IdentityStubTool {
        seen_agent: Arc<StdMutex<Option<Uuid>>>,
        seen_parent: Arc<StdMutex<Option<Uuid>>>,
    }

    #[async_trait]
    impl TestTool for IdentityStubTool {
        fn name(&self) -> &'static str {
            "identity"
        }
        fn description(&self) -> &'static str {
            "records the agent identity it sees"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            if let Some(infra) = ctx.get_extension::<AgentToolInfra>() {
                *self.seen_agent.lock().unwrap() = Some(infra.agent_id);
                *self.seen_parent.lock().unwrap() = infra.parent_id;
            }
            Ok(TestToolOutput {
                content: serde_json::json!({"ok": true}),
                is_error: false,
                duration: Duration::ZERO,
            })
        }
    }

    /// Minimal echo stub tool.
    struct EchoStubTool {
        tool_name: &'static str,
    }

    #[async_trait]
    impl TestTool for EchoStubTool {
        fn name(&self) -> &'static str {
            self.tool_name
        }
        fn description(&self) -> &'static str {
            "echo stub for tests"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            Ok(TestToolOutput {
                content: serde_json::json!({"echoed": self.tool_name}),
                is_error: false,
                duration: Duration::ZERO,
            })
        }
    }

    /// Provider that gates its first stream behind a [`tokio::sync::Notify`]
    /// so a test can observe the child's `Active` status before it runs.
    struct GatedProvider {
        gate: Arc<tokio::sync::Notify>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }

    impl Provider for GatedProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            let mut seq = Some(self.responses.lock().unwrap().remove(0));
            let gate = Arc::clone(&self.gate);
            let s = stream::once(async move { gate.notified().await }).flat_map(move |()| {
                stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok))
            });
            Ok(Box::pin(s))
        }
    }

    /// Provider that records the `tools` of every request it receives.
    struct CapturingProvider {
        captured: Arc<StdMutex<Vec<ProviderToolDefinition>>>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }

    impl Provider for CapturingProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.captured.lock().unwrap().clone_from(&request.tools);
            let seq = self.responses.lock().unwrap().remove(0);
            Ok(Box::pin(stream::iter(seq.into_iter().map(Ok))))
        }
    }

    /// R6: spawn returns immediately with `status: "active"` while the child
    /// is still blocked, then the child completes asynchronously.
    #[tokio::test]
    async fn spawn_returns_immediately_then_child_runs_async() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
            gate: Arc::clone(&gate),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "child done".to_string(),
                },
                done_event(),
            ]]),
        });
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "do it", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        // Child is gated — registry still shows it Active, not Completed.
        assert_eq!(
            agent_registry.read().get(child_id).expect("entry").status,
            AgentStatus::Active,
        );

        // Release the gate and let the child finish. `notify_one` stores a
        // permit even if the child has not yet reached `notified()`, so
        // this is race-free regardless of scheduling.
        gate.notify_one();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle");
        let mut status_rx = handle.status_rx.clone();
        handle.join_handle.await.expect("join");
        // Terminal transition retains the entry (status displays hold it)
        // with terminal status; the watch channel carries it too.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("completed child entry stays observable until reclaimed")
                .status,
            AgentStatus::Completed,
        );
        assert_eq!(*status_rx.borrow_and_update(), AgentStatus::Completed);
    }

    #[tokio::test]
    async fn spawn_agent_without_infra_returns_execution_failed() {
        let tool = SpawnAgentTool::new();
        let envelope = envelope_for(json!({"task": "x", "model": "m", "role": "r"}));
        let ctx = ToolContext::empty();
        let err = tool.execute(&envelope, &ctx).await.expect_err("no infra");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("agent runtime not configured"), "{reason}");
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    /// When `AgentToolInfra.tool_registry` is `None`, spawn refuses to launch.
    #[tokio::test]
    async fn spawn_agent_errors_when_no_tool_registry() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&registry),
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            tool_registry: None,
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("must error when no registry");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("tool_registry") || reason.contains("tools"),
                    "reason must mention missing registry: {reason}"
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    /// Spawn refuses to launch when the `AgentHandles` extension is absent —
    /// a child must never run unobservable.
    #[tokio::test]
    async fn spawn_agent_errors_when_no_agent_handles() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&registry),
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("must error when AgentHandles missing");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("AgentHandles"), "{reason}");
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    /// R3: the spawned child's `AgentToolInfra` carries the child's own
    /// `agent_id` and `parent_id`, observed from within a tool the child
    /// dispatches.
    #[tokio::test]
    async fn spawned_child_has_correct_identity() {
        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let seen_agent = Arc::new(StdMutex::new(None));
        let seen_parent = Arc::new(StdMutex::new(None));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(IdentityStubTool {
            seen_agent: Arc::clone(&seen_agent),
            seen_parent: Arc::clone(&seen_parent),
        }));
        let registry = Arc::new(registry);

        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "introspect", "model": "haiku", "role": "worker"}),
        )
        .await;

        assert_eq!(
            *seen_agent.lock().unwrap(),
            Some(child_id),
            "child tool must see the child's agent_id",
        );
        assert_eq!(
            *seen_parent.lock().unwrap(),
            Some(parent),
            "child tool must see the spawning agent as parent",
        );
    }

    /// R5: the child receives exactly the tool definitions surviving the
    /// allow-list — `tools: ["read"]` while the registry has read + edit.
    #[tokio::test]
    async fn spawn_filters_tool_definitions_through_allow_list() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ]]),
        });

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool { tool_name: "read" }));
        registry.register(Box::new(EchoStubTool { tool_name: "edit" }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "limited", "model": "haiku", "role": "worker", "tools": ["read"]}),
        )
        .await;

        let defs = captured.lock().unwrap().clone();
        assert_eq!(
            defs.len(),
            1,
            "exactly one tool definition survives the allow-list"
        );
        assert!(matches!(
            defs.as_slice(),
            [ProviderToolDefinition::Function(function)] if function.name == "read"
        ));
    }

    /// R7: when the child completes, the parent receives a
    /// `ChildAgentResult` through the result channel with `succeeded: true`.
    #[tokio::test]
    async fn child_completion_sends_through_result_channel() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let mailbox = Arc::new(Mailbox::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&mailbox),
        );
        let sender = ChildResultSender(Arc::new(tx));
        ctx.insert_extension(Arc::new(sender));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "notify me", "model": "haiku", "role": "worker"}),
        )
        .await;

        let result = rx.try_recv().expect("exactly one result on the channel");
        assert_eq!(result.agent_id, child_id);
        assert!(result.succeeded, "child completed successfully");
        assert!(result.error.is_none(), "no error on success");
        assert!(
            !result.formatted_message.is_empty(),
            "formatted message must be non-empty",
        );
    }

    /// R7: the failure path still marks the registry `Failed` and still
    /// sends a result through the child result channel with
    /// `succeeded: false`.
    #[tokio::test]
    async fn child_failure_marks_failed_and_sends_result() {
        // Empty MockProvider — the first `stream()` call errors, so the
        // child's `run_agent_step` returns Err.
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let mailbox = Arc::new(Mailbox::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&mailbox),
        );
        let sender = ChildResultSender(Arc::new(tx));
        ctx.insert_extension(Arc::new(sender));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "will fail", "model": "haiku", "role": "worker"}),
        )
        .await;

        // Terminal transition retains the entry with Failed status; the
        // result channel carries the failure.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("failed child entry stays observable until reclaimed")
                .status,
            AgentStatus::Failed,
        );
        let result = rx.try_recv().expect("failure result on the channel");
        assert_eq!(result.agent_id, child_id);
        assert!(!result.succeeded, "child must report failure");
        assert!(result.error.is_some(), "error message present on failure");
    }

    /// R2: a disallowed tool name surfaces as `ToolNotFound` from the
    /// child's executor; the loop falls back to its next turn's text and the
    /// spawn still completes.
    #[tokio::test]
    async fn spawn_agent_tool_subset_gates_disallowed_tools() {
        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                name: Some("bash".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::TextDelta {
                text: "fell back to text".to_string(),
            },
            done_event(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool {
            tool_name: "search",
        }));
        registry.register(Box::new(EchoStubTool { tool_name: "bash" }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "try bash", "model": "haiku", "role": "worker", "tools": ["search"]}),
        )
        .await;
        // The child completed — the disallowed tool call did not fail the
        // run. The entry stays observable with Completed status.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("completed child entry stays observable until reclaimed")
                .status,
            AgentStatus::Completed,
        );
    }

    /// R4: a named profile resolved from a temp `.md` file supplies the
    /// child's `LoopContext` system instruction.
    #[test]
    fn build_child_loop_context_uses_profile_body() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("researcher.md");
        std::fs::write(
            &profile_path,
            "---\nname: researcher\nmodel: gpt-5\ntools: read, grep\n---\nYou are a focused researcher.\n",
        )
        .unwrap();

        let scan_dirs = vec![dir.path().to_path_buf()];
        let (loop_ctx, tools) =
            build_child_loop_context(Some("researcher"), "find the bug", &scan_dirs)
                .expect("profile resolves");
        assert!(
            loop_ctx.system_sections[0].contains("You are a focused researcher."),
            "profile body must become the child's base system instruction",
        );
        assert_eq!(
            tools,
            Some(vec!["read".to_owned(), "grep".to_owned()]),
            "profile's resolved tool list flows back as the allow-list",
        );
    }

    /// R4: when no profile is given the child's system instruction embeds
    /// the task itself.
    #[test]
    fn build_child_loop_context_default_embeds_task() {
        let (loop_ctx, tools) =
            build_child_loop_context(None, "summarise the report", &[]).expect("default builds");
        assert!(loop_ctx.system_sections[0].contains("summarise the report"));
        assert!(
            tools.is_none(),
            "no profile means no allow-list from a profile"
        );
    }

    /// R4: an unresolvable profile name surfaces as `ExecutionFailed` — no
    /// silent fallback to a default profile.
    #[test]
    fn build_child_loop_context_unknown_profile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let scan_dirs = vec![dir.path().to_path_buf()];
        // `LoopContext` is not `Debug`, so the `Ok` arm cannot use
        // `expect_err`; match the result explicitly instead.
        match build_child_loop_context(Some("missing"), "task", &scan_dirs) {
            Err(ToolError::ExecutionFailed { reason }) => {
                assert!(reason.contains("missing"), "{reason}");
            }
            Err(other) => panic!("expected ExecutionFailed, got {other:?}"),
            Ok(_) => panic!("unknown profile must error"),
        }
    }

    /// R5: with no allow-list, every available parent tool is offered.
    #[test]
    fn build_tool_definitions_includes_all_when_no_allow_list() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool { tool_name: "read" }));
        registry.register(Box::new(EchoStubTool { tool_name: "edit" }));
        let defs = build_tool_definitions(&registry, None);
        let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["edit", "read"]);
    }

    /// Stub tool counting how many times it actually executed, so a test
    /// can prove a denied tool never ran inside a child.
    struct CountingStubTool {
        tool_name: &'static str,
        executions: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl TestTool for CountingStubTool {
        fn name(&self) -> &'static str {
            self.tool_name
        }
        fn description(&self) -> &'static str {
            "counts executions"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            self.executions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(TestToolOutput {
                content: serde_json::json!({"ok": true}),
                is_error: false,
                duration: Duration::ZERO,
            })
        }
    }

    /// Permission-escape regression (blocker): the consent-boundary
    /// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must be
    /// forwarded from the parent's context into the child's context —
    /// the child loop resolves both from its own executor's shared
    /// context, so a missing forward disables enforcement entirely.
    #[tokio::test]
    async fn child_context_forwards_permission_policy_and_effect_index() {
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
        let policy = Arc::new(crate::config::permissions::PermissionPolicy::from_patterns(
            &["bash"],
            &[],
            &[],
        ));
        let effects = Arc::new(ToolEffectIndex::new());
        parent_ctx.insert_extension(Arc::clone(&policy));
        parent_ctx.insert_extension(Arc::clone(&effects));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
        );

        let forwarded_policy = child_ctx
            .get_extension::<crate::config::permissions::PermissionPolicy>()
            .expect("PermissionPolicy must be forwarded to the child context");
        assert!(
            Arc::ptr_eq(&forwarded_policy, &policy),
            "the child must share the parent's policy instance",
        );
        let forwarded_effects = child_ctx
            .get_extension::<ToolEffectIndex>()
            .expect("ToolEffectIndex must be forwarded to the child context");
        assert!(
            Arc::ptr_eq(&forwarded_effects, &effects),
            "the child must share the parent's effect index instance",
        );
    }

    /// Permission-escape regression (blocker), end to end: a tool denied
    /// by the parent's policy must stay denied inside a spawned child —
    /// the child model calls it, dispatch blocks it, and the tool body
    /// never executes.
    #[tokio::test]
    async fn denied_tool_stays_denied_inside_spawned_child() {
        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                name: Some("victim".to_string()),
                arguments_delta: r#"{"command": "rm -rf /"}"#.to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::TextDelta {
                text: "gave up".to_string(),
            },
            done_event(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CountingStubTool {
            tool_name: "victim",
            executions: Arc::clone(&executions),
        }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );
        ctx.insert_extension(Arc::new(
            crate::config::permissions::PermissionPolicy::from_patterns(&["victim"], &[], &[]),
        ));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "try the denied tool", "model": "haiku", "role": "worker"}),
        )
        .await;

        assert_eq!(
            executions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a tool denied in the parent must never execute inside a spawned child",
        );
        // The child itself still finishes (the deny surfaces as a blocked
        // tool result, not a child crash).
        assert_eq!(
            agent_registry.read().get(child_id).expect("entry").status,
            AgentStatus::Completed,
        );
    }

    /// R2: a child tool call carrying a `tool_use_description` envelope field
    /// is recorded verbatim in the child's [`EventStore`] on the
    /// `AssistantMessage` event — the runner captures the full raw arguments
    /// JSON before envelope fields are stripped — so the parent can read it
    /// straight from the handle's event store.
    #[tokio::test]
    async fn child_tool_use_description_recorded_in_event_store() {
        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                name: Some("probe".to_string()),
                arguments_delta: r#"{"tool_use_description":"inspecting the config"}"#.to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool { tool_name: "probe" }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "probe it", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle stored for child");
        let event_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("child task joins");

        let events = event_store.events();
        let found = events.iter().any(|e| match e {
            SessionEvent::AssistantMessage { tool_calls, .. } => tool_calls.iter().any(|tc| {
                tc.arguments
                    .get("tool_use_description")
                    .and_then(serde_json::Value::as_str)
                    == Some("inspecting the config")
            }),
            _ => false,
        });
        assert!(
            found,
            "tool_use_description must be recorded in the child's EventStore: {events:?}",
        );
    }

    /// R3 (standalone mode): with no [`SharedSessionTree`] installed, the
    /// child's events are still reachable through `AgentHandle.event_store`,
    /// and the [`AgentHandles`] accessors expose the store, the provenance
    /// metadata, and the child id.
    #[tokio::test]
    async fn child_event_store_accessible_via_agent_handle() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child output".to_string(),
            },
            done_event(),
        ]]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "produce events", "model": "haiku", "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed");

        let store_via_accessor = handles
            .event_store(child_id)
            .expect("event_store accessor returns the child store");
        assert_eq!(handles.list_children(), vec![child_id]);
        let meta = handles
            .branch_metadata(child_id)
            .expect("branch_metadata accessor");
        assert_eq!(meta.child_agent_id, child_id);
        assert_eq!(meta.parent_agent_id, parent);
        assert!(meta.profile_name.is_none());

        let handle = handles.remove(child_id).expect("handle stored for child");
        assert!(
            Arc::ptr_eq(&store_via_accessor, &handle.event_store),
            "accessor and handle must share the same EventStore Arc",
        );
        handle.join_handle.await.expect("child task joins");

        assert!(
            !store_via_accessor.is_empty(),
            "the child produced events the parent can read through the handle",
        );
    }

    /// R3 (SessionTree mode): when an orchestrator publishes a
    /// [`SharedSessionTree`], the child's [`EventStore`] is created as a
    /// named branch under the parent's session — the parent gains a `Fork`
    /// event, the branch carries `spawned/<role>` metadata, and
    /// `handle.event_store` aliases the tree's branch store.
    #[tokio::test]
    async fn spawn_uses_session_tree_branch_when_extension_present() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "branched child".to_string(),
            },
            done_event(),
        ]]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );

        let tree = Arc::new(SessionTree::new(SessionMetadata {
            created_at: Utc::now(),
            model: "opus".to_string(),
            role: Some("root".to_string()),
            status: SessionStatus::Active,
        }));
        let root_id = tree.root();
        ctx.insert_extension(Arc::new(SharedSessionTree {
            tree: Arc::clone(&tree),
            session_id: root_id,
        }));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "branch me", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let children = tree.list_children(root_id);
        assert_eq!(children.len(), 1, "child session branched under the root");
        let child_session_id = children[0];

        let child_node = tree.get(child_session_id).expect("child session node");
        assert_eq!(child_node.parent, Some(root_id));
        assert_eq!(
            child_node.metadata.role.as_deref(),
            Some("spawned/worker"),
            "branch metadata records the spawned/<role> label",
        );

        let root_events = tree.get_store(root_id).expect("root store").events();
        assert!(
            root_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Fork { .. })),
            "branching appends a Fork event to the parent's store",
        );

        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle stored for child");
        let tree_store = tree.get_store(child_session_id).expect("child store");
        assert!(
            Arc::ptr_eq(&tree_store, &handle.event_store),
            "handle.event_store must alias the SessionTree branch store",
        );
        handle.join_handle.await.expect("child task joins");
    }

    // NH-006 R5 / C56 + C57: SubagentHook fires on launch (`start`)
    // and on completion (`stop`). The shared HookRegistry is installed
    // on the parent's ToolContext as an Arc<HookRegistry> extension —
    // that is how the spawn site reaches it without a LoopContext.
    #[tokio::test]
    async fn subagent_hook_start_and_stop_fire_around_spawn() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

        struct CountingSubagentHook {
            start_count: Arc<AtomicUsize>,
            stop_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl SubagentHook for CountingSubagentHook {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {
                self.start_count.fetch_add(1, AtomicOrdering::SeqCst);
            }
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                self.stop_count.fetch_add(1, AtomicOrdering::SeqCst);
                HookOutcome::Proceed
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );

        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let mut registry = HookRegistry::new();
        registry.register(Hook::Subagent(Box::new(CountingSubagentHook {
            start_count: Arc::clone(&start_count),
            stop_count: Arc::clone(&stop_count),
        })));
        ctx.insert_extension(Arc::new(registry));

        let tool = SpawnAgentTool::new();
        let _child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "do it", "model": "haiku", "role": "worker"}),
        )
        .await;

        assert_eq!(
            start_count.load(AtomicOrdering::SeqCst),
            1,
            "SubagentHook::on_subagent_start must fire exactly once per spawn",
        );
        assert_eq!(
            stop_count.load(AtomicOrdering::SeqCst),
            1,
            "SubagentHook::on_subagent_stop must fire exactly once per spawn",
        );
    }

    /// Awaits `cond` becoming true within 5 seconds, polling. Used where
    /// the asserted state is produced by the child wrapper task *after*
    /// the observable result delivery (so there is no handle left to
    /// join on).
    async fn wait_for_condition<F: Fn() -> bool>(cond: F, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for: {what}",
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Unbounded-retention regression: with [`ReclaimOnResultDelivery`]
    /// installed and a result channel present, a naturally-completed
    /// child's registry entry AND parent-held handle are reclaimed once
    /// its result has been delivered — nothing pins the child's
    /// EventStore forever in embedded/headless runs.
    #[tokio::test]
    async fn delivered_result_reclaims_registry_and_handle_when_marker_present() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("result within timeout")
            .expect("channel open");
        assert_eq!(result.agent_id, child_id);
        assert!(result.succeeded);

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        wait_for_condition(
            || agent_registry.read().get(child_id).is_none() && !handles.contains(child_id),
            "registry entry and handle reclaimed after result delivery",
        )
        .await;
    }

    /// Reclamation ownership: with the marker installed but NO result
    /// channel, the wrapper must not reclaim — the handle holder owns
    /// the end of life (there is no delivery to anchor reclamation to).
    #[tokio::test]
    async fn no_reclamation_without_result_channel_even_with_marker() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );
        ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let mut status_rx = handles.status_rx(child_id).expect("status_rx tracked");
        status_rx
            .wait_for(|s| s.is_terminal())
            .await
            .expect("child reaches terminal status");

        assert!(
            handles.contains(child_id),
            "without a result channel the handle holder owns reclamation",
        );
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("entry stays observable")
                .status,
            AgentStatus::Completed,
        );
    }

    /// TUI-mode regression: without the marker (default), a delivered
    /// result must NOT reclaim — terminal entries stay observable for
    /// the external observer's hold window.
    #[tokio::test]
    async fn no_reclamation_without_marker_even_with_result_channel() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("result within timeout")
            .expect("channel open");

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let mut status_rx = handles.status_rx(child_id).expect("status_rx tracked");
        status_rx
            .wait_for(|s| s.is_terminal())
            .await
            .expect("child reaches terminal status");
        assert!(
            handles.contains(child_id),
            "without the marker the external observer owns reclamation",
        );
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("terminal entry stays observable for the hold window")
                .status,
            AgentStatus::Completed,
        );
    }

    /// A stop hook's Block suppresses the terminal transition — and must
    /// also suppress reclamation: a deliberately-held-open child is
    /// never swept away.
    #[tokio::test]
    async fn hook_block_suppresses_reclamation() {
        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

        struct BlockOnStop;

        #[async_trait]
        impl SubagentHook for BlockOnStop {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                HookOutcome::Block {
                    reason: "child has more to do".to_owned(),
                }
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::Subagent(Box::new(BlockOnStop)));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("result within timeout")
            .expect("channel open");

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let mut status_rx = handles.status_rx(child_id).expect("status_rx tracked");
        status_rx
            .wait_for(|s| s.is_terminal())
            .await
            .expect("watch reaches terminal status");

        assert!(
            handles.contains(child_id),
            "a hook-blocked child's handle must not be reclaimed",
        );
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("hook-blocked child stays registered")
                .status,
            AgentStatus::Active,
            "Block suppresses the terminal transition, so the entry stays Active",
        );
    }

    // NH-006 R5: SubagentHook::on_subagent_stop returning Block must
    // suppress the registry's terminal transition. The child stays in
    // whatever pre-terminal state it reached (Active here, since the
    // wrapper never called mark_completing).
    #[tokio::test]
    async fn subagent_hook_stop_block_suppresses_terminal_mark() {
        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

        struct BlockOnStop;

        #[async_trait]
        impl SubagentHook for BlockOnStop {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                HookOutcome::Block {
                    reason: "child has more to do".to_owned(),
                }
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mailbox::new()),
        );

        let mut registry = HookRegistry::new();
        registry.register(Hook::Subagent(Box::new(BlockOnStop)));
        ctx.insert_extension(Arc::new(registry));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "do it", "model": "haiku", "role": "worker"}),
        )
        .await;

        let status = agent_registry.read().get(child_id).expect("entry").status;
        assert_ne!(
            status,
            AgentStatus::Completed,
            "Block from SubagentHook::on_subagent_stop must prevent mark_completed",
        );
    }

    /// Confinement-escape regression (blocker): `workspace_root` is a
    /// plain field on [`ToolContext`] — not an extension — so
    /// `build_child_context` must forward it explicitly, and the child's
    /// working dir must be seeded from the parent's *current* working dir
    /// on the child's own handle (snapshot semantics), never from the
    /// process CWD.
    #[test]
    fn child_context_forwards_workspace_root_and_snapshots_working_dir() {
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
        let mut parent_ctx =
            ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/parent-wd")));
        parent_ctx.confine_to_workspace(PathBuf::from("/tmp/workspace-root"));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
        );

        assert_eq!(
            child_ctx.workspace_root(),
            Some(std::path::Path::new("/tmp/workspace-root")),
            "the parent's confinement root must be forwarded to the child",
        );
        assert_eq!(
            child_ctx.working_dir(),
            PathBuf::from("/tmp/parent-wd"),
            "the child's working dir must be seeded from the parent's current dir",
        );

        // Snapshot semantics: the child owns its handle, so a child-side
        // `cd` must not move the parent's working dir.
        child_ctx.set_working_dir(PathBuf::from("/tmp/child-moved"));
        assert_eq!(
            parent_ctx.working_dir(),
            PathBuf::from("/tmp/parent-wd"),
            "child working-dir mutations must not propagate to the parent",
        );
    }

    /// Hook-coverage regression: the parent's shared [`HookRegistry`]
    /// extension must be forwarded to the child context so the child's
    /// own spawn sites (grandchildren) can reach it.
    #[test]
    fn child_context_forwards_hook_registry_extension() {
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

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
        );

        let forwarded = child_ctx
            .get_extension::<HookRegistry>()
            .expect("HookRegistry must be forwarded to the child context");
        assert!(
            Arc::ptr_eq(&forwarded, &hooks),
            "the child must share the parent's hook registry instance",
        );
    }

    /// Builds a provider turn carrying a single `read` tool call.
    fn read_call_turn(item_id: &str, path: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: item_id.to_string(),
                name: Some("read".to_string()),
                arguments_delta: json!({ "path": path }).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ]
    }

    /// Collects the `read` tool results from a child store in event order.
    fn read_results(events: &[SessionEvent]) -> Vec<serde_json::Value> {
        events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::ToolResult {
                    tool_name, output, ..
                } if tool_name == "read" => Some(output.clone()),
                _ => None,
            })
            .collect()
    }

    /// Confinement-escape regression (blocker), end to end: a parent
    /// confined to a workspace root spawns a child; the child's `read`
    /// of an out-of-root file is REFUSED while an in-root read works.
    #[tokio::test]
    async fn spawned_child_file_tools_respect_parent_confinement() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let in_path = root.path().join("inside.txt");
        std::fs::write(&in_path, "inside-content").unwrap();
        let out_path = outside.path().join("secret.txt");
        std::fs::write(&out_path, "secret-content").unwrap();

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            read_call_turn("tc-out", &out_path.to_string_lossy()),
            read_call_turn("tc-in", &in_path.to_string_lossy()),
            vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ],
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::read::ReadTool::new()));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let mut ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );
        ctx.confine_to_workspace(root.path().to_path_buf());

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "read files", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let results = read_results(&child_store.events());
        assert_eq!(results.len(), 2, "both reads produced results: {results:?}");
        assert_eq!(
            results[0]["kind"], "confinement_refused",
            "the out-of-root read must be refused inside the child: {}",
            results[0],
        );
        assert_eq!(
            results[1]["kind"], "text",
            "the in-root read must succeed inside the child: {}",
            results[1],
        );
        assert!(
            results[1]["content"]
                .as_str()
                .unwrap()
                .contains("inside-content"),
            "the in-root read must return the file content: {}",
            results[1],
        );
    }

    /// Working-dir regression (blocker): a child must resolve relative
    /// paths under the parent's working dir, not the process CWD.
    #[tokio::test]
    async fn spawned_child_resolves_relative_paths_under_parent_working_dir() {
        let wd = tempfile::tempdir().unwrap();
        std::fs::write(wd.path().join("norn-rel-probe.txt"), "rel-probe-content").unwrap();

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            read_call_turn("tc-rel", "norn-rel-probe.txt"),
            vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ],
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::read::ReadTool::new()));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );
        ctx.set_working_dir(wd.path().to_path_buf());

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "read rel", "model": "haiku", "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let results = read_results(&child_store.events());
        assert_eq!(results.len(), 1, "the read produced a result: {results:?}");
        assert_eq!(
            results[0]["kind"], "text",
            "the relative read must resolve under the parent's working dir, \
             not the process CWD: {}",
            results[0],
        );
        assert!(
            results[0]["content"]
                .as_str()
                .unwrap()
                .contains("rel-probe-content"),
            "the relative read must return the probe content: {}",
            results[0],
        );
    }

    /// Hook-coverage regression (reviewer issue): a PreToolUse hook
    /// registered on the parent must observe a spawned child's tool
    /// calls — the child's loop dispatches hooks from its own
    /// `LoopContext`, so the parent's registry must be forwarded.
    #[tokio::test]
    async fn parent_pre_tool_hook_fires_for_spawned_child_tool_call() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        use crate::integration::hooks::{Hook, PreToolHook};

        struct CountingPreTool {
            tool_name: &'static str,
            count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl PreToolHook for CountingPreTool {
            async fn before_tool(
                &self,
                envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> HookOutcome {
                if envelope.tool_name == self.tool_name {
                    self.count.fetch_add(1, AtomicOrdering::SeqCst);
                }
                HookOutcome::Proceed
            }
        }

        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                name: Some("probe".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool { tool_name: "probe" }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(Mailbox::new()),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::PreTool(Box::new(CountingPreTool {
            tool_name: "probe",
            count: Arc::clone(&count),
        })));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "probe it", "model": "haiku", "role": "worker"}),
        )
        .await;

        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            1,
            "a parent-registered PreToolUse hook must fire for the child's tool call",
        );
    }
}
