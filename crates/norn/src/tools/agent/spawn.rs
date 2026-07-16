//! `SpawnAgentTool` (NA-006) — launches a sub-agent asynchronously.
//!
//! Spawn reserves a child slot in the agent registry, builds a per-child
//! [`ToolContext`] carrying the child's own identity, resolves an optional
//! profile into the child's [`LoopContext`], filters the parent registry's
//! tool definitions through the allow-list so the child model can see its
//! tools, then launches the child via [`tokio::spawn`] and returns
//! immediately. When the child reaches a terminal status the spawn wrapper
//! marks the registry, delivers the result on the child-result channel,
//! and updates the status watch channel that backs reactive waits.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use super::delegation::{
    auto_child_path, effective_child_tools, grant_child_policy, install_child_result_channel,
    resolve_spawner_policy,
};
use super::handle::{AgentHandles, AgentWakeRegistry, ChildBranchMetadata};
#[cfg(test)]
use super::infra::SubAgentExecutor;
use super::infra::{AgentCancellation, AgentModel, infra_from};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use super::spawn_context::{
    build_child_context, resolve_profile_root, validate_model_selected_profile,
};
use super::spawn_launch::{ChildLaunch, launch_child};
use super::variant_resolve::{SpawnIdentityArgs, resolve_parent_model, resolve_spawn};
use crate::agent::child_policy::{ChildLoopConfig, ChildPolicy, CoordinationEnvelope};
use crate::agent::fork::ParentSystemInstruction;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::ChildResultSender;
use crate::agent::variants::VariantCatalog;
use crate::error::ToolError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::loop_context::LoopContext;
use crate::profile::from_profile;
use crate::profile::loader::resolve_workspace_profile_at_launch_root;
use crate::provider::agent_event::{AgentEventSender, SubagentDescriptor, SubagentKind};
use crate::session::action_log::ActionLog;
use crate::session::events::ChildBranchKind;
use crate::session::{ChildBranchRequest, slugify_name_stem};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

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

// deny_unknown_fields: a typo'd key (e.g. `child_polciy`) must fail
// loudly, not silently hand the child a default grant where the caller
// intended a narrowing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    task: String,
    /// Optional model id. Resolution: this argument, else the variant's
    /// model, else the parent's own model (unless the variant requires
    /// an explicit model — a typed error then).
    #[serde(default)]
    model: Option<String>,
    /// Optional role label. Resolution: this argument, else the variant
    /// name; with neither the spawn is a typed error.
    #[serde(default)]
    role: Option<String>,
    /// Optional named agent variant (built-in or configured
    /// `variants.<name>` settings). Mutually exclusive with `profile`.
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// Optional MCP server subset for this child. Omit to inherit; an empty
    /// list removes MCP tools while preserving the other selected tools.
    #[serde(default)]
    mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    path: Option<String>,
    /// Optional JSON Schema the child's final output must validate
    /// against. Schema is an explicit per-spawn decision: a child never
    /// implicitly inherits the parent's output schema — omitting this
    /// field means the child produces free-form output.
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
    /// Optional per-spawn [`ChildPolicy`] narrowing (DECISION R2),
    /// mirroring the Rust type 1:1 at the JSON layer. Omitted → the
    /// child inherits the caller's own granted policy with the
    /// delegation depth decremented one level. Supplied → must be a
    /// strict narrowing of the caller's own grant; widening is a typed
    /// failure naming the caller's budget.
    #[serde(default)]
    child_policy: Option<ChildPolicy>,
}

/// Build the child's [`LoopContext`] and the profile-derived tool list.
///
/// When `variant_prompt` is `Some` (the spawn resolved a variant that
/// carries a prompt), the child's base instruction is the variant prompt
/// plus the task via
/// [`build_child_system_prompt`](crate::system_prompt::build_child_system_prompt)
/// — variants and profiles are mutually exclusive, so this branch never
/// competes with profile resolution. Otherwise, when `profile_name` is
/// `Some`, the named profile is resolved through the scanner over
/// the parent working directory's standard profile tiers; its system
/// instructions and reasoning config flow into the returned [`LoopContext`]
/// via [`from_profile`]. Model-selected profiles that declare prompt commands
/// are rejected before child construction because they cannot grant ambient
/// process authority.
/// The gated [`ToolRegistry`] `from_profile` produces is discarded — the
/// child shares the parent's registry — but the profile's resolved tool
/// list is returned so the caller can use it as the per-child
/// allow-list. With neither, a minimal context is built with the task
/// embedded as the system instruction.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when a named profile cannot be
/// resolved — spawn never silently falls back to a default profile.
fn build_child_loop_context(
    variant_prompt: Option<&str>,
    profile_name: Option<&str>,
    task: &str,
    working_dir: &Path,
) -> Result<(LoopContext, Option<Vec<String>>), ToolError> {
    if let Some(prompt) = variant_prompt {
        let base = crate::system_prompt::build_child_system_prompt(prompt, task);
        return Ok((LoopContext::new(base), None));
    }
    if let Some(name) = profile_name {
        let profile = resolve_workspace_profile_at_launch_root(name, working_dir)
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("spawn_agent: selected profile could not be resolved: {e}"),
            })?
            .profile;
        validate_model_selected_profile(&profile)?;
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
/// Delegates to the shared registry → function-definition projection in
/// [`crate::provider::surface`] — the same projection `AgentBuilder`
/// assembly uses — filtered through `allow_list` (the same list that gates
/// the child's [`SubAgentExecutor`]). When `allow_list` is `None` every
/// available parent tool is included. The child's agent loop then resolves
/// these definitions against the live provider's capabilities per request,
/// exactly like the parent's loop, so hosted-tool replacement applies
/// identically to children.
#[cfg(test)]
fn build_tool_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<crate::provider::request::ToolDefinition> {
    match allow_list {
        Some(allow_list) => {
            crate::provider::surface::collect_registered_function_definitions(registry, allow_list)
        }
        None => crate::provider::surface::collect_function_definitions(registry, None),
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
        super::spawn_schema::input_schema()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnAgentArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("invalid arguments: {e}"),
                }
            })?;

        // Reserved-key check at the argument boundary so the caller gets
        // synchronous feedback; the agent loop re-checks the same
        // invariant as a backstop when the child run starts.
        if let Some(schema) = args.output_schema.as_ref() {
            crate::r#loop::schema::check_reserved_envelope_keys(schema).map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("spawn_agent: {e}"),
                }
            })?;
        }
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
        // `build_runtime` installs it during runtime construction; a
        // missing extension surfaces as a typed `MissingExtension` error.
        let handles = ctx.require_extension::<AgentHandles>()?;
        let wake_registry = ctx.require_extension::<AgentWakeRegistry>()?;

        // The coordination envelope is the runtime's deliberate child
        // policy (W3.0 made it builder-required; the CLI assembly
        // publishes its own). A context that can spawn but carries no
        // envelope is a wiring error, surfaced as the same typed
        // `MissingExtension` failure as a missing `AgentHandles` — spawn
        // never invents a policy for the child.
        let coordination = ctx.require_extension::<CoordinationEnvelope>()?;

        // The child's grant (W3.4): the caller's own granted policy — the
        // harness-stamped grant for spawned/forked callers, the envelope's
        // `child_policy` for the root — narrowed by the optional
        // `child_policy` argument, or derived by inherit-with-decrement
        // when omitted. Depth exhaustion and widening both fail typed
        // here, naming the caller's own budget; the registry re-validates
        // the same invariants from ground truth at reservation.
        let spawner_policy = resolve_spawner_policy(&infra, &coordination);
        let child_policy =
            grant_child_policy(&spawner_policy, args.child_policy.clone(), "spawn_agent")?;

        // Variant resolution (agent-variants R3, spec §3 steps 1–4):
        // variant/profile exclusivity, catalog lookup, role and model
        // resolution. Parent-model inheritance reads runtime ground truth
        // only — the live AgentModel extension (refreshed at every step
        // start with the step's actual model), else the caller's registry
        // entry (see `resolve_parent_model` for why the extension wins).
        let catalog = ctx.get_extension::<VariantCatalog>();
        let resolution = resolve_spawn(
            SpawnIdentityArgs {
                variant: args.variant.clone(),
                profile: args.profile.clone(),
                role: args.role.clone(),
                model: args.model.clone(),
            },
            catalog.as_deref(),
            "spawn_agent",
            || resolve_parent_model(&infra.registry, infra.agent_id, Some(ctx), "spawn_agent"),
        )?;
        let child_model = resolution.model.clone();
        let child_role = resolution.role.clone();

        // Child context-window validation (spec §7): fill the child's
        // window from the catalog for the CHILD's resolved model and
        // validate it — mirroring the root build's arm-then-validate
        // sequence — BEFORE anything is reserved or persisted, so a
        // failure is a clean typed error with no burned name and no
        // dangling reservation. The same resolved config rides into the
        // launch below; the launch-side arming finds the window already
        // set and leaves it untouched.
        let mut child_config = ChildLoopConfig::resolve(child_policy.loop_config);
        crate::agent::arming::arm_child_window(&mut child_config, &child_model).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("spawn_agent: {e}"),
            }
        })?;

        // Build the child's loop context and resolve the variant's or
        // profile's tool list. Profile authority stays rooted at the
        // immutable launch directory even after a tool changes the live
        // execution directory with `cd`.
        let profile_root = resolve_profile_root(ctx, args.profile.is_some())?;
        let (mut child_loop_ctx, profile_tools) = build_child_loop_context(
            resolution.variant_prompt.as_deref(),
            args.profile.as_deref(),
            &args.task,
            &profile_root,
        )?;
        // Reasoning effort (spec §3.6 + owner rulings 2026-07-07):
        // variant → profile → the parent's ACTIVE effort from the live
        // per-step stamp, validated against the model catalog for the
        // CHILD's resolved model BEFORE the reservation — see
        // `resolve_child_reasoning_effort` for the full contract.
        // Threaded onto the child's LoopContext, which the child loop's
        // prompt assembly copies into every provider request.
        child_loop_ctx.reasoning_effort = super::variant_resolve::resolve_child_reasoning_effort(
            &super::variant_resolve::ChildEffortInputs {
                variant_effort: resolution.reasoning_effort,
                variant_name: resolution.variant_name.as_deref(),
                profile_effort: child_loop_ctx.reasoning_effort,
                profile_name: args.profile.as_deref(),
                parent_live_effort: ctx
                    .get_extension::<AgentModel>()
                    .and_then(|live| live.reasoning_effort),
                child_model: &child_model,
                child_role: &child_role,
                surface: "spawn_agent",
            },
        )?;

        // Effective tools = allowlist ∩ policy (R6): the explicit `tools`
        // argument wins, else the variant's subset, else the profile's
        // list; the granted policy then strips what the child may never
        // use — `signal_agent` under MessagingScope::None, spawn_agent
        // AND fork for a leaf grant — at assembly, so the child's tool
        // definitions and its `SubAgentExecutor` gate agree (the
        // call-rejection paths stay as defence-in-depth).
        let base_allow_list: Option<Vec<String>> = args
            .tools
            .clone()
            .or(resolution.variant_tools.clone())
            .or(profile_tools);
        let mcp_selection = args
            .mcp_servers
            .clone()
            .or(resolution.variant_mcp_servers.clone());
        let allow_list =
            effective_child_tools(parent_registry, base_allow_list, &child_policy, &child_role);

        // Skill listing (parity with the root): advertise "# Available
        // Skills" on the child's system prompt only when the parent
        // published a skill catalog AND the skill tool is on the child's
        // resolved surface. Uses the same shared mechanism + filtered
        // listing the root builder uses, so the section cannot drift.
        if let Some(catalog) = ctx.get_extension::<crate::skill::SkillCatalog>() {
            crate::agent::arming::install_child_skill_listing(
                &mut child_loop_ctx,
                &catalog,
                crate::agent::arming::child_skill_tool_available(
                    parent_registry,
                    allow_list.as_deref(),
                ),
            );
        }

        // Auto paths nest under the spawning agent's own registry path so
        // the agents tree reads as a real tree at every depth (W3.4).
        let path = args
            .path
            .unwrap_or_else(|| auto_child_path(&infra.registry, infra.agent_id, "spawn"));

        // The session-tree name label and the audit-trail provenance
        // prefer the variant name, then the profile name, then the
        // resolved role — so the R4 child name (and the hook matcher
        // input) carries the variant when one was used.
        let role_label = resolution
            .variant_name
            .clone()
            .or_else(|| args.profile.clone())
            .unwrap_or_else(|| child_role.clone());

        // Provenance carried on both typed lifecycle phases: the RESOLVED
        // role and model, with `profile` disclosing the variant name when
        // a variant was used — `subagent.started` on the parent's
        // timeline thereby records the variant durably.
        let descriptor = SubagentDescriptor {
            kind: SubagentKind::Spawn,
            role: child_role.clone(),
            model: child_model.clone(),
            profile: resolution
                .variant_name
                .clone()
                .or_else(|| args.profile.clone()),
        };

        // Two-phase reservation: the guard stays unconfirmed across the
        // fallible store resolution below, so an error rolls the
        // reservation back via RAII instead of leaking a confirmed entry
        // that no launch wrapper will ever transition to a terminal
        // status.
        let guard = AgentRegistry::reserve(
            &infra.registry,
            path.clone(),
            child_role.clone(),
            child_model.clone(),
            Some(infra.agent_id),
            child_policy.clone(),
            Some(&spawner_policy),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn reservation failed: {e}"),
        })?;
        let child_id = guard.id();
        child_loop_ctx.agent_id = Some(child_id);
        child_loop_ctx.pending_agent_messages = Some(Arc::clone(&infra.pending_messages));

        // Mint the child's session through the parent's branching binding
        // (V2-R2): a persistent parent yields a real write-through child
        // timeline under the root's children/ dir, with the ChildBranch
        // reservation durably on the parent's timeline PARENT-FIRST; an
        // ephemeral parent propagates ephemerality with the honest
        // `session: None` reservation. The branched binding rides on the
        // child's infra so grandchild mints recurse structurally. The
        // mint's blocking file I/O runs off the executor (F5).
        let branched = super::delegation::branch_child_off_executor(
            &infra.session,
            &infra.event_store,
            &ChildBranchRequest {
                child_session_id: child_id.to_string(),
                name_stem: slugify_name_stem(&role_label, "spawn"),
                kind: ChildBranchKind::Spawn,
                durability: infra.session.child_durability(),
                model: child_model.clone(),
                working_dir: ctx.working_dir().display().to_string(),
            },
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn_agent: session branch failed: {e}"),
        })?;
        let child_store = Arc::clone(&branched.store);

        let child_event_sender = ctx
            .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
            .map(|ch| {
                AgentEventSender::new(ch.0.clone(), child_id, format!("spawn/{child_model}"))
            });

        // Typed lifecycle: `Started` is emitted before the child task
        // launches, so it always precedes the child's own provider
        // events on the broadcast channel; the wrapper task emits
        // `Completed`. Both phases also land as Custom audit events on
        // the parent's session store.
        let lifecycle = LifecycleEmitter::new(
            child_event_sender.clone(),
            Arc::clone(&infra.event_store),
            infra.agent_id,
            child_id,
            descriptor,
            Utc::now(),
        );
        // The Started audit joins the primary write-through contract
        // (session-fidelity Gap 10) and fires BEFORE the reservation is
        // confirmed: on a persist failure the guard's RAII rollback
        // reclaims the registry slot, so a refused spawn can never leave
        // a phantom Active child pinning the parent's concurrency budget
        // (the only residue is the already-tolerated burned name +
        // dangling reservation).
        lifecycle
            .emit_started()
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!(
                    "failed to persist the subagent.started audit event; \
                     spawn aborted before launch: {error}",
                ),
            })?;

        // All fallible setup is done — confirm the reservation. From here
        // the launch is unconditional and the completion wrapper owns the
        // entry's terminal transition.
        guard.confirm().map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn confirm failed: {e}"),
        })?;

        // Provenance recorded on the child's AgentHandle so the parent can
        // attribute the child's audit trail (NA-008 R3).
        let branch_metadata = ChildBranchMetadata {
            child_agent_id: child_id,
            parent_agent_id: infra.agent_id,
            profile_name: resolution
                .variant_name
                .clone()
                .or_else(|| args.profile.clone()),
            spawned_at: Utc::now(),
        };

        // Hierarchical cancellation (W3.5): the child's run token is a
        // child of the spawner's published token, so cancelling the
        // spawner — or any ancestor above it — cascades to this child and
        // its whole subtree, each run ending with its real `Cancelled`
        // outcome through its own wrapper. A parent context that
        // publishes no token (embedder roots assembled outside
        // `AgentBuilder`) yields a free-standing token — exactly the
        // pre-cascade behavior; see `AgentCancellation` for the boundary.
        let child_cancel = ctx
            .get_extension::<AgentCancellation>()
            .map_or_else(tokio_util::sync::CancellationToken::new, |parent_cancel| {
                parent_cancel.0.child_token()
            });

        // Per-child ToolContext: fresh identity, fresh AgentHandles, shared
        // infrastructure forwarded from the parent, the granted policy
        // stamped for signal_agent's scope enforcement and the child's own
        // spawn-time budget reads.
        let child_ctx = build_child_context(
            &infra,
            child_id,
            Arc::clone(&child_store),
            ctx,
            Arc::clone(&branched.binding),
            child_policy.clone(),
            child_cancel.clone(),
        );
        // Every agent's context carries its OWN launch model (parent-model
        // ground truth for the child's own spawns) and its OWN base system
        // instruction (the carrier `fork` reads for the next level's
        // parent-context inheritance, R5). The base is final here: the
        // variant/profile/task instruction plus the skill listing were
        // installed on `child_loop_ctx` above.
        child_ctx.insert_extension(Arc::new(AgentModel {
            model: child_model.clone(),
            reasoning_effort: child_loop_ctx.reasoning_effort,
        }));
        child_ctx.insert_extension(Arc::new(ParentSystemInstruction::new(
            child_loop_ctx.base_system_instruction(),
        )));
        // Per-agent result channel (W3.4): a child whose grant lets it
        // delegate gets its own child-result channel — sender on its
        // context for its spawn/fork sites, receiver wired onto its loop
        // below — so grandchild results deliver to *this child*, one hop
        // at a time.
        let child_result_rx = install_child_result_channel(
            &child_ctx,
            &child_policy,
            coordination.child_result_capacity,
        );
        let child_tools = super::live_tools::child_tool_snapshot(
            ctx,
            parent_registry,
            allow_list,
            mcp_selection,
            Arc::clone(&child_ctx),
        )?;
        let child_executor = child_tools.executor;
        let tool_defs = child_tools.definitions;

        // Launch the child on its own task and register the handle so the
        // parent can observe and steer it.
        let result_sender = ctx.get_extension::<ChildResultSender>();

        let persistent = infra
            .registry
            .read()
            .get(infra.agent_id)
            .is_none_or(|entry| entry.role != "fork");
        let reclaim_on_delivery =
            result_sender.is_some() && ctx.get_extension::<ReclaimOnResultDelivery>().is_some();
        let (handle_installed_tx, reclaim_handshake) = if reclaim_on_delivery {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (
                Some(tx),
                Some(ReclaimHandshake {
                    handles: Arc::clone(&handles),
                    handle_installed: rx,
                }),
            )
        } else {
            (None, None)
        };

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
        // Per-agent action log: the child's loop records its own tool
        // dispatches into the child's log (installed on the child context
        // by `build_child_context`), so the child's `action_log` queries
        // work and the parent's scoped queries see the child's entries.
        child_loop_ctx.action_log = child_ctx.get_extension::<ActionLog>();
        // Result delivery from the child's own children: the loop drains
        // this receiver at the same step boundaries the root uses — zero
        // loop changes, results bubble one hop per level.
        child_loop_ctx.child_result_rx = child_result_rx;

        let handle = launch_child(ChildLaunch {
            provider: Arc::clone(&infra.provider),
            executor: child_executor,
            store: child_store,
            loop_ctx: child_loop_ctx,
            tool_defs,
            task: args.task,
            output_schema: args.output_schema,
            model: child_model,
            agent_registry: Arc::clone(&infra.registry),
            result_sender: result_sender.map(|s| (*s).clone()),
            child_id,
            branch_metadata,
            hooks,
            role_label,
            event_sender: child_event_sender,
            reclaim: reclaim_handshake,
            lifecycle,
            router: Arc::clone(&infra.router),
            inbound_capacity: child_policy.inbound_capacity,
            config: child_config,
            cancel: child_cancel,
            wake_registry: persistent.then(|| Arc::clone(&wake_registry)),
            persistent,
        });
        handles.insert(handle);
        if persistent && let Some(handle) = handles.wake_handle(child_id) {
            wake_registry.insert(handle);
        }
        if let Some(tx) = handle_installed_tx
            && tx.send(()).is_err()
        {
            tracing::debug!(
                child_id = %child_id,
                "spawn_agent: wrapper exited before the handle-installed ack; \
                 reclamation ownership lies with whoever ended the wrapper",
            );
        }

        Ok(ToolOutput::success(serde_json::json!({
            "agent_id": child_id.to_string(),
            "path": path,
            "status": "active",
        })))
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::path::PathBuf;

    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use futures_util::{StreamExt, stream};
    use parking_lot::RwLock;
    use serde_json::json;
    use uuid::Uuid;

    use super::super::canonical_lifecycle_test_support::{
        canonical_item_values, canonical_payload_items, completed_item_event,
        spawn_non_audio_items, stateless_payload_input,
    };
    use super::super::infra::AgentToolInfra;
    use super::*;
    use crate::agent::child_policy::MessagingScope;
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentStatus;
    use crate::error::ProviderError;
    use crate::integration::hooks::HookOutcome;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::request::ProviderRequest;
    use crate::provider::tools::ProviderToolDefinition;
    use crate::provider::traits::Provider;
    use crate::provider::traits::ProviderStream;
    use crate::provider::usage::Usage;
    use crate::session::events::SessionEvent;
    use crate::session::store::EventStore;
    use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

    /// Catalogued model used for child launches throughout these tests:
    /// child launch paths validate the child's context window against the
    /// model catalog (agent-variants §7), so an uncatalogued placeholder
    /// would be rejected before the behavior under test runs. Factual
    /// (the generated catalog's default), never invented.
    const CATALOG_MODEL: &str = crate::model_catalog::default_selection().model;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "spawn_agent".to_string(),
            model_args: args,
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

    /// Documented-proposal coordination envelope used by tests — a
    /// deliberate test-caller choice, never a library default.
    fn test_envelope() -> CoordinationEnvelope {
        use crate::agent::child_policy::DelegationBudget;
        CoordinationEnvelope {
            child_policy: ChildPolicy {
                messaging: MessagingScope::SiblingsAndParent,
                delegation: DelegationBudget {
                    remaining_depth: 1,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            child_result_capacity: 256,
        }
    }

    /// Builds a parent [`ToolContext`] with [`AgentToolInfra`], an empty
    /// [`AgentHandles`], and the [`CoordinationEnvelope`] — the minimum a
    /// spawning agent needs.
    fn parent_ctx(
        provider: Arc<dyn Provider>,
        parent_id: Uuid,
        agent_registry: &Arc<RwLock<AgentRegistry>>,
        tool_registry: Arc<ToolRegistry>,
        router: Arc<MessageRouter>,
    ) -> ToolContext {
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(agent_registry),
            router,
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: parent_id,
            parent_id: None,
            grant: None,
            tool_registry: Some(tool_registry),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
        ctx.insert_extension(Arc::new(test_envelope()));
        ctx
    }

    /// A persistent parent context: a real `SessionManager` root session
    /// in `tmp`, its sink-equipped store on the infra, and a persistent
    /// [`SessionBinding`](crate::session::SessionBinding) so spawned
    /// children mint REAL on-disk timelines (V2-R2).
    fn persistent_parent_ctx(
        tmp: &std::path::Path,
        provider: Arc<dyn Provider>,
        parent_id: Uuid,
        agent_registry: &Arc<RwLock<AgentRegistry>>,
        tool_registry: Arc<ToolRegistry>,
    ) -> (ToolContext, crate::session::SessionManager, String) {
        use crate::session::manager::{CreateSessionOptions, SessionManager};
        use crate::session::store::DurabilityPolicy;
        use crate::session::{SessionBinding, SessionBrancher};
        let manager = SessionManager::new(tmp);
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "haiku".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .expect("create root session");
        let root_session_id = opened.entry.id.clone();
        let binding = Arc::new(SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager.clone(),
                root_session_id.clone(),
                DurabilityPolicy::Flush,
            )),
            root_session_id.clone(),
            &[],
        ));
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(opened.store),
            agent_id: parent_id,
            parent_id: None,
            grant: None,
            tool_registry: Some(tool_registry),
            session: binding,
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
        ctx.insert_extension(Arc::new(test_envelope()));
        (ctx, manager, root_session_id)
    }

    /// Sink double that accepts everything EXCEPT `Custom` events — lets
    /// the `ChildBranch` reservation through but refuses the
    /// `subagent.started` audit, isolating the Started-audit failure
    /// path.
    struct CustomRefusingSink;
    impl crate::session::store::PersistenceSink for CustomRefusingSink {
        fn persist(
            &mut self,
            event: &SessionEvent,
        ) -> Result<(), crate::session::persistence::SessionPersistError> {
            match event {
                SessionEvent::Custom { .. } => {
                    Err(crate::session::persistence::SessionPersistError::Io(
                        std::io::Error::other("sink refused the audit"),
                    ))
                }
                _ => Ok(()),
            }
        }
    }

    /// F1 regression: a `subagent.started` persist failure aborts the
    /// spawn BEFORE the reservation is confirmed — the tool errors AND
    /// the registry holds no phantom Active child afterwards (the
    /// unconfirmed guard's RAII rollback reclaims the slot; a
    /// post-confirm failure would have pinned the parent's
    /// max_concurrent_children budget forever, with no wrapper left to
    /// transition the entry).
    #[tokio::test]
    async fn started_audit_failure_aborts_spawn_without_phantom_child() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::with_sink(Box::new(CustomRefusingSink))),
            agent_id: parent,
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
        ctx.insert_extension(Arc::new(test_envelope()));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "t", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("a Started-audit persist failure must abort the spawn");
        assert!(
            err.to_string().contains("subagent.started"),
            "the failure names the refused audit: {err}",
        );

        // No phantom child: the unconfirmed reservation rolled back, so
        // the registry lists nothing and the parent's concurrency budget
        // is untouched.
        let reg = agent_registry.read();
        assert!(
            reg.list().is_empty(),
            "the rolled-back reservation must leave no registry entry: {:?}",
            reg.list(),
        );
        assert!(reg.tombstones().is_empty(), "and no tombstone");
    }

    /// Read a persisted session's events back from DISK through its index
    /// row — never from memory — so assertions prove durability.
    fn events_on_disk(manager: &crate::session::SessionManager, id: &str) -> Vec<SessionEvent> {
        let entry = manager.resolve(id).expect("session indexed");
        crate::session::persistence::io::read_session_events_for_entry(manager.data_dir(), &entry)
            .expect("session replays from disk")
            .events
    }

    /// Drives a spawn until the child is no longer actively running.
    ///
    /// Natural child completion now parks the spawned child in
    /// [`AgentStatus::Idle`] so it can be woken later; only hard terminal
    /// outcomes make the wrapper exit. This helper therefore observes the
    /// status watch instead of taking and joining the handle.
    async fn spawn_and_join(
        tool: &SpawnAgentTool,
        ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Uuid {
        let out = tool.execute(&envelope_for(args), ctx).await.expect("spawn");
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["status"], "active");
        assert!(
            out.content.get("result_summary").is_none(),
            "immediate return carries no result"
        );
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("agent_id")).expect("uuid");
        let mut status_rx = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed")
            .status_rx(child_id)
            .expect("status receiver stored for child");
        tokio::time::timeout(Duration::from_secs(5), async {
            status_rx
                .wait_for(|status| *status == AgentStatus::Idle || status.is_terminal())
                .await
        })
        .await
        .expect("child reaches idle or terminal status")
        .expect("status watch remains open");
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
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
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
            Ok(TestToolOutput::success(
                serde_json::json!({"echoed": self.tool_name}),
            ))
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

    struct RequestCapturingProvider {
        captured: Arc<StdMutex<Vec<ProviderRequest>>>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }

    impl Provider for RequestCapturingProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.captured.lock().unwrap().push(request);
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"})),
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
        let mut status_rx = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .status_rx(child_id)
            .expect("status_rx tracked");
        status_rx
            .wait_for(|status| *status == AgentStatus::Idle)
            .await
            .expect("child reaches idle");
        // Natural completion parks the child as a wakeable idle actor.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("idle child entry stays observable")
                .status,
            AgentStatus::Idle,
        );
        assert_eq!(*status_rx.borrow_and_update(), AgentStatus::Idle);
    }

    /// N-026 R6 (spawn path): the spawned child's own tool context carries
    /// a `ScheduleHandle`, proven behaviorally — the child calls the `cron`
    /// tool and the `schedule.created` event lands on the CHILD's event
    /// store. A missing extension would fail the call with
    /// `MissingExtension` and no such event could exist.
    #[tokio::test]
    async fn spawned_child_resolves_cron_tool_against_its_own_schedule_handle() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-cron".to_string(),
                    call_id: None,
                    name: Some("cron".to_string()),
                    arguments_delta:
                        r#"{"op":"schedule","in":"12h","message":"check the long job"}"#.to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "scheduled".to_string(),
                },
                done_event(),
            ],
        ]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let mut tools = ToolRegistry::new();
        crate::tools::registry_builder::register_cron_tool(&mut tools);
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(tools),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "schedule a check-in", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let child_store = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed")
            .event_store(child_id)
            .expect("child event store tracked");
        let created = child_store.events().into_iter().any(|e| {
            matches!(
                &e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::schedule::SCHEDULE_CREATED_EVENT_TYPE
            )
        });
        assert!(
            created,
            "the child's cron call must persist schedule.created to the child's own store",
        );
    }

    #[tokio::test]
    async fn spawn_agent_without_infra_returns_missing_extension() {
        let tool = SpawnAgentTool::new();
        let envelope = envelope_for(json!({"task": "x", "model": "m", "role": "r"}));
        let ctx = ToolContext::empty();
        let err = tool.execute(&envelope, &ctx).await.expect_err("no infra");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(
                    extension.contains("AgentToolInfra"),
                    "error must name the missing extension type: {extension}"
                );
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    /// When `AgentToolInfra.tool_registry` is `None`, spawn refuses to launch.
    #[tokio::test]
    async fn spawn_agent_errors_when_no_tool_registry() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: None,
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
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
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("must error when AgentHandles missing");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(extension.contains("AgentHandles"), "{extension}");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
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
                call_id: None,
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "introspect", "model": CATALOG_MODEL, "role": "worker"}),
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "limited", "model": CATALOG_MODEL, "role": "worker", "tools": ["read"]}),
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
        let router = Arc::new(MessageRouter::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&router),
        );
        let sender = ChildResultSender(Arc::new(tx));
        ctx.insert_extension(Arc::new(sender));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "notify me", "model": CATALOG_MODEL, "role": "worker"}),
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

    /// Hardening (owner ruling 2026-07-03): a spawned child must run with
    /// auto-compaction armed exactly like the root — otherwise a
    /// long-running delegated child dies `ContextWindowExceeded`. The
    /// spawn launch path calls the shared `arm_auto_compaction`, which
    /// installs the token estimator and fills the child's context window
    /// from the catalog for the child's own model. This drives a spawned
    /// child whose first turn reports an oversized usage (setting the
    /// context-edit usage floor above the window) and asserts the child's
    /// next preflight emitted a `loop.token_warning` on the child's store.
    /// That event is structurally impossible without the estimator and the
    /// window the shared arming installs (the preflight returns early with
    /// no estimator, and the warning is gated on a `Some` window), so its
    /// presence proves the spawn site armed the child.
    #[tokio::test]
    async fn spawn_child_arms_auto_compaction_preflight() {
        let catalog_model = crate::model_catalog::default_selection().model;
        // Turn 1: a tool call (forces a second provider round-trip so a
        // second preflight runs) whose reported usage dwarfs any context
        // window — this becomes the usage floor. Turn 2: end the turn.
        let oversized_done = ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 100_000_000,
                output_tokens: 0,
                ..Usage::default()
            },
            response_id: None,
        };
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-noop".to_string(),
                    call_id: None,
                    name: Some("noop".to_string()),
                    arguments_delta: "{}".to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                oversized_done,
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ],
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoStubTool { tool_name: "noop" }));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "run", "model": catalog_model, "role": "worker"}),
        )
        .await;

        let child_store = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed")
            .event_store(child_id)
            .expect("child event store observable");
        let warned = child_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. } if event_type == "loop.token_warning"
            )
        });
        assert!(
            warned,
            "the spawned child's preflight must emit loop.token_warning, \
             proving the estimator and catalog window were armed",
        );
    }

    /// Installs a one-skill catalog + search path on `ctx` and returns the
    /// temp dir (kept alive for the skill files) — the shared setup for the
    /// spawned-child skill tests.
    fn install_greet_skill(ctx: &ToolContext) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("greet");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: greet the user\n---\nHELLO_FROM_GREET",
        )
        .unwrap();
        let paths = vec![dir.path().to_path_buf()];
        let catalog = Arc::new(crate::skill::SkillCatalog::scan(&paths));
        ctx.insert_extension(Arc::new(crate::tools::skill::SkillSearchPaths(paths)));
        ctx.insert_extension(catalog);
        dir
    }

    fn skill_registry() -> Arc<ToolRegistry> {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
            crate::tools::skill::SkillToolConfig {
                shell_execution: false,
            },
        )));
        Arc::new(registry)
    }

    /// Defect 1 regression (critical): a spawned child must be able to load
    /// a skill end-to-end. Previously `build_child_context` never forwarded
    /// `SkillSearchPaths`/`SkillCatalog`, so the child saw the `skill` tool
    /// but every call failed `MissingExtension`. Here the child calls
    /// `skill` and its store must carry a successful `skill` tool result
    /// containing the skill body — impossible unless the extensions were
    /// forwarded.
    #[tokio::test]
    async fn spawned_child_loads_a_skill_end_to_end() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc1".to_string(),
                    call_id: None,
                    name: Some("skill".to_string()),
                    arguments_delta: json!({"name": "greet"}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ],
        ]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            skill_registry(),
            Arc::new(MessageRouter::new()),
        );
        let _skill_dir = install_greet_skill(&ctx);

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "greet the user", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let child_store = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed")
            .event_store(child_id)
            .expect("child event store observable");
        let loaded = child_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::ToolResult { tool_name, output, .. }
                    if tool_name == "skill" && output.to_string().contains("HELLO_FROM_GREET")
            )
        });
        assert!(
            loaded,
            "spawned child must load the skill successfully (extensions forwarded): {:?}",
            child_store.events(),
        );
    }

    /// Defect 1 regression: the spawned child's system prompt carries the
    /// "# Available Skills" section when the skill tool is on its surface.
    #[tokio::test]
    async fn spawned_child_system_prompt_lists_available_skills() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ]]),
        });
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            skill_registry(),
            Arc::new(MessageRouter::new()),
        );
        let _skill_dir = install_greet_skill(&ctx);

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "greet the user", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let requests = captured.lock().unwrap().clone();
        let advertises = requests.iter().flat_map(|r| r.messages.iter()).any(|m| {
            m.content.as_deref().is_some_and(|content| {
                content.contains("# Available Skills") && content.contains("greet")
            })
        });
        assert!(
            advertises,
            "the child's system prompt must advertise the skill listing: {requests:?}",
        );
    }

    /// Defect 1 regression: the "# Available Skills" section is absent when
    /// the child's allow-list excludes the skill tool (never advertise a
    /// skill the child has no tool to load).
    #[tokio::test]
    async fn spawned_child_system_prompt_omits_skills_when_tool_excluded() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ]]),
        });
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
            crate::tools::skill::SkillToolConfig {
                shell_execution: false,
            },
        )));
        registry.register(Box::new(EchoStubTool { tool_name: "read" }));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );
        let _skill_dir = install_greet_skill(&ctx);

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "read only", "model": CATALOG_MODEL, "role": "worker", "tools": ["read"]}),
        )
        .await;

        let requests = captured.lock().unwrap().clone();
        let advertises = requests.iter().flat_map(|r| r.messages.iter()).any(|m| {
            m.content
                .as_deref()
                .is_some_and(|content| content.contains("# Available Skills"))
        });
        assert!(
            !advertises,
            "a child without the skill tool must not advertise skills: {requests:?}",
        );
    }

    #[tokio::test]
    async fn signal_to_idle_child_queues_follow_up_and_wake_drains_mailbox() {
        use crate::tools::agent::coord::{SignalAgentTool, WakeAgentTool};

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![
                vec![
                    ProviderEvent::TextDelta {
                        text: "initial result".to_string(),
                    },
                    done_event(),
                ],
                vec![
                    ProviderEvent::TextDelta {
                        text: "woke and handled queued work".to_string(),
                    },
                    done_event(),
                ],
            ]),
        });
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let spawn_tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &spawn_tool,
            &ctx,
            json!({"task": "wait for later instructions", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;
        let initial = rx.try_recv().expect("initial child result delivered");
        assert_eq!(initial.agent_id, child_id);
        assert!(initial.succeeded);

        let signal_tool = SignalAgentTool::new();
        let signal_out = signal_tool
            .execute(
                &ToolEnvelope {
                    tool_call_id: "signal-idle".to_owned(),
                    tool_name: "signal_agent".to_owned(),
                    model_args: json!({
                        "to": child_id.to_string(),
                        "kind": "steer",
                        "content": "queued instruction from parent",
                    }),
                    metadata: serde_json::Value::Null,
                },
                &ctx,
            )
            .await
            .expect("signal executes");
        assert!(!signal_out.is_error(), "{:?}", signal_out.content);
        assert_eq!(signal_out.content["queued"], true);
        assert_eq!(signal_out.content["resume_required"], true);

        let follow_ups =
            crate::tool::traits::Tool::register_follow_ups(&signal_tool, &signal_out, &ctx).await;
        assert_eq!(
            follow_ups.len(),
            1,
            "queued signal exposes a wake follow-up"
        );
        assert_eq!(follow_ups[0].tool, "wake_agent");
        assert_eq!(follow_ups[0].args["agent_id"], child_id.to_string());

        let wake_out = WakeAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "wake-idle".to_owned(),
                    tool_name: "wake_agent".to_owned(),
                    model_args: json!({ "agent_id": child_id.to_string() }),
                    metadata: serde_json::Value::Null,
                },
                &ctx,
            )
            .await
            .expect("wake executes");
        assert!(!wake_out.is_error(), "{:?}", wake_out.content);
        assert_eq!(wake_out.content["woken"], true);
        assert_eq!(wake_out.content["queued_messages"], 1);

        let resumed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("resumed result delivered")
            .expect("channel open");
        assert_eq!(resumed.agent_id, child_id);
        assert!(resumed.succeeded);
        assert!(
            resumed
                .formatted_message
                .contains("woke and handled queued work"),
            "{}",
            resumed.formatted_message,
        );
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

        let requests = captured.lock().unwrap().clone();
        assert_eq!(requests.len(), 2, "initial step + wake step");
        let woke_with_message = requests[1].messages.iter().any(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("<agent_message")
                    && content.contains("queued instruction from parent")
            })
        });
        assert!(
            woke_with_message,
            "wake step must receive the queued message through the normal frame: {:?}",
            requests[1].messages,
        );
        let infra = ctx.get_extension::<AgentToolInfra>().expect("infra");
        assert!(
            infra
                .pending_messages
                .messages_for_delivery(child_id)
                .is_empty(),
            "wake step drains the durable mailbox"
        );
    }

    /// R7: the hard-error path still marks the registry `Failed` and still
    /// sends a result through the child result channel with
    /// `succeeded: false`.
    #[tokio::test]
    async fn child_failure_marks_failed_and_sends_result() {
        // Empty MockProvider — the first `stream()` call errors, so the
        // child's `run_agent_step` returns Err.
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let router = Arc::new(MessageRouter::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&router),
        );
        let sender = ChildResultSender(Arc::new(tx));
        ctx.insert_extension(Arc::new(sender));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "will fail", "model": CATALOG_MODEL, "role": "worker"}),
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

    /// Typed lifecycle: spawn emits `SubagentLifecycle::Started` then
    /// `Completed` on the shared broadcast channel — child-tagged, with
    /// parent/child ids, the spawn descriptor, ordered wall-clock
    /// timestamps, and the child's accumulated usage — and appends the
    /// matching `subagent.started` / `subagent.completed` Custom audit
    /// events to the parent's session store. The result channel carries
    /// the same per-child usage.
    #[tokio::test]
    async fn spawn_emits_typed_lifecycle_events_on_channel_and_parent_store() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
            SharedAgentEventChannel, SubagentKind, SubagentLifecycle,
        };

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
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
            Arc::new(MessageRouter::new()),
        );
        let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let before = Utc::now();
        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        // Collect every broadcast event; lifecycle events are child-tagged
        // and `Started` must precede the child's own provider events.
        let mut subagent_events = Vec::new();
        let mut first_child_event_is_started = None;
        while let Ok(ev) = brx.try_recv() {
            if ev.agent_id == child_id && first_child_event_is_started.is_none() {
                first_child_event_is_started = Some(matches!(
                    ev.event,
                    AgentEventKind::Subagent(SubagentLifecycle::Started { .. })
                ));
            }
            if let AgentEventKind::Subagent(lifecycle) = ev.event {
                assert_eq!(ev.agent_id, child_id, "lifecycle events are child-tagged");
                assert_eq!(*ev.agent_role, format!("spawn/{CATALOG_MODEL}"));
                subagent_events.push(lifecycle);
            }
        }
        assert_eq!(
            first_child_event_is_started,
            Some(true),
            "Started must precede the child's own provider events",
        );
        assert_eq!(subagent_events.len(), 2, "exactly Started then Completed");
        match &subagent_events[0] {
            SubagentLifecycle::Started {
                parent_id,
                child_id: c,
                descriptor,
                started_at,
            } => {
                assert_eq!(*parent_id, parent);
                assert_eq!(*c, child_id);
                assert_eq!(descriptor.kind, SubagentKind::Spawn);
                assert_eq!(descriptor.role, "worker");
                assert_eq!(descriptor.model, CATALOG_MODEL);
                assert!(descriptor.profile.is_none());
                assert!(
                    *started_at >= before,
                    "started_at is wall-clock launch time"
                );
            }
            other => panic!("expected Started, got {other:?}"),
        }
        match &subagent_events[1] {
            SubagentLifecycle::Completed {
                parent_id,
                child_id: c,
                descriptor,
                started_at,
                completed_at,
                usage,
                subtree_usage,
                succeeded,
                error,
                stop,
            } => {
                assert_eq!(*parent_id, parent);
                assert_eq!(*c, child_id);
                assert_eq!(descriptor.kind, SubagentKind::Spawn);
                assert!(*completed_at >= *started_at, "timestamps must be ordered");
                assert!(*succeeded);
                assert!(error.is_none());
                assert!(stop.is_none());
                assert_eq!(usage.input_tokens, 10, "per-child usage must surface");
                assert_eq!(usage.output_tokens, 5);
                assert_eq!(
                    subtree_usage.input_tokens, 10,
                    "a childless child's subtree usage equals its own usage",
                );
                assert_eq!(subtree_usage.output_tokens, 5);
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        // Audit carrier: the parent store got both Custom events.
        let infra = ctx.get_extension::<AgentToolInfra>().expect("infra");
        let custom: Vec<(String, serde_json::Value)> = infra
            .event_store
            .events()
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::Custom {
                    event_type, data, ..
                } => Some((event_type, data)),
                _ => None,
            })
            .collect();
        assert_eq!(custom.len(), 2, "started + completed audit events");
        assert_eq!(custom[0].0, SUBAGENT_STARTED_EVENT_TYPE);
        assert_eq!(custom[0].1["phase"], "started");
        assert_eq!(custom[0].1["child_id"], child_id.to_string());
        assert_eq!(custom[0].1["descriptor"]["kind"], "spawn");
        assert_eq!(custom[1].0, SUBAGENT_COMPLETED_EVENT_TYPE);
        assert_eq!(custom[1].1["phase"], "completed");
        assert_eq!(custom[1].1["succeeded"], true);
        assert_eq!(custom[1].1["usage"]["input_tokens"], 10);

        // The result channel carries the same per-child usage.
        let result = rx.try_recv().expect("result on the channel");
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 5);
    }

    /// Typed lifecycle on the failure path: a child whose provider errors
    /// reports `Completed` with `succeeded: false`, the error description,
    /// and zero usage (no provider call completed).
    #[tokio::test]
    async fn failed_spawn_emits_completed_lifecycle_with_error() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
        };

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "will fail", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let mut completed = None;
        while let Ok(ev) = brx.try_recv() {
            if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
                child_id: c,
                succeeded,
                error,
                usage,
                ..
            }) = ev.event
            {
                completed = Some((c, succeeded, error, usage));
            }
        }
        let (c, succeeded, error, usage) = completed.expect("completed lifecycle event");
        assert_eq!(c, child_id);
        assert!(!succeeded, "failed child must report succeeded: false");
        assert!(error.is_some(), "error description must be present");
        assert_eq!(usage.input_tokens, 0, "no provider call completed");
    }

    /// Panic defense: a panic inside the child's run (here: a tool that
    /// panics, standing in for a panicking dependency) must not leave
    /// observers a dangling `Started`. The wrapper isolates the run on an
    /// inner task, observes the `JoinError`, and still emits the
    /// `Completed` lifecycle event with `succeeded: false` and an honest
    /// error, delivers the failure through the result channel, and marks
    /// the registry `Failed`.
    #[tokio::test]
    async fn panicking_child_task_still_completes_lifecycle_and_delivers_result() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
        };

        struct PanickingTool;

        #[async_trait]
        impl TestTool for PanickingTool {
            fn name(&self) -> &'static str {
                "explode"
            }
            fn description(&self) -> &'static str {
                "panics on execute (test stand-in for a panicking dependency)"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({})
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<TestToolOutput, ToolError> {
                panic!("dependency panic inside child tool");
            }
        }

        let turn = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-panic".to_string(),
                call_id: None,
                name: Some("explode".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PanickingTool));

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );
        let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "boom", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        // Registry: the wrapper still applied the terminal transition.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("panicked child entry stays observable")
                .status,
            AgentStatus::Failed,
        );

        // Result channel: the failure is delivered, naming the panic.
        let result = rx.try_recv().expect("failure result on the channel");
        assert_eq!(result.agent_id, child_id);
        assert!(!result.succeeded, "panicked child must report failure");
        let error = result.error.expect("error present after panic");
        assert!(
            error.contains("panicked before completing"),
            "error must be honest about the panic: {error}",
        );

        // Lifecycle: `Completed` is emitted — no dangling `Started`.
        let mut completed = None;
        while let Ok(ev) = brx.try_recv() {
            if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
                child_id: c,
                succeeded,
                error,
                usage,
                ..
            }) = ev.event
            {
                completed = Some((c, succeeded, error, usage));
            }
        }
        let (c, succeeded, error, usage) =
            completed.expect("Completed lifecycle event after panic");
        assert_eq!(c, child_id);
        assert!(!succeeded);
        assert!(
            error
                .unwrap_or_default()
                .contains("panicked before completing"),
            "lifecycle error must name the panic outcome",
        );
        assert_eq!(usage.input_tokens, 0, "usage is unknown after a panic");
    }

    /// Schema is an explicit per-spawn decision: the `output_schema`
    /// argument flows into the child's loop, which enforces it — the
    /// structured result reaches the parent through the result channel.
    /// (Without the argument the child runs free-form; children never
    /// inherit the parent's schema implicitly.)
    #[tokio::test]
    async fn spawn_output_schema_enforces_structured_output() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"answer": 42}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            // Fallback done-turn in case the runner loops after structured
            // output.
            vec![done_event()],
        ]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({
                "task": "answer the question",
                "model": CATALOG_MODEL,
                "role": "worker",
                "output_schema": {
                    "type": "object",
                    "required": ["answer"],
                    "additionalProperties": false,
                    "properties": { "answer": { "type": "integer" } }
                }
            }),
        )
        .await;

        let result = rx.try_recv().expect("result on the channel");
        assert_eq!(result.agent_id, child_id);
        assert!(result.succeeded, "schema-valid output completes the child");
        assert!(
            result.formatted_message.contains("42"),
            "the structured output must reach the parent: {}",
            result.formatted_message,
        );
    }

    /// R2: a disallowed tool name surfaces as `ToolNotFound` from the
    /// child's executor; the loop falls back to its next turn's text and the
    /// spawn still completes.
    #[tokio::test]
    async fn spawn_agent_tool_subset_gates_disallowed_tools() {
        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                call_id: None,
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "try bash", "model": CATALOG_MODEL, "role": "worker", "tools": ["search"]}),
        )
        .await;
        // The child completed its step — the disallowed tool call did not fail
        // the run. The entry stays observable and wakeable with Idle status.
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("idle child entry stays observable")
                .status,
            AgentStatus::Idle,
        );
    }

    /// R4: a named profile resolved from a temp `.md` file supplies the
    /// child's `LoopContext` system instruction.
    #[test]
    fn build_child_loop_context_uses_profile_body() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let profile_path = dir
            .path()
            .join(".norn")
            .join("profiles")
            .join("researcher.md");
        let Some(profile_parent) = profile_path.parent() else {
            return Err(std::io::Error::other("profile path must have a parent").into());
        };
        std::fs::create_dir_all(profile_parent)?;
        std::fs::write(
            &profile_path,
            "---\nname: researcher\nmodel: gpt-5\ntools: read, grep\n---\nYou are a focused researcher.\n",
        )?;

        let launch_root = dir.path().canonicalize()?;
        let (loop_ctx, tools) =
            build_child_loop_context(None, Some("researcher"), "find the bug", &launch_root)?;
        assert!(
            loop_ctx.system_sections[0].contains("You are a focused researcher."),
            "profile body must become the child's base system instruction",
        );
        assert_eq!(
            tools,
            Some(vec!["read".to_owned(), "grep".to_owned()]),
            "profile's resolved tool list flows back as the allow-list",
        );
        Ok(())
    }

    #[test]
    fn child_profile_resolution_stays_pinned_to_the_launch_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let launch = tempfile::tempdir()?;
        let moved = tempfile::tempdir()?;
        for (root, body) in [
            (launch.path(), "original launch-root profile"),
            (moved.path(), "mutable execution-cwd profile"),
        ] {
            let profile_path = root.join(".norn/profiles/shared.md");
            let Some(profile_parent) = profile_path.parent() else {
                return Err(std::io::Error::other("profile path must have a parent").into());
            };
            std::fs::create_dir_all(profile_parent)?;
            std::fs::write(
                profile_path,
                format!("---\nname: shared\nmodel: gpt-5\n---\n{body}\n"),
            )?;
        }

        let launch_root = launch.path().canonicalize()?;
        let moved_root = moved.path().canonicalize()?;
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(crate::runtime_init::extensions::LaunchWorkingDir(
            launch_root.clone(),
        )));
        ctx.set_working_dir(moved_root);

        let profile_root = resolve_profile_root(&ctx, true)?;
        let (loop_ctx, _) = build_child_loop_context(None, Some("shared"), "task", &profile_root)?;
        let prompt = loop_ctx.base_system_instruction();

        assert_eq!(profile_root, launch_root);
        assert!(prompt.contains("original launch-root profile"));
        assert!(!prompt.contains("mutable execution-cwd profile"));
        Ok(())
    }

    #[test]
    fn build_child_loop_context_rejects_workspace_profile_prompt_commands()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let profile_path = dir
            .path()
            .join(".norn")
            .join("profiles")
            .join("hostile.json");
        let Some(profile_parent) = profile_path.parent() else {
            return Err(std::io::Error::other("profile path must have a parent").into());
        };
        std::fs::create_dir_all(profile_parent)?;
        std::fs::write(
            profile_path,
            r#"{
                "name": "hostile",
                "model": "gpt-5.6-sol",
                "prompt_commands": [{
                    "name": "private",
                    "command": "touch child-profile-command-secret",
                    "cache_ttl": null
                }]
            }"#,
        )?;

        let launch_root = dir.path().canonicalize()?;
        let result = build_child_loop_context(None, Some("hostile"), "task", &launch_root);
        assert!(
            matches!(&result, Err(ToolError::ExecutionFailed { .. })),
            "workspace prompt command must be rejected as ExecutionFailed",
        );
        if let Err(ToolError::ExecutionFailed { reason }) = result {
            assert!(reason.contains("prompt_commands"));
            assert!(!reason.contains("child-profile-command-secret"));
        }
        assert!(!dir.path().join("child-profile-command-secret").exists());
        Ok(())
    }

    /// R4: when no profile is given the child's system instruction embeds
    /// the task itself.
    #[test]
    fn build_child_loop_context_default_embeds_task() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let (loop_ctx, tools) =
            build_child_loop_context(None, None, "summarise the report", dir.path())?;
        assert!(loop_ctx.system_sections[0].contains("summarise the report"));
        assert!(
            tools.is_none(),
            "no profile means no allow-list from a profile"
        );
        Ok(())
    }

    /// R4: an unresolvable profile name surfaces as `ExecutionFailed` — no
    /// silent fallback to a default profile.
    #[test]
    fn build_child_loop_context_unknown_profile_errors() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        // `LoopContext` is not `Debug`, so the `Ok` arm cannot use
        // `expect_err`; match the result explicitly instead.
        let result = build_child_loop_context(None, Some("missing"), "task", dir.path());
        assert!(
            matches!(&result, Err(ToolError::ExecutionFailed { .. })),
            "unknown profile must return ExecutionFailed",
        );
        if let Err(ToolError::ExecutionFailed { reason }) = result {
            assert!(reason.contains("missing"), "{reason}");
        }
        Ok(())
    }

    #[test]
    fn child_profile_errors_do_not_echo_control_characters()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        for name in [
            "sentinel\nnewline",
            "sentinel\u{1b}[31m",
            "sentinel\u{7}bell",
        ] {
            let result = build_child_loop_context(None, Some(name), "task", dir.path());
            assert!(result.is_err(), "unsafe profile name must be rejected");
            if let Err(error) = result {
                let display = error.to_string();
                let debug = format!("{error:?}");
                assert!(!display.contains("sentinel"));
                assert!(!debug.contains("sentinel"));
                assert!(!display.contains(['\n', '\u{1b}', '\u{7}']));
            }
        }
        Ok(())
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
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    /// Permission-escape regression (blocker): the consent-boundary
    /// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must be
    /// forwarded from the parent's context into the child's context —
    /// the child loop resolves both from its own executor's shared
    /// context, so a missing forward disables enforcement entirely.
    #[tokio::test]
    async fn child_context_forwards_permission_policy_and_effect_index() {
        use crate::tool::scheduling::ToolEffectIndex;

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
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
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
            Arc::new(crate::session::SessionBinding::ephemeral_root()),
            test_envelope().child_policy,
            tokio_util::sync::CancellationToken::new(),
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
                call_id: None,
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
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(
            crate::config::permissions::PermissionPolicy::from_patterns(&["victim"], &[], &[]),
        ));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "try the denied tool", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        assert_eq!(
            executions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a tool denied in the parent must never execute inside a spawned child",
        );
        // The child itself still finishes its step and parks idle (the deny
        // surfaces as a blocked tool result, not a child crash).
        assert_eq!(
            agent_registry.read().get(child_id).expect("entry").status,
            AgentStatus::Idle,
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
                call_id: None,
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "probe it", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        let event_store = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .event_store(child_id)
            .expect("child store remains reachable while idle");

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

    /// R3 (ephemeral parent): under an ephemeral parent the child's events
    /// are still reachable through `AgentHandle.event_store`, and the
    /// [`AgentHandles`] accessors expose the store, the provenance
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "produce events", "model": CATALOG_MODEL, "role": "worker"}),
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

        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

        assert!(
            !store_via_accessor.is_empty(),
            "the child produced events the parent can read through the handle",
        );
    }

    /// V2-R2 (persistent parent): the spawned child's store is a REAL
    /// write-through timeline under the root's `children/` dir — the index
    /// row carries `rel_path` + `parent_id`, the child's own run events
    /// land on disk, and the parent's file carries the `ChildBranch`
    /// reservation naming the child (parent-first ordering's durable
    /// record).
    #[tokio::test]
    async fn spawn_under_persistent_parent_persists_child_timeline()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let canonical_output = spawn_non_audio_items("spawn_persisted", "branched child");
        let mut provider_events = canonical_output
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, raw)| Ok(completed_item_event(raw, u64::try_from(index)?)?))
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        provider_events.push(done_event());
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![provider_events]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, manager, root_session_id) = persistent_parent_ctx(
            tmp.path(),
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "branch me", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

        // Index row: rel_path under the root's children/ dir + parent
        // linkage; the file really exists at the nested path.
        let row = manager
            .resolve(&child_id.to_string())
            .expect("child session indexed under the SAME id as its agent");
        let rel = row.rel_path.as_deref().expect("child rows carry rel_path");
        assert!(
            rel.starts_with(&format!("{root_session_id}/children/worker-"))
                && std::path::Path::new(rel)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
            "child file must live under the root's children/ dir: {rel}",
        );
        assert_eq!(row.parent_id.as_deref(), Some(root_session_id.as_str()));
        assert!(
            tmp.path().join(rel).exists(),
            "child timeline file must exist on disk",
        );

        // The child's run events are ON DISK — write-through, not
        // memory-only (Gap 1 closure).
        let child_events = events_on_disk(&manager, &child_id.to_string());
        assert!(
            child_events
                .iter()
                .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
            "the child's file carries its ChildBranch provenance header",
        );
        assert!(
            child_events.iter().any(|e| matches!(
                e,
                SessionEvent::AssistantMessage { content, .. } if content.contains("branched child")
            )),
            "the child's own run output must reach its on-disk timeline",
        );
        assert_eq!(
            canonical_item_values(&child_events),
            canonical_output,
            "the spawned child's completed item must survive its production write-through path",
        );
        let replay_input = stateless_payload_input(&child_events)?;
        assert_eq!(
            canonical_payload_items(&replay_input),
            canonical_output,
            "the spawned child's persisted canonical items must be the exact replay corpus",
        );

        // Parent side ON DISK: the ChildBranch reservation names the child.
        let parent_events = events_on_disk(&manager, &root_session_id);
        assert!(
            parent_events.iter().any(|e| matches!(
                e,
                SessionEvent::ChildBranch {
                    child_session_id: Some(c),
                    path_address,
                    ..
                } if *c == child_id.to_string() && path_address.starts_with("root/worker-")
            )),
            "the parent's file must carry the child's reservation: {parent_events:?}",
        );
        Ok(())
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
            Arc::new(MessageRouter::new()),
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
            json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
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

    async fn wait_for_child_status(ctx: &ToolContext, child_id: Uuid, expected: AgentStatus) {
        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed");
        let mut status_rx = handles.status_rx(child_id).expect("status_rx tracked");
        status_rx
            .wait_for(|status| *status == expected)
            .await
            .expect("child reaches expected status");
    }

    /// Wakeable-spawn regression: a naturally-completed spawned child is
    /// retained as Idle even when [`ReclaimOnResultDelivery`] is installed.
    /// Explicit `close_agent` is the cleanup boundary for persistent
    /// spawned actors.
    #[tokio::test]
    async fn delivered_result_retains_registry_and_handle_when_marker_present() {
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
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
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
        assert!(handles.contains(child_id));
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("idle child retained")
                .status,
            AgentStatus::Idle,
        );
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
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let mut status_rx = handles.status_rx(child_id).expect("status_rx tracked");
        status_rx
            .wait_for(|s| *s == AgentStatus::Idle)
            .await
            .expect("child reaches idle status");

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
            AgentStatus::Idle,
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
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
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
            .wait_for(|s| *s == AgentStatus::Idle)
            .await
            .expect("child reaches idle status");
        assert!(
            handles.contains(child_id),
            "without the marker the external observer owns reclamation",
        );
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("idle entry stays observable for wake")
                .status,
            AgentStatus::Idle,
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
            Arc::new(MessageRouter::new()),
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
                &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
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
            .wait_for(|s| *s == AgentStatus::Idle)
            .await
            .expect("watch reaches idle status");

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
            AgentStatus::Idle,
            "Block suppresses the terminal transition; persistent children park idle",
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
            Arc::new(MessageRouter::new()),
        );

        let mut registry = HookRegistry::new();
        registry.register(Hook::Subagent(Box::new(BlockOnStop)));
        ctx.insert_extension(Arc::new(registry));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
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
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let mut parent_ctx =
            ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/parent-wd")));
        parent_ctx.confine_to_workspace(PathBuf::from("/tmp/workspace-root"));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(crate::session::SessionBinding::ephemeral_root()),
            test_envelope().child_policy,
            tokio_util::sync::CancellationToken::new(),
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
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let parent_ctx = ToolContext::empty();
        let hooks = Arc::new(HookRegistry::new());
        parent_ctx.insert_extension(Arc::clone(&hooks));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(crate::session::SessionBinding::ephemeral_root()),
            test_envelope().child_policy,
            tokio_util::sync::CancellationToken::new(),
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
                call_id: None,
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
            Arc::new(MessageRouter::new()),
        );
        ctx.confine_to_workspace(root.path().to_path_buf());

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "read files", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        let child_store = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .event_store(child_id)
            .expect("child store remains reachable while idle");

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
            Arc::new(MessageRouter::new()),
        );
        ctx.set_working_dir(wd.path().to_path_buf());

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "read rel", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        let child_store = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .event_store(child_id)
            .expect("child store remains reachable while idle");

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
                call_id: None,
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
            Arc::new(MessageRouter::new()),
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
            json!({"task": "probe it", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            1,
            "a parent-registered PreToolUse hook must fire for the child's tool call",
        );
    }

    /// Provider whose stream never yields: the child's run parks inside
    /// the in-flight provider call until cancelled. Counts `stream()`
    /// calls and notifies `entered` on each, so a test can close the
    /// child deterministically mid-call and prove the run never reached
    /// another iteration.
    struct ParkedProvider {
        entered: Arc<tokio::sync::Notify>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for ParkedProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.notify_one();
            Ok(Box::pin(stream::pending::<
                Result<ProviderEvent, ProviderError>,
            >()))
        }
    }

    /// Mid-run close terminates the inner run (HIGH-fix regression): a
    /// child parked inside an in-flight provider call is closed. The
    /// handle's cancellation token must terminate the run itself — not
    /// just the wrapper task — so the run never continues toward natural
    /// completion: the loop's biased select resolves the cancel arm, the
    /// wrapper records the run's REAL outcome (registry `Failed`, typed
    /// `AgentStopReason::Cancelled` on the result channel), and the
    /// closer's job reduces to reclaiming the terminal entry.
    #[tokio::test]
    async fn close_mid_run_cancels_inner_run_and_records_cancelled_outcome() {
        use crate::agent::output::AgentStopReason;
        use crate::tools::agent::coord::CloseAgentTool;

        let entered = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn Provider> = Arc::new(ParkedProvider {
            entered: Arc::clone(&entered),
            calls: Arc::clone(&calls),
        });
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "long haul", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("agent_id")).expect("uuid");

        // Deterministic hook: the child is now inside its first in-flight
        // provider call (`notify_one` stores a permit, so this is
        // race-free regardless of scheduling).
        entered.notified().await;

        let close_out = CloseAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "close-1".to_string(),
                    tool_name: "close_agent".to_string(),
                    model_args: json!({
                        "agent_id": child_id.to_string(),
                        "reason": "stand down",
                    }),
                    metadata: serde_json::Value::Null,
                },
                &ctx,
            )
            .await
            .expect("close executes");

        // The wrapper recorded the run's real outcome and the closer
        // reclaimed the (already terminal) entry — it never had to force
        // a mark of its own.
        assert_eq!(
            close_out.content["shut_down"][0]["status"], "reclaimed",
            "cancellation lets the wrapper finish its own terminal sequence: {:?}",
            close_out.content,
        );
        let reg = agent_registry.read();
        assert!(reg.get(child_id).is_none(), "entry reclaimed by the close");
        let tombstone = reg.tombstone(child_id).expect("tombstone retained");
        assert_eq!(
            tombstone.status,
            AgentStatus::Failed,
            "a cancelled run records Failed — never Completed",
        );
        drop(reg);

        // The run terminated with the cancellation outcome, delivered by
        // the wrapper before the close's join returned.
        let result = rx
            .try_recv()
            .expect("the wrapper delivered the cancelled outcome before the close returned");
        assert_eq!(result.agent_id, child_id);
        assert!(!result.succeeded, "a cancelled run is not a success");
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
        assert!(
            result.error.unwrap_or_default().contains("cancelled"),
            "the failure must name the cancellation",
        );

        // And the inner run did NOT keep executing after the close:
        // exactly one provider call ever started, and the handle is gone.
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the inner run must stop at the cancelled provider call, not \
             continue to further iterations",
        );
        assert!(
            !ctx.get_extension::<AgentHandles>()
                .expect("AgentHandles installed")
                .contains(child_id),
            "the closer takes ownership of the handle",
        );
    }

    /// Production regression (action-log tree): a spawned child inherits
    /// the `action_log` TOOL through the shared registry but previously
    /// received no `ActionLog` extension — every call inside the child
    /// failed with `MissingExtension`. The child now carries its own
    /// per-agent log, so the call succeeds end-to-end, and the parent can
    /// federate over the child's entries with `scope: "all"`.
    #[tokio::test]
    async fn spawned_child_action_log_query_works_and_parent_federates() {
        use crate::session::action_log::ActionLog;
        use crate::tools::action_log::ActionLogTool;

        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-log".to_string(),
                call_id: None,
                name: Some("action_log".to_string()),
                arguments_delta: json!({ "query": "list" }).to_string(),
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
        registry.register(Box::new(ActionLogTool::new()));
        let registry = Arc::new(registry);

        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            registry,
            Arc::new(MessageRouter::new()),
        );
        // The parent has its own action log (as every builder-assembled
        // agent does) so the lazily-installed tree can register its root.
        let parent_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        ctx.insert_extension(Arc::clone(&parent_log));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "task": "inspect your log",
                    "model": CATALOG_MODEL,
                    "role": "worker",
                    "path": "/smoke/child",
                })),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        let child_store = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .event_store(child_id)
            .expect("child store remains reachable while idle");

        // The child's action_log call succeeded — the MissingExtension
        // regression is pinned here.
        let result = child_store
            .events()
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::ToolResult {
                    tool_name, output, ..
                } if tool_name == "action_log" => Some(output),
                _ => None,
            })
            .expect("the child's action_log call produced a result");
        assert!(
            result.get("error").is_none(),
            "the child's action_log query must succeed: {result}",
        );
        assert_eq!(result["query"], "list");
        assert_eq!(
            result["count"], 0,
            "the child's log is its own and starts empty: {result}",
        );

        // Federation: the parent's scope=all sees the child's recorded
        // call, labeled with the child's registry path.
        let federated = ActionLogTool::new()
            .execute(
                &crate::tool::envelope::ToolEnvelope {
                    tool_call_id: "parent-query".to_string(),
                    tool_name: "action_log".to_string(),
                    model_args: json!({ "query": "list", "scope": "all" }),
                    metadata: serde_json::Value::Null,
                },
                &ctx,
            )
            .await
            .expect("parent federated query");
        assert!(!federated.is_error(), "{:?}", federated.content);
        let entries = federated.content["entries"].as_array().unwrap();
        let child_entry = entries
            .iter()
            .find(|e| e["tool"] == "action_log")
            .expect("the child's call surfaces in the parent's scope=all");
        assert_eq!(child_entry["agent"], "/smoke/child");
    }

    /// Route ownership (W3.2): the launch path registers the child's
    /// inbound route at launch and the step wrapper deregisters when the
    /// child parks idle — `signal_agent` reaches a running child without
    /// any tool-side registration, while an idle child queues into its
    /// durable mailbox for `wake_agent`.
    #[tokio::test]
    async fn spawn_registers_route_at_launch_and_deregisters_at_idle() {
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
        let agent_registry = AgentRegistry::shared();
        let router = Arc::new(MessageRouter::new());
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&router),
        );

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "wait", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect("spawn");
        let child_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).expect("uuid");
        assert!(
            router.is_routed(child_id),
            "the launch path must register the child's inbound route",
        );

        gate.notify_one();
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        assert!(
            !router.is_routed(child_id),
            "an idle child must not keep a live inbound route",
        );
    }

    /// Missing-envelope boundary: a context that can spawn but carries no
    /// [`CoordinationEnvelope`] is a wiring error — spawn refuses with a
    /// typed `MissingExtension` naming the envelope, never inventing a
    /// child policy.
    #[tokio::test]
    async fn spawn_requires_coordination_envelope() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("spawn without an envelope must fail typed");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(
                    extension.contains("CoordinationEnvelope"),
                    "error must name the missing envelope: {extension}",
                );
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
        assert!(
            agent_registry.read().list().is_empty(),
            "no reservation may leak from the refused spawn",
        );
    }

    /// `MessagingScope::None` removes `signal_agent` from the child's
    /// surface: the tool definitions shown to the child model exclude it
    /// (with or without an explicit allow-list) while every other tool
    /// survives.
    #[tokio::test]
    async fn spawn_strips_signal_agent_from_child_surface_under_scope_none() {
        use crate::tools::agent::coord::SignalAgentTool;

        for explicit_tools in [None, Some(vec!["signal_agent", "read"])] {
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
            registry.register(Box::new(SignalAgentTool::new()));
            let registry = Arc::new(registry);

            let agent_registry = AgentRegistry::shared();
            let ctx = parent_ctx(
                provider,
                Uuid::new_v4(),
                &agent_registry,
                registry,
                Arc::new(MessageRouter::new()),
            );
            // Replace the standard test envelope with a muted one.
            let mut envelope = test_envelope();
            envelope.child_policy.messaging = MessagingScope::None;
            ctx.insert_extension(Arc::new(envelope));

            let mut args = json!({"task": "quiet work", "model": CATALOG_MODEL, "role": "worker"});
            if let Some(tools) = &explicit_tools {
                args["tools"] = json!(tools);
            }
            let tool = SpawnAgentTool::new();
            spawn_and_join(&tool, &ctx, args).await;

            let names: Vec<String> = captured
                .lock()
                .unwrap()
                .iter()
                .map(|def| match def {
                    ProviderToolDefinition::Function(function) => function.name.clone(),
                    other => panic!("unexpected tool definition: {other:?}"),
                })
                .collect();
            assert!(
                !names.iter().any(|n| n == "signal_agent"),
                "scope none must remove signal_agent (explicit_tools: \
                 {explicit_tools:?}): {names:?}",
            );
            assert!(
                names.iter().any(|n| n == "read"),
                "other tools must survive the strip (explicit_tools: \
                 {explicit_tools:?}): {names:?}",
            );
        }
    }

    /// The spawned child's `AgentToolInfra` carries the granted policy and
    /// the scope-granting parent's event store — the ground truth
    /// `signal_agent` enforces scope from and writes the dual-store audit
    /// to.
    #[tokio::test]
    async fn spawned_child_infra_carries_granted_policy_and_parent_store() {
        struct PolicyProbe {
            seen_scope: Arc<StdMutex<Option<MessagingScope>>>,
            seen_capacity: Arc<StdMutex<Option<usize>>>,
            parent_store_matches: Arc<StdMutex<Option<bool>>>,
            parent_store: Arc<EventStore>,
        }

        #[async_trait]
        impl TestTool for PolicyProbe {
            fn name(&self) -> &'static str {
                "policy_probe"
            }
            fn description(&self) -> &'static str {
                "records the granted policy it sees"
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
                    *self.seen_scope.lock().unwrap() =
                        infra.grant.as_ref().map(|g| g.policy.messaging);
                    *self.seen_capacity.lock().unwrap() =
                        infra.grant.as_ref().map(|g| g.policy.inbound_capacity);
                    *self.parent_store_matches.lock().unwrap() = Some(
                        infra
                            .grant
                            .as_ref()
                            .is_some_and(|g| Arc::ptr_eq(&g.parent_store, &self.parent_store)),
                    );
                }
                Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
            }
        }

        let turn1 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                call_id: None,
                name: Some("policy_probe".to_string()),
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

        let agent_registry = AgentRegistry::shared();
        let parent = Uuid::new_v4();
        let seen_scope = Arc::new(StdMutex::new(None));
        let seen_capacity = Arc::new(StdMutex::new(None));
        let parent_store_matches = Arc::new(StdMutex::new(None));

        // Build the parent ctx first so its infra's event store is the
        // store the probe compares against.
        let ctx = {
            let parent_event_store = Arc::new(EventStore::new());
            let mut registry = ToolRegistry::new();
            registry.register(Box::new(PolicyProbe {
                seen_scope: Arc::clone(&seen_scope),
                seen_capacity: Arc::clone(&seen_capacity),
                parent_store_matches: Arc::clone(&parent_store_matches),
                parent_store: Arc::clone(&parent_event_store),
            }));
            let infra = Arc::new(AgentToolInfra {
                registry: Arc::clone(&agent_registry),
                router: Arc::new(MessageRouter::new()),
                pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
                provider,
                event_store: parent_event_store,
                agent_id: parent,
                parent_id: None,
                grant: None,
                tool_registry: Some(Arc::new(registry)),
                session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
            });
            let ctx = ToolContext::empty();
            ctx.insert_extension(infra);
            ctx.insert_extension(Arc::new(AgentHandles::new()));
            ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
            let mut envelope = test_envelope();
            envelope.child_policy.messaging = MessagingScope::ParentOnly;
            envelope.child_policy.inbound_capacity = 7;
            ctx.insert_extension(Arc::new(envelope));
            ctx
        };

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "introspect", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        assert_eq!(
            *seen_scope.lock().unwrap(),
            Some(MessagingScope::ParentOnly),
            "the child must carry the envelope's granted messaging scope",
        );
        assert_eq!(
            *seen_capacity.lock().unwrap(),
            Some(7),
            "the child must carry the envelope's inbound capacity",
        );
        assert_eq!(
            *parent_store_matches.lock().unwrap(),
            Some(true),
            "the child's parent_store must be the spawning parent's event store",
        );
    }

    // -- W3.4: budgeted recursive delegation --------------------------------

    /// A caller whose own granted budget has `remaining_depth = 0` may not
    /// spawn at all: typed, honest refusal naming the budget, and nothing
    /// is reserved.
    #[tokio::test]
    async fn spawn_refused_when_caller_depth_exhausted() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 0;
        ctx.insert_extension(Arc::new(envelope));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
                &ctx,
            )
            .await
            .expect_err("a zero-depth caller must be refused");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("delegation depth exhausted"),
                    "the refusal names the budget: {reason}",
                );
                assert!(
                    reason.contains("remaining_depth = 0"),
                    "the refusal states the budget value: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
        let reg = agent_registry.read();
        assert!(reg.is_empty(), "a refused spawn reserves nothing");
        assert!(reg.tombstones().is_empty(), "and leaves no tombstone");
    }

    /// A `child_policy` argument that widens the caller's own grant is
    /// refused typed (per field), naming the caller's budget; a valid
    /// narrowing is stamped on the registry entry verbatim.
    /// A typo'd top-level key must fail loudly — silently dropping a
    /// misspelled `child_policy` would hand the child a default grant
    /// where the caller intended a narrowing.
    #[tokio::test]
    async fn spawn_rejects_unknown_arg_keys() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(test_envelope()));
        let tool = SpawnAgentTool::new();

        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "x", "model": CATALOG_MODEL, "role": "worker",
                    "child_polciy": { "inbound_capacity": 32 },
                })),
                &ctx,
            )
            .await
            .expect_err("a typo'd key must not be silently dropped");
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("child_polciy") || rendered.contains("unknown field"),
            "the failure names the unknown key: {rendered}",
        );
    }

    /// U2-M1 regression: an `output_schema` declaring a reserved envelope
    /// key as a top-level property is refused synchronously at the
    /// argument boundary — required collisions would make the child's
    /// schema unsatisfiable (the key is stripped before validation) and
    /// optional ones silently lossy. The failure names the key.
    #[tokio::test]
    async fn spawn_rejects_output_schema_with_reserved_envelope_key() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(test_envelope()));
        let tool = SpawnAgentTool::new();

        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "x", "model": CATALOG_MODEL, "role": "worker",
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "answer": { "type": "string" },
                            "tool_use_description": { "type": "string" }
                        },
                        "required": ["answer", "tool_use_description"],
                        "additionalProperties": false
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("a reserved-key schema must be refused, not silently mangled");
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("tool_use_description") && rendered.contains("reserved"),
            "the failure names the colliding key and the convention: {rendered}",
        );
    }

    #[tokio::test]
    async fn spawn_child_policy_narrowing_enforced() {
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
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));
        let tool = SpawnAgentTool::new();

        // Depth widened (equal to the caller's own — not strictly less).
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "x", "model": CATALOG_MODEL, "role": "worker",
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": {"remaining_depth": 2, "max_concurrent_children": 32},
                        "inbound_capacity": 32,
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("equal depth is a widening");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("remaining_depth = 2 exceeds") && reason.contains("at most 1"),
                    "names the strict decrement: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }

        // Inbound capacity widened.
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "x", "model": CATALOG_MODEL, "role": "worker",
                    "child_policy": {
                        "messaging": "parent_only",
                        "delegation": {"remaining_depth": 0, "max_concurrent_children": 1},
                        "inbound_capacity": 33,
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("inbound capacity widening is refused");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("inbound_capacity = 33 exceeds"),
                    "names the violation: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
        assert!(
            agent_registry.read().is_empty(),
            "refused narrowings reserve nothing",
        );

        // A valid narrowing is accepted and stamped verbatim.
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "child_policy": {
                    "messaging": "parent_only",
                    "delegation": {"remaining_depth": 1, "max_concurrent_children": 2},
                    "inbound_capacity": 8,
                },
            }),
        )
        .await;
        let entry = agent_registry
            .read()
            .get(child_id)
            .expect("terminal entry retained without reclaim marker");
        assert_eq!(entry.policy.messaging, MessagingScope::ParentOnly);
        assert_eq!(entry.policy.delegation.remaining_depth, 1);
        assert_eq!(entry.policy.delegation.max_concurrent_children, 2);
        assert_eq!(entry.policy.inbound_capacity, 8);
    }

    /// Omitting `child_policy` grants the caller's own policy with the
    /// delegation depth decremented one level, and the auto path nests
    /// under the spawning agent's registered path.
    #[tokio::test]
    async fn spawn_stamps_decremented_grant_and_nested_path() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]));
        let agent_registry = AgentRegistry::shared();
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;

        // Register the spawner itself first (the CLI does this for its
        // root) so the auto path has a prefix to nest under, then key the
        // spawning context to the registered id.
        let guard = AgentRegistry::reserve(
            &agent_registry,
            "/lead".to_string(),
            "lead".to_string(),
            "opus".to_string(),
            None,
            envelope.child_policy.clone(),
            None,
        )
        .expect("register spawner");
        let registered_parent = guard.id();
        guard.confirm().expect("confirm");

        let ctx = parent_ctx(
            provider,
            registered_parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(envelope.clone()));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let entry = agent_registry
            .read()
            .get(child_id)
            .expect("terminal entry retained without reclaim marker");
        assert!(
            entry.path.starts_with("/lead/spawn/"),
            "auto path nests under the spawner: {}",
            entry.path,
        );
        assert_eq!(entry.parent_id, Some(registered_parent));
        assert_eq!(
            entry.policy.delegation.remaining_depth, 1,
            "the default derivation decrements the caller's depth 2 to 1",
        );
        assert_eq!(entry.policy.messaging, envelope.child_policy.messaging);
        assert_eq!(
            entry.policy.delegation.max_concurrent_children,
            envelope.child_policy.delegation.max_concurrent_children,
        );
        assert_eq!(
            entry.policy.inbound_capacity,
            envelope.child_policy.inbound_capacity,
        );
    }

    /// A leaf child (granted depth 0) that tries to spawn is refused by
    /// the registry budget with the typed message, the grandchild is never
    /// registered, and the child still completes normally.
    #[tokio::test]
    async fn leaf_child_spawn_attempt_refused_and_run_completes() {
        // Child script: call spawn_agent (refused — the tool error is
        // injected as the tool result), then finish.
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc1".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "grandchild", "model": CATALOG_MODEL, "role": "leaf",
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "stopping at my budget".to_string(),
                },
                done_event(),
            ],
        ]));
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        // Envelope depth 1: the child is a leaf (granted depth 0).
        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "try to delegate", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let reg = agent_registry.read();
        let entry = reg.get(child_id).expect("child entry retained");
        assert_eq!(
            entry.status,
            AgentStatus::Idle,
            "the child completed and idled"
        );
        assert_eq!(entry.policy.delegation.remaining_depth, 0, "leaf grant");
        assert_eq!(
            reg.len(),
            1,
            "the grandchild must never be registered: {:?}",
            reg.list(),
        );
        assert!(reg.tombstones().is_empty(), "nothing was reclaimed");
    }

    fn idle_grandchild_entry(
        registry: &RwLock<AgentRegistry>,
    ) -> Option<crate::agent::registry::AgentEntry> {
        registry.read().list().into_iter().find(|entry| {
            entry.path.matches("/spawn/").count() == 2 && entry.status == AgentStatus::Idle
        })
    }

    /// Routes provider scripts by conversation identity (the first user
    /// message) so a mid-tree child and its grandchild can share the one
    /// workspace provider deterministically; the child's would-stop turn
    /// is held until the registry shows the grandchild parked idle, which
    /// guarantees its result is already in the child's channel.
    struct TreeProvider {
        registry: Arc<RwLock<AgentRegistry>>,
        child_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for TreeProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            use std::sync::atomic::Ordering as AtomicOrdering;
            // The grandchild's run always ends its request with its own
            // task prompt ("grandchild-task"); every child turn ends with
            // something else (the child's prompt, a tool result, or an
            // injected <agent_result> frame). Note the *first* user
            // message would be wrong here: in session-tree mode a spawned
            // child's branch store is seeded with its parent's context,
            // so every conversation in the tree starts with the root
            // prompt.
            // The managed dynamic-context Developer message now rides at the
            // tail of every request (prompt-cache fix), so route on the last
            // non-Developer message — the turn content that actually seeds
            // this child.
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            if last == "grandchild-task" {
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "grandchild says hi".to_string(),
                    }),
                    Ok(done_event()),
                ])));
            }
            let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
            match call {
                0 => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-grandchild".to_string(),
                        call_id: None,
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "grandchild-task",
                            "model": CATALOG_MODEL,
                            "role": "leaf",
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_event_tool_use()),
                ]))),
                1 => {
                    let registry = Arc::clone(&self.registry);
                    let s = stream::once(async move {
                        for _ in 0..2400 {
                            if idle_grandchild_entry(&registry).is_some() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        panic!("grandchild never parked idle — the test cannot proceed");
                    })
                    .flat_map(|()| {
                        stream::iter(vec![
                            Ok(ProviderEvent::TextDelta {
                                text: "waited for grandchild".to_string(),
                            }),
                            Ok(done_event()),
                        ])
                    });
                    Ok(Box::pin(s))
                }
                _ => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "child done after grandchild".to_string(),
                    }),
                    Ok(done_event()),
                ]))),
            }
        }
    }

    /// W3.4 end-to-end: with an envelope granting depth 2, a spawned child
    /// spawns a grandchild; the grandchild's result is delivered into the
    /// **child's** conversation (one hop — never to the root), the child's
    /// own result reaches the root's channel, the agents tree nests, and
    /// every spawned actor remains idle and addressable.
    #[tokio::test]
    async fn grandchild_results_bubble_one_hop_and_idle_at_every_level() {
        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(TreeProvider {
            registry: Arc::clone(&agent_registry),
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        let root_id = Uuid::new_v4();
        // Persistent parent (V2-R2): child and grandchild conversations
        // land on REAL on-disk timelines, readable after reclamation —
        // and Gap 11's depth-2 traffic is asserted from disk below.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (ctx, manager, root_session_id) = persistent_parent_ctx(
            tmp.path(),
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));

        // Root result channel + delivery-anchored marker. Persistent
        // spawned children deliberately ignore the marker on natural
        // completion so they remain wakeable.
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn child");
        assert!(!out.is_error(), "{:?}", out.content);
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");
        let child_path = out.content["path"].as_str().expect("path").to_string();
        assert!(child_path.starts_with("/spawn/"), "{child_path}");

        // The root receives exactly one result: the child's — the
        // grandchild's bubbled one hop, never skipping a level.
        let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("child result must arrive")
            .expect("channel open");
        assert_eq!(child_result.agent_id, child_id);
        assert!(child_result.succeeded, "{:?}", child_result.error);
        assert!(
            child_result
                .formatted_message
                .contains("child done after grandchild"),
            "the child's final answer is the delivered result: {}",
            child_result.formatted_message,
        );
        assert!(
            rx.try_recv().is_err(),
            "the grandchild's result must never reach the root directly",
        );

        // Wakeable tree at every level: both entries remain observable,
        // idle, and correctly linked.
        wait_for_condition(
            || {
                let reg = agent_registry.read();
                let child_idle = reg
                    .get(child_id)
                    .is_some_and(|entry| entry.status == AgentStatus::Idle);
                let grandchild_idle = reg.children(child_id).iter().any(|entry| {
                    entry.status == AgentStatus::Idle
                        && entry.path.starts_with(&format!("{child_path}/spawn/"))
                });
                child_idle && grandchild_idle
            },
            "child and grandchild must both park idle",
        )
        .await;
        let reg = agent_registry.read();
        assert_eq!(
            reg.tombstones().len(),
            0,
            "natural idle creates no tombstones"
        );
        let child_entry = reg.get(child_id).expect("child entry retained");
        assert_eq!(child_entry.parent_id, Some(root_id));
        assert_eq!(child_entry.status, AgentStatus::Idle);
        let grandchild_entry = reg
            .children(child_id)
            .into_iter()
            .find(|entry| entry.path.starts_with(&format!("{child_path}/spawn/")))
            .expect("grandchild entry retained");
        assert_eq!(
            grandchild_entry.parent_id,
            Some(child_id),
            "the grandchild's parent is the mid-tree child, not the root",
        );
        assert_eq!(grandchild_entry.status, AgentStatus::Idle);
        assert!(
            grandchild_entry
                .path
                .starts_with(&format!("{child_path}/spawn/")),
            "grandchild path nests under the child: {}",
            grandchild_entry.path,
        );
        drop(reg);

        // One-hop delivery into the child's conversation, DURABLY: the
        // child's on-disk session holds the framed grandchild result
        // (Gap 1 + Gap 11 closure asserted from disk, not memory).
        let rows = crate::session::persistence::index::read_index(manager.data_dir())
            .expect("index reads");
        let child_row = rows
            .iter()
            .find(|r| r.parent_id.as_deref() == Some(root_session_id.as_str()))
            .expect("child session row indexed under the root");
        let child_events = events_on_disk(&manager, &child_row.id);
        let injected = child_events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_result")
                        && content.contains("grandchild says hi")
            )
        });
        assert!(
            injected,
            "the grandchild's framed result must be durably injected into \
             the child's conversation",
        );
        // And the grandchild's session persisted under the child, keyed by
        // the full-path slug in the SAME root-keyed children/ dir.
        let grandchild_row = rows
            .iter()
            .find(|r| r.parent_id.as_deref() == Some(child_row.id.as_str()))
            .expect("grandchild session row indexed under the child");
        let grandchild_rel = grandchild_row.rel_path.as_deref().expect("rel_path");
        assert!(
            grandchild_rel.starts_with(&format!("{root_session_id}/children/"))
                && grandchild_rel.contains("--"),
            "grandchild file must be keyed by the full path slug: {grandchild_rel}",
        );
        assert!(
            tmp.path().join(grandchild_rel).exists(),
            "grandchild timeline file must exist on disk",
        );
        assert!(
            !events_on_disk(&manager, &grandchild_row.id).is_empty(),
            "the grandchild's own events must reach disk (Gap 11)",
        );
    }

    /// `Done` event with explicit usage so each tree level reports
    /// distinct token counts (W3.6 rollup tests).
    fn done_with(stop_reason: StopReason, input: u64, output: u64) -> ProviderEvent {
        ProviderEvent::Done {
            stop_reason,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
            response_id: None,
        }
    }

    /// W3.6 provider: routes like [`TreeProvider`] but stamps distinct
    /// usage on every level and call — grandchild (7, 3); child calls
    /// (100, 50), (200, 60), (300, 70) — so any double count or dropped
    /// level changes the totals. The child's would-stop turn is held
    /// until the registry shows the grandchild parked idle, guaranteeing
    /// its result (and its `subtree_usage`) is already in the child's
    /// channel when the boundary sweep folds it.
    struct UsageTreeProvider {
        registry: Arc<RwLock<AgentRegistry>>,
        child_calls: Arc<std::sync::atomic::AtomicUsize>,
        /// Tool call the third child turn emits: `None` stops with text
        /// (rollup test); `Some(name)` calls that tool (panic test).
        third_turn_tool: Option<&'static str>,
    }

    impl Provider for UsageTreeProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            use std::sync::atomic::Ordering as AtomicOrdering;
            // The managed dynamic-context Developer message now rides at the
            // tail of every request (prompt-cache fix), so route on the last
            // non-Developer message — the turn content that actually seeds
            // this child.
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            if last == "usage-grandchild-task" {
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "grandchild usage report".to_string(),
                    }),
                    Ok(done_with(StopReason::EndTurn, 7, 3)),
                ])));
            }
            let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
            match call {
                0 => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-usage-grandchild".to_string(),
                        call_id: None,
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "usage-grandchild-task",
                            "model": CATALOG_MODEL,
                            "role": "leaf",
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_with(StopReason::ToolUse, 100, 50)),
                ]))),
                1 => {
                    let registry = Arc::clone(&self.registry);
                    let s = stream::once(async move {
                        for _ in 0..2400 {
                            if idle_grandchild_entry(&registry).is_some() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        panic!("grandchild never parked idle — the test cannot proceed");
                    })
                    .flat_map(|()| {
                        stream::iter(vec![
                            Ok(ProviderEvent::TextDelta {
                                text: "waited for grandchild".to_string(),
                            }),
                            Ok(done_with(StopReason::EndTurn, 200, 60)),
                        ])
                    });
                    Ok(Box::pin(s))
                }
                _ => match self.third_turn_tool {
                    Some(tool_name) => Ok(Box::pin(stream::iter(vec![
                        Ok(ProviderEvent::ToolCallDelta {
                            item_id: "tc-third-turn".to_string(),
                            call_id: None,
                            name: Some(tool_name.to_string()),
                            arguments_delta: "{}".to_string(),
                            kind: crate::provider::request::ToolCallKind::Function,
                        }),
                        Ok(done_with(StopReason::ToolUse, 300, 70)),
                    ]))),
                    None => Ok(Box::pin(stream::iter(vec![
                        Ok(ProviderEvent::TextDelta {
                            text: "child done after grandchild".to_string(),
                        }),
                        Ok(done_with(StopReason::EndTurn, 300, 70)),
                    ]))),
                },
            }
        }
    }

    /// W3.6 acceptance (rollup): a depth-2 tree with distinct synthetic
    /// usage at each level sums **exactly once** at the root. The
    /// grandchild's lifecycle reports `subtree_usage == own (7, 3)`; the
    /// child's own `usage` stays own-calls-only (600, 180) — proving the
    /// drained grandchild subtree was never folded into it — and the
    /// root receives `subtree_usage == (607, 183) == Σ` both levels,
    /// each counted once.
    #[tokio::test]
    async fn depth2_subtree_usage_sums_each_level_exactly_once_at_the_root() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
        };

        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(UsageTreeProvider {
            registry: Arc::clone(&agent_registry),
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            third_turn_tool: None,
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        let root_id = Uuid::new_v4();
        let ctx = parent_ctx(
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));
        let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn child");
        assert!(!out.is_error(), "{:?}", out.content);
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");

        let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("child result must arrive")
            .expect("channel open");
        assert_eq!(child_result.agent_id, child_id);
        assert!(child_result.succeeded, "{:?}", child_result.error);

        // Own usage stays own-calls-only: 100+200+300 / 50+60+70.
        assert_eq!(
            child_result.usage.input_tokens, 600,
            "the child's own usage must never absorb the grandchild's",
        );
        assert_eq!(child_result.usage.output_tokens, 180);
        // Subtree = child own + grandchild own, each exactly once.
        assert_eq!(
            child_result.subtree_usage.input_tokens, 607,
            "root must receive Σ of both levels, each counted once",
        );
        assert_eq!(child_result.subtree_usage.output_tokens, 183);

        // Lifecycle carrier agrees at every level: the grandchild (a
        // leaf) reports subtree == own (7, 3); the child reports its own
        // usage and the folded subtree total.
        let mut completed: Vec<(Uuid, Usage, Usage)> = Vec::new();
        while let Ok(ev) = brx.try_recv() {
            if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
                child_id: c,
                usage,
                subtree_usage,
                ..
            }) = ev.event
            {
                completed.push((c, usage, subtree_usage));
            }
        }
        assert_eq!(completed.len(), 2, "grandchild + child lifecycles");
        let (_, grandchild_usage, grandchild_subtree) = completed
            .iter()
            .find(|(c, _, _)| *c != child_id)
            .expect("grandchild Completed");
        assert_eq!(grandchild_usage.input_tokens, 7);
        assert_eq!(grandchild_usage.output_tokens, 3);
        assert_eq!(
            grandchild_subtree.input_tokens, 7,
            "a leaf's subtree usage equals its own usage",
        );
        assert_eq!(grandchild_subtree.output_tokens, 3);
        let (_, child_usage, child_subtree) = completed
            .iter()
            .find(|(c, _, _)| *c == child_id)
            .expect("child Completed");
        assert_eq!(child_usage.input_tokens, 600);
        assert_eq!(child_usage.output_tokens, 180);
        assert_eq!(child_subtree.input_tokens, 607);
        assert_eq!(child_subtree.output_tokens, 183);
    }

    /// W3.6 acceptance (honest zeros): a mid-tree child that panics
    /// AFTER its loop delivered the grandchild's result reports its own
    /// usage as unknown-zeros while the grandchild's `subtree_usage`
    /// still rolls up — partial truth beats silent loss. The shared
    /// accumulator survives the unwound loop task; nothing is invented
    /// for the child's own spend.
    #[tokio::test]
    async fn panicked_mid_tree_child_still_rolls_up_delivered_grandchild_usage() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
        };

        /// Stands in for a panicking dependency inside the child's run.
        struct ExplodingTool;

        #[async_trait]
        impl TestTool for ExplodingTool {
            fn name(&self) -> &'static str {
                "explode"
            }
            fn description(&self) -> &'static str {
                "panics on execute (test stand-in for a panicking dependency)"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({})
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<TestToolOutput, ToolError> {
                panic!("dependency panic after the grandchild result was delivered");
            }
        }

        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(UsageTreeProvider {
            registry: Arc::clone(&agent_registry),
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            third_turn_tool: Some("explode"),
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        tool_registry.register(Box::new(ExplodingTool));
        let root_id = Uuid::new_v4();
        let ctx = parent_ctx(
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));
        let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
        ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn child");
        assert!(!out.is_error(), "{:?}", out.content);
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");

        let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("child result must arrive")
            .expect("channel open");
        assert_eq!(child_result.agent_id, child_id);
        assert!(!child_result.succeeded, "the panicked child must fail");
        assert!(
            child_result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("panicked before completing"),
            "the panic must surface honestly: {:?}",
            child_result.error,
        );
        // Own usage: unknown-zeros — the panicked task took it along.
        assert_eq!(
            child_result.usage.input_tokens, 0,
            "own usage is unknown after a panic — zeros, never invented",
        );
        assert_eq!(child_result.usage.output_tokens, 0);
        // Delivered grandchild subtree still present in the rollup.
        assert_eq!(
            child_result.subtree_usage.input_tokens, 7,
            "the grandchild's delivered subtree must survive the panic",
        );
        assert_eq!(child_result.subtree_usage.output_tokens, 3);

        // The lifecycle carrier agrees: the child's Completed reports
        // zeros for its own usage with the grandchild subtree intact.
        let mut child_completed = None;
        while let Ok(ev) = brx.try_recv() {
            if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
                child_id: c,
                usage,
                subtree_usage,
                succeeded,
                ..
            }) = ev.event
                && c == child_id
            {
                child_completed = Some((usage, subtree_usage, succeeded));
            }
        }
        let (usage, subtree_usage, succeeded) =
            child_completed.expect("child Completed lifecycle after panic");
        assert!(!succeeded);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(subtree_usage.input_tokens, 7);
        assert_eq!(subtree_usage.output_tokens, 3);
    }

    /// Provider for the W3.5 cascade trees: the mid-tree child's first
    /// call emits a `spawn_agent` tool call for the grandchild; every
    /// later child call — and the grandchild's only call — parks inside
    /// a never-yielding stream, notifying the matching `Notify` so the
    /// test knows both runs are mid-flight before cancelling. Routes by
    /// last message exactly like `TreeProvider` above.
    struct CascadeTreeProvider {
        child_calls: Arc<std::sync::atomic::AtomicUsize>,
        grandchild_calls: Arc<std::sync::atomic::AtomicUsize>,
        child_parked: Arc<tokio::sync::Notify>,
        grandchild_parked: Arc<tokio::sync::Notify>,
    }

    impl Provider for CascadeTreeProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            use std::sync::atomic::Ordering as AtomicOrdering;
            // The managed dynamic-context Developer message now rides at the
            // tail of every request (prompt-cache fix), so route on the last
            // non-Developer message — the turn content that actually seeds
            // this child.
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            if last == "grandchild-task" {
                self.grandchild_calls.fetch_add(1, AtomicOrdering::SeqCst);
                self.grandchild_parked.notify_one();
                return Ok(Box::pin(stream::pending::<
                    Result<ProviderEvent, ProviderError>,
                >()));
            }
            let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
            if call == 0 {
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-grandchild".to_string(),
                        call_id: None,
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "grandchild-task",
                            "model": CATALOG_MODEL,
                            "role": "leaf",
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_event_tool_use()),
                ])));
            }
            self.child_parked.notify_one();
            Ok(Box::pin(stream::pending::<
                Result<ProviderEvent, ProviderError>,
            >()))
        }
    }

    /// A depth-2 tree (root context → child → grandchild) with both runs
    /// parked inside in-flight provider calls, ready for a cascade test.
    struct ParkedDepth2Tree {
        ctx: ToolContext,
        agent_registry: Arc<RwLock<AgentRegistry>>,
        rx: tokio::sync::mpsc::Receiver<crate::agent::result_channel::ChildAgentResult>,
        root_id: Uuid,
        child_id: Uuid,
        grandchild_id: Uuid,
        grandchild_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Builds the depth-2 tree: a root context (publishing `root_cancel`
    /// as its [`AgentCancellation`] when given, token-less otherwise)
    /// with delivery-anchored reclamation, an envelope granting depth 2,
    /// a spawned child that spawns a grandchild, and both runs parked
    /// mid-provider-call (deterministic — `notify_one` stores permits).
    async fn parked_depth2_tree(
        root_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> ParkedDepth2Tree {
        let agent_registry = AgentRegistry::shared();
        let child_parked = Arc::new(tokio::sync::Notify::new());
        let grandchild_parked = Arc::new(tokio::sync::Notify::new());
        let grandchild_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn Provider> = Arc::new(CascadeTreeProvider {
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            grandchild_calls: Arc::clone(&grandchild_calls),
            child_parked: Arc::clone(&child_parked),
            grandchild_parked: Arc::clone(&grandchild_parked),
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        let root_id = Uuid::new_v4();
        let ctx = parent_ctx(
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));
        if let Some(token) = root_cancel {
            ctx.insert_extension(Arc::new(AgentCancellation(token)));
        }
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn child");
        assert!(!out.is_error(), "{:?}", out.content);
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");

        tokio::time::timeout(Duration::from_secs(60), async {
            grandchild_parked.notified().await;
            child_parked.notified().await;
        })
        .await
        .expect("both the child and the grandchild must park mid-call");

        let grandchild_id = {
            let reg = agent_registry.read();
            let children = reg.children(child_id);
            assert_eq!(children.len(), 1, "exactly one grandchild registered");
            children[0].id
        };

        ParkedDepth2Tree {
            ctx,
            agent_registry,
            rx,
            root_id,
            child_id,
            grandchild_id,
            grandchild_calls,
        }
    }

    /// W3.5 cooperative cascade end-to-end: with the root's token
    /// published, cancelling the ROOT token alone terminates a depth-2
    /// subtree mid-run — the child's and the grandchild's runs both end
    /// at their next cancellation boundary with the real `Cancelled`
    /// outcome, every wrapper performs its own terminal sequence (honest
    /// `Failed` records at every level, lineage intact), and the whole
    /// subtree reclaims: no dangling `Started`, no leaked entries, no
    /// aborted tasks. The grandchild's result lands in the cancelled
    /// child's channel (delivered, or error-logged when the child's loop
    /// already dropped its receiver — never silent), so the root sees
    /// exactly one result: the child's.
    #[tokio::test]
    async fn cancelling_root_token_cascades_to_depth2_subtree_with_honest_outcomes() {
        use crate::agent::output::AgentStopReason;

        let root_cancel = tokio_util::sync::CancellationToken::new();
        let mut tree = parked_depth2_tree(Some(root_cancel.clone())).await;

        root_cancel.cancel();

        // The child's wrapper delivers the run's real outcome to the
        // root's channel — cancellation yields an accounted tree.
        let result = tokio::time::timeout(Duration::from_secs(60), tree.rx.recv())
            .await
            .expect("child result must arrive after the cascade")
            .expect("channel open");
        assert_eq!(result.agent_id, tree.child_id);
        assert!(!result.succeeded, "a cancelled run is not a success");
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

        // Whole-subtree reclamation under cascade (the W3.4 machinery at
        // depth 2): every entry leaves the registry, every level keeps an
        // honest Failed tombstone with intact parent links.
        wait_for_condition(
            || tree.agent_registry.read().is_empty(),
            "registry must fully reclaim after a root-token cascade",
        )
        .await;
        let reg = tree.agent_registry.read();
        let tombstones = reg.tombstones();
        assert_eq!(tombstones.len(), 2, "child + grandchild: {tombstones:?}");
        let child_tomb = tombstones
            .iter()
            .find(|t| t.id == tree.child_id)
            .expect("child tombstone");
        assert_eq!(
            child_tomb.status,
            AgentStatus::Failed,
            "a cancelled run records Failed — never Completed",
        );
        assert_eq!(child_tomb.parent_id, Some(tree.root_id));
        let grandchild_tomb = tombstones
            .iter()
            .find(|t| t.id == tree.grandchild_id)
            .expect("grandchild tombstone");
        assert_eq!(grandchild_tomb.status, AgentStatus::Failed);
        assert_eq!(
            grandchild_tomb.parent_id,
            Some(tree.child_id),
            "lineage survives reclamation at every level",
        );
        drop(reg);

        // The grandchild's run actually ended: its provider was entered
        // exactly once and never again after the cascade.
        assert_eq!(
            tree.grandchild_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the grandchild's run must end at the cascaded cancel",
        );
        // No result ever skipped a level to the root.
        assert!(
            tree.rx.try_recv().is_err(),
            "the grandchild's result must never reach the root directly",
        );
        // The parent-held child handle was reclaimed by its wrapper.
        assert!(
            tree.ctx
                .get_extension::<AgentHandles>()
                .expect("handles")
                .is_empty(),
            "no handle may leak after the cascade",
        );
    }

    /// W3.5 forced cascade at depth: `close_agent` on a MID-TREE agent —
    /// the closer holds only the target's handle, never the grandchild's
    /// — fires the target's token before the walk, which cascades to the
    /// grandchild through token parentage. The close returns only after
    /// the TARGET's wrapper completes (its Cancelled result is already
    /// on the root's channel when the tool returns); the grandchild is
    /// reported honestly ("cancelling", or a terminal label when its own
    /// wrapper wins the race — never "unreachable") and terminates
    /// through its own wrapper without close touching it. Leaves-first
    /// ordering holds, and the whole subtree reclaims with honest Failed
    /// records.
    ///
    /// The root context here deliberately publishes NO
    /// [`AgentCancellation`] — additionally pinning that the cascade
    /// below depth 1 works under a token-less embedder root, because the
    /// child's own token is published at its context construction either
    /// way.
    #[tokio::test]
    async fn close_mid_tree_cascades_to_grandchild_and_returns_after_target_wrapper() {
        use crate::agent::output::AgentStopReason;
        use crate::tools::agent::coord::CloseAgentTool;

        let mut tree = parked_depth2_tree(None).await;

        let close_out = CloseAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "close-1".to_string(),
                    tool_name: "close_agent".to_string(),
                    model_args: json!({
                        "agent_id": tree.child_id.to_string(),
                        "reason": "stand down",
                    }),
                    metadata: serde_json::Value::Null,
                },
                &tree.ctx,
            )
            .await
            .expect("close executes");
        assert!(!close_out.is_error(), "{:?}", close_out.content);

        // Leaves-first: the grandchild is reported before the target.
        let shut_down = close_out.content["shut_down"].as_array().expect("array");
        assert_eq!(shut_down.len(), 2, "{shut_down:?}");
        assert_eq!(shut_down[0]["agent_id"], tree.grandchild_id.to_string());
        assert_eq!(shut_down[1]["agent_id"], tree.child_id.to_string());

        // Never "unreachable" under a cascade: the grandchild's token
        // was cancelled before the walk, so close reports the truth —
        // cancelling (live, its wrapper finishing) or a terminal label
        // when its wrapper won the race.
        let grandchild_status = shut_down[0]["status"].as_str().expect("status");
        assert!(
            ["cancelling", "reclaimed", "already_completed"].contains(&grandchild_status),
            "cascade-reached grandchild must not be reported unreachable: {grandchild_status}",
        );
        // The target's wrapper completed before close returned, recording
        // the run's real outcome itself.
        let child_status = shut_down[1]["status"].as_str().expect("status");
        assert!(
            ["reclaimed", "already_completed"].contains(&child_status),
            "the cancelled target's wrapper owns its terminal sequence: {child_status}",
        );

        // Join-at-depth pin: the target's result was delivered before the
        // close's join returned — try_recv, no awaiting.
        let result = tree
            .rx
            .try_recv()
            .expect("the target's result must be delivered before close returns");
        assert_eq!(result.agent_id, tree.child_id);
        assert!(!result.succeeded);
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

        // The grandchild terminates through its own wrapper — close never
        // held its handle — and the subtree fully reclaims with honest
        // Failed records at both levels.
        wait_for_condition(
            || tree.agent_registry.read().is_empty(),
            "subtree must fully reclaim after a mid-tree close",
        )
        .await;
        let reg = tree.agent_registry.read();
        let tombstones = reg.tombstones();
        assert_eq!(tombstones.len(), 2, "{tombstones:?}");
        for id in [tree.child_id, tree.grandchild_id] {
            let tomb = tombstones
                .iter()
                .find(|t| t.id == id)
                .expect("tombstone at every level");
            assert_eq!(
                tomb.status,
                AgentStatus::Failed,
                "honest Failed at every level — never Completed, no force marks",
            );
        }
        drop(reg);
        assert_eq!(
            tree.grandchild_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the grandchild's run must end at the cascaded cancel, not re-enter the provider",
        );
    }

    /// Root boundary pin (W3.5): a parent context that publishes no
    /// [`AgentCancellation`] still launches children — with free-standing
    /// run tokens, exactly the pre-cascade behavior — and the child's own
    /// handle token remains fully functional: cancelling it ends the run
    /// with the real Cancelled outcome through the wrapper.
    #[tokio::test]
    async fn root_without_published_token_launches_free_standing_children() {
        use crate::agent::output::AgentStopReason;

        let entered = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn Provider> = Arc::new(ParkedProvider {
            entered: Arc::clone(&entered),
            calls: Arc::clone(&calls),
        });
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        assert!(
            ctx.get_extension::<AgentCancellation>().is_none(),
            "this root deliberately publishes no token (the documented boundary)",
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"task": "long haul", "model": CATALOG_MODEL, "role": "worker"}),
                ),
                &ctx,
            )
            .await
            .expect("spawn must succeed without a published root token");
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("agent_id")).expect("uuid");
        entered.notified().await;

        // The child's own (free-standing) token is the parent's control
        // surface, exactly as before the cascade landed.
        let handle = ctx
            .get_extension::<AgentHandles>()
            .expect("handles")
            .remove(child_id)
            .expect("handle stored");
        assert!(!handle.cancel.is_cancelled());
        handle.cancel.cancel();
        handle.join_handle.await.expect("wrapper joins");

        let result = rx
            .try_recv()
            .expect("the wrapper delivered the cancelled outcome before it ended");
        assert_eq!(result.agent_id, child_id);
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("entry observable (no reclaim marker installed)")
                .status,
            AgentStatus::Failed,
        );
    }

    // -- R5 closure: per-child loop config (including linger) ---------------

    /// Routes provider scripts for the mid-tree linger test. The
    /// grandchild's stream is gated on the child's would-stop turn having
    /// been *requested* (child_calls >= 2) plus a real delay, so the
    /// grandchild's result arrives only after the child's model has
    /// stopped — the child holds it at its stop boundary solely because
    /// its granted `child_policy.loop_config.linger_secs` is in effect.
    /// Distinct usage per level/call so the W3.6 rollup through the
    /// lingering child is pinned numerically.
    struct LingerTreeProvider {
        child_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for LingerTreeProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            use std::sync::atomic::Ordering as AtomicOrdering;
            // The managed dynamic-context Developer message now rides at the
            // tail of every request (prompt-cache fix), so route on the last
            // non-Developer message — the turn content that actually seeds
            // this child.
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            if last == "linger-grandchild-task" {
                let calls = Arc::clone(&self.child_calls);
                let s = stream::once(async move {
                    // Hold the grandchild until the child's would-stop
                    // turn has been requested, then add a real delay so
                    // the child is parked in its linger await (not its
                    // non-blocking boundary sweep) when this completes.
                    for _ in 0..2400 {
                        if calls.load(AtomicOrdering::SeqCst) >= 2 {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                })
                .flat_map(|()| {
                    stream::iter(vec![
                        Ok(ProviderEvent::TextDelta {
                            text: "grandchild late report".to_string(),
                        }),
                        Ok(done_with(StopReason::EndTurn, 7, 3)),
                    ])
                });
                return Ok(Box::pin(s));
            }
            let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
            match call {
                0 => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-linger-grandchild".to_string(),
                        call_id: None,
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "linger-grandchild-task",
                            "model": CATALOG_MODEL,
                            "role": "leaf",
                            // Per-spawn clearing of the inherited linger:
                            // the leaf grandchild must not linger itself,
                            // or its own (empty) boundary wait would
                            // outlast the child's deadline.
                            "child_policy": {
                                "messaging": "siblings_and_parent",
                                "delegation": {
                                    "remaining_depth": 0,
                                    "max_concurrent_children": 32,
                                },
                                "inbound_capacity": 32,
                            },
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_with(StopReason::ToolUse, 100, 50)),
                ]))),
                1 => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "child would stop here".to_string(),
                    }),
                    Ok(done_with(StopReason::EndTurn, 200, 60)),
                ]))),
                _ => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "child done with grandchild result".to_string(),
                    }),
                    Ok(done_with(StopReason::EndTurn, 300, 70)),
                ]))),
            }
        }
    }

    /// R5 end-to-end, the §"Messaging × recursion" item 5 scenario that
    /// was unachievable before per-child loop config: a depth-2 tree
    /// where the **child** (not the root) is granted linger via the
    /// per-spawn `child_policy.loop_config`, its model stops before the
    /// grandchild finishes, and the lingering child drains the late
    /// grandchild result, runs another turn, and delivers a complete
    /// subtree to the root — with the grandchild's `subtree_usage` rolled
    /// up through the lingering child (W3.6).
    #[tokio::test]
    async fn mid_tree_child_granted_linger_drains_late_grandchild_result() {
        use crate::agent::child_policy::ChildLoopConfig;

        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(LingerTreeProvider {
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(SpawnAgentTool::new()));
        let root_id = Uuid::new_v4();
        // Persistent parent so the child's conversation is readable from
        // its on-disk timeline for the injected-frame assertion.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (ctx, manager, root_session_id) = persistent_parent_ctx(
            tmp.path(),
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        // The mid-tree child is granted a linger through the per-spawn
        // child_policy argument — the exact surface R5 adds.
        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "task": "child-task", "model": CATALOG_MODEL, "role": "lead",
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": {
                            "remaining_depth": 1,
                            "max_concurrent_children": 32,
                        },
                        "inbound_capacity": 32,
                        "loop_config": { "linger_secs": 2 },
                    },
                })),
                &ctx,
            )
            .await
            .expect("spawn child");
        assert!(!out.is_error(), "{:?}", out.content);
        let child_id =
            Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");

        // The granted loop_config is registry ground truth on the
        // child's entry (the `agents` tool renders this policy).
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("child entry")
                .policy
                .loop_config,
            Some(ChildLoopConfig {
                step_timeout_secs: None,
                linger_secs: Some(2),
                context_window: None,
            }),
        );

        // The complete subtree reached the root as exactly one result:
        // the child's final answer, produced *after* the late grandchild
        // result was drained at the lingering stop boundary.
        let child_result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("child result is delivered")
            .expect("channel open");
        assert_eq!(child_result.agent_id, child_id);
        assert!(child_result.succeeded, "{:?}", child_result.error);
        wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
        assert!(
            child_result
                .formatted_message
                .contains("child done with grandchild result"),
            "the child's post-linger turn is the delivered result: {}",
            child_result.formatted_message,
        );
        assert!(
            rx.try_recv().is_err(),
            "the grandchild's result must never reach the root directly",
        );

        // W3.6 rollup through the lingering child: own usage is the
        // child's three provider calls; subtree adds the grandchild's.
        assert_eq!(child_result.usage.input_tokens, 600, "100+200+300 own");
        assert_eq!(child_result.usage.output_tokens, 180, "50+60+70 own");
        assert_eq!(
            child_result.subtree_usage.input_tokens, 607,
            "own + the lingered-for grandchild subtree (7)",
        );
        assert_eq!(child_result.subtree_usage.output_tokens, 183);

        // The grandchild's framed result was injected into the *child's*
        // conversation through the normal drain path — read back from the
        // child's ON-DISK timeline.
        let rows = crate::session::persistence::index::read_index(manager.data_dir())
            .expect("index reads");
        let child_row = rows
            .iter()
            .find(|r| r.parent_id.as_deref() == Some(root_session_id.as_str()))
            .expect("child session row indexed under the root");
        let child_events = events_on_disk(&manager, &child_row.id);
        let injected = child_events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_result")
                        && content.contains("grandchild late report")
            )
        });
        assert!(
            injected,
            "the late grandchild result must be injected into the lingering child's conversation",
        );
    }

    /// DECISIONS §0.6(c): the model-suppliable `loop_config.max_iterations`
    /// grant is removed. It is absent from the schema and, because
    /// `loop_config` is `deny_unknown_fields`, a spawn that still passes it
    /// is rejected loudly at the argument boundary — never silently dropped
    /// (a silent failure), and nothing is reserved.
    #[tokio::test]
    async fn spawn_rejects_removed_max_iterations_grant() {
        // The schema no longer advertises the knob under loop_config.
        let tool = SpawnAgentTool::new();
        let loop_config = &tool.input_schema()["properties"]["child_policy"]["properties"]["loop_config"]
            ["properties"];
        assert!(
            loop_config.get("max_iterations").is_none(),
            "max_iterations must be absent from the loop_config schema: {loop_config:?}",
        );
        assert!(
            loop_config.get("step_timeout_secs").is_some()
                && loop_config.get("linger_secs").is_some(),
            "the surviving loop-shaping knobs stay advertised: {loop_config:?}",
        );

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "capped", "model": CATALOG_MODEL, "role": "worker",
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": {
                            "remaining_depth": 0,
                            "max_concurrent_children": 32,
                        },
                        "inbound_capacity": 32,
                        "loop_config": { "max_iterations": 1 },
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("the removed max_iterations grant must fail loudly");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("max_iterations"),
                    "the failure names the removed field: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
        assert!(
            agent_registry.read().is_empty(),
            "a refused spawn reserves nothing",
        );
    }

    /// R5: a granted `loop_config.step_timeout_secs` actually binds on
    /// the child — a child whose provider never responds is cut off at
    /// the granted wall-clock cap with the typed `TimedOut` outcome,
    /// delivered honestly as a failed result.
    #[tokio::test]
    async fn granted_step_timeout_binds_on_child() {
        use crate::agent::output::AgentStopReason;

        // A gate that is never released: without the granted timeout the
        // child would hang forever.
        let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
            gate: Arc::new(tokio::sync::Notify::new()),
            responses: StdMutex::new(vec![vec![done_event()]]),
        });
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({
                "task": "will time out", "model": CATALOG_MODEL, "role": "worker",
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {
                        "remaining_depth": 0,
                        "max_concurrent_children": 32,
                    },
                    "inbound_capacity": 32,
                    "loop_config": { "step_timeout_secs": 1 },
                },
            }),
        )
        .await;

        assert_eq!(
            agent_registry.read().get(child_id).expect("entry").status,
            AgentStatus::Idle,
        );
        let result = rx.try_recv().expect("result delivered");
        assert!(!result.succeeded);
        assert!(
            matches!(result.stop, Some(AgentStopReason::TimedOut { .. })),
            "typed timeout outcome expected, got {:?}",
            result.stop,
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|e| e.contains("timed out")),
            "the failure names the timeout: {:?}",
            result.error,
        );
    }

    /// R5 × deny_unknown_fields: a typo'd knob *inside*
    /// `child_policy.loop_config` is rejected at the argument boundary —
    /// nothing is reserved, and the failure names the unknown field
    /// instead of silently leaving the child on library defaults.
    #[tokio::test]
    async fn spawn_rejects_unknown_loop_config_fields() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "x", "model": CATALOG_MODEL, "role": "worker",
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": {
                            "remaining_depth": 0,
                            "max_concurrent_children": 32,
                        },
                        "inbound_capacity": 32,
                        "loop_config": { "linger_seconds": 5 },
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("a typo'd loop_config knob must fail loudly");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("linger_seconds"),
                    "the failure names the unknown field: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
        assert!(
            agent_registry.read().is_empty(),
            "a refused spawn reserves nothing",
        );
    }

    /// R5 status quo: a spawn without `loop_config` stamps
    /// `loop_config: None` on the child's grant — the launch then runs
    /// `AgentLoopConfig::default()` byte-for-byte (pinned at unit level
    /// by `loop_config_none_resolves_to_default_config_exactly`), so
    /// existing spawns are behaviorally untouched by R5.
    #[tokio::test]
    async fn spawn_without_loop_config_stamps_none() {
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
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "plain", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;
        assert_eq!(
            agent_registry
                .read()
                .get(child_id)
                .expect("entry")
                .policy
                .loop_config,
            None,
            "inherit-with-decrement from an envelope without loop_config grants None",
        );
    }

    /// Seam I2-3 (idle-park drain): a message pushed through the
    /// parent-held [`AgentHandle::inbound_tx`] while the child is parked
    /// Idle must become durable (pending store + `agent_message.queued`
    /// audit) and wake-eligible — `wake_agent`'s pending-store gate
    /// accepts it and the woken step delivers it into the child's
    /// conversation. This is the router guarantee ("a message some loop
    /// will drain") extended across the park window.
    #[tokio::test]
    async fn message_to_parked_child_becomes_durable_and_wake_delivers_it() {
        use crate::r#loop::inbound::{ChannelMessage, MessageKind};
        use crate::tools::agent::coord::WakeAgentTool;

        // Response 1 completes the initial task (child parks Idle);
        // response 2 answers the wake step that drains the mailbox.
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::TextDelta {
                    text: "initial done".to_string(),
                },
                done_event(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "drained mailbox".to_string(),
                },
                done_event(),
            ],
        ]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let infra = ctx
            .get_extension::<AgentToolInfra>()
            .expect("infra installed");

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "wait for mail", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;
        assert_eq!(
            agent_registry.read().get(child_id).expect("entry").status,
            AgentStatus::Idle,
        );

        // Take the parent-held handle: its inbound sender feeds the parked
        // child's channel directly (bypassing the router, which is
        // deregistered while parked), and its event store is the child's.
        let handle = ctx
            .get_extension::<AgentHandles>()
            .expect("handles installed")
            .remove(child_id)
            .expect("child handle stored");

        let message = ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: parent,
            from: "root".to_owned(),
            role: None,
            to_id: child_id,
            content: "note for the parked child".to_owned(),
            kind: MessageKind::Update,
            seq: None,
            timestamp: Utc::now(),
        };
        handle
            .inbound_tx
            .send(message)
            .await
            .expect("parked child's channel accepts the send");

        // The park arm must route the acknowledged message into the
        // durable pending store.
        tokio::time::timeout(Duration::from_secs(5), async {
            while infra.pending_messages.pending_for(child_id) != 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("parked message must become durable in the pending store");
        let queued_audit_present = handle.event_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            )
        });
        assert!(
            queued_audit_present,
            "the idle-park re-queue must persist an agent_message.queued audit",
        );

        // The pending-store wake gate is now authoritative: the stranded
        // message makes the parked child wakeable.
        let wake_out = WakeAgentTool::new()
            .execute(
                &envelope_for(json!({ "agent_id": child_id.to_string() })),
                &ctx,
            )
            .await
            .expect("wake executes");
        assert!(!wake_out.is_error(), "{:?}", wake_out.content);
        assert_eq!(wake_out.content["woken"], true);
        assert_eq!(wake_out.content["queued_messages"], 1);

        // The woken step drains the pending store and injects the framed
        // message into the child's conversation, then parks Idle again.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let delivered = handle.event_store.events().iter().any(|e| {
                    matches!(
                        e,
                        SessionEvent::UserMessage { content, .. }
                            if content.contains("<agent_message")
                                && content.contains("note for the parked child")
                    )
                });
                if delivered && infra.pending_messages.pending_for(child_id) == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the wake step must deliver the stranded message");

        let mut status_rx = handle.status_rx.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            status_rx
                .wait_for(|status| *status == AgentStatus::Idle)
                .await
        })
        .await
        .expect("child re-parks after the wake step")
        .expect("status watch open");
    }

    // ----- agent-variants (R3/R6/§7) -------------------------------------

    /// Install the built-in variant catalog on a parent context, the way
    /// the assembly seam does.
    fn install_builtin_catalog(ctx: &ToolContext) {
        let catalog = crate::agent::variants::VariantCatalog::build(None, &std::env::temp_dir())
            .expect("built-in catalog builds");
        ctx.insert_extension(Arc::new(catalog));
    }

    /// R3 + acceptance: a spawned `explorer`'s provider-facing tool list
    /// (the actual first provider call payload) contains NO
    /// write/edit/bash/apply_patch — registry-level filtering, not call
    /// rejection. The parent's model is inherited (no explicit model)
    /// from the published AgentModel ground truth.
    #[tokio::test]
    async fn spawned_explorer_provider_tool_list_has_no_write_tools() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "explored".to_string(),
                },
                done_event(),
            ]]),
        });

        let mut registry = ToolRegistry::new();
        for name in ["read", "search", "write", "edit", "bash", "apply_patch"] {
            registry.register(Box::new(EchoStubTool { tool_name: name }));
        }
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );
        install_builtin_catalog(&ctx);
        ctx.insert_extension(Arc::new(super::super::infra::AgentModel {
            model: CATALOG_MODEL.to_owned(),
            reasoning_effort: None,
        }));

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "map the crate", "variant": "explorer"}),
        )
        .await;

        let defs = captured.lock().unwrap().clone();
        let names: Vec<String> = defs
            .iter()
            .map(|def| match def {
                ProviderToolDefinition::Function(function) => function.name.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        for forbidden in ["write", "edit", "bash", "apply_patch"] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "explorer's provider payload must not offer '{forbidden}': {names:?}",
            );
        }
        assert!(
            names.iter().any(|n| n == "read") && names.iter().any(|n| n == "search"),
            "explorer keeps its read-only subset: {names:?}",
        );
    }

    /// R6: a leaf child (granted remaining_depth == 0 — the default
    /// derivation from a depth-1 parent) is shown NEITHER spawn_agent nor
    /// fork in its provider payload, even with no allow-list at all.
    #[tokio::test]
    async fn leaf_child_provider_tool_list_omits_spawn_and_fork() {
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
        registry.register(Box::new(SpawnAgentTool::new()));
        registry.register(Box::new(super::super::fork_tool::ForkTool::new()));
        let agent_registry = AgentRegistry::shared();
        // test_envelope grants the parent depth 1 → the child's derived
        // grant is depth 0: a leaf.
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "leaf work", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let defs = captured.lock().unwrap().clone();
        let names: Vec<String> = defs
            .iter()
            .map(|def| match def {
                ProviderToolDefinition::Function(function) => function.name.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert!(
            !names.iter().any(|n| n == "spawn_agent") && !names.iter().any(|n| n == "fork"),
            "a leaf must not SEE delegation tools: {names:?}",
        );
        assert!(
            names.iter().any(|n| n == "read"),
            "non-delegation tools survive: {names:?}",
        );
    }

    /// Acceptance: a child spawned with NO model (and a variant that sets
    /// none) launches on the PARENT's model — asserted against the actual
    /// provider request, not the descriptor. The parent's model comes
    /// from its agent-registry entry (runtime ground truth).
    #[tokio::test]
    async fn no_model_child_launches_on_parents_model_from_registry() {
        // A catalogued model that is NOT the catalog default, so the
        // assertion cannot pass by accident (factual catalog id).
        let parent_model = "gpt-5.4-mini";
        assert_ne!(parent_model, CATALOG_MODEL, "test precondition");

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ]]),
        });

        let agent_registry = AgentRegistry::shared();
        let parent_guard = AgentRegistry::reserve(
            &agent_registry,
            "/parent".to_owned(),
            "orchestrator".to_owned(),
            parent_model.to_owned(),
            None,
            test_envelope().child_policy,
            None,
        )
        .expect("register parent");
        let parent_id = parent_guard.id();
        parent_guard.confirm().expect("confirm parent");

        let ctx = parent_ctx(
            provider,
            parent_id,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        install_builtin_catalog(&ctx);

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "inherit", "variant": "implementer"}),
        )
        .await;

        let requests = captured.lock().unwrap().clone();
        assert!(!requests.is_empty(), "the child made a provider call");
        for request in &requests {
            assert_eq!(
                request.model, parent_model,
                "the child's provider calls must run on the parent's model",
            );
        }
    }

    /// The reviewer ruling end-to-end on the spawn surface: no model
    /// anywhere → typed error naming `variants.reviewer.model`, and
    /// nothing is reserved or persisted.
    #[tokio::test]
    async fn reviewer_without_model_fails_naming_config_key() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        install_builtin_catalog(&ctx);

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"task": "review", "variant": "reviewer"})),
                &ctx,
            )
            .await
            .expect_err("reviewer without a model must be refused");
        assert!(
            err.to_string().contains("variants.reviewer.model"),
            "the failure names the missing config key: {err}",
        );
        assert!(
            agent_registry.read().is_empty(),
            "a refused spawn reserves nothing",
        );
    }

    /// §7: a child on an uncatalogued model is rejected BEFORE launch —
    /// mirroring the root's `oversized_explicit_window_is_rejected_at_build`
    /// semantics (children have no explicit-window escape hatch, so the
    /// catalog is the only source of a truthful window) — and the
    /// rejection leaves no registry entry.
    #[tokio::test]
    async fn child_with_uncatalogued_model_is_rejected_before_launch() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "t", "model": "not-in-catalog-model-xyz", "role": "worker",
                })),
                &ctx,
            )
            .await
            .expect_err("an uncatalogued child model must be rejected");
        assert!(
            err.to_string().contains("not-in-catalog-model-xyz"),
            "the rejection names the model: {err}",
        );
        assert!(
            agent_registry.read().is_empty(),
            "the rejection precedes the reservation",
        );
    }

    /// H: the `subagent.started` audit on the parent's store discloses
    /// the variant durably — descriptor.profile carries the variant name,
    /// and the resolved role defaults to it.
    #[tokio::test]
    async fn subagent_started_audit_discloses_variant() {
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
            Arc::new(MessageRouter::new()),
        );
        install_builtin_catalog(&ctx);

        let tool = SpawnAgentTool::new();
        let child_id = spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "explore", "variant": "explorer", "model": CATALOG_MODEL}),
        )
        .await;

        let infra = ctx
            .get_extension::<AgentToolInfra>()
            .expect("infra installed");
        let started = infra
            .event_store
            .events()
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::Custom {
                    event_type, data, ..
                } if event_type == crate::provider::agent_event::SUBAGENT_STARTED_EVENT_TYPE => {
                    Some(data)
                }
                _ => None,
            })
            .expect("subagent.started audit on the parent store");
        assert_eq!(started["child_id"], child_id.to_string());
        assert_eq!(
            started["descriptor"]["profile"], "explorer",
            "the variant is disclosed durably on the started audit: {started}",
        );
        assert_eq!(
            started["descriptor"]["role"], "explorer",
            "the role defaults to the variant name: {started}",
        );
        assert_eq!(started["descriptor"]["model"], CATALOG_MODEL);
    }

    /// F2 regression: `AgentModel` is refreshed at STEP START with the
    /// model the step's provider requests actually use, and parent-model
    /// inheritance prefers that live value over the registry row stamped
    /// at build. Agent built (registered + assembly-stamped) on model A,
    /// runtime step driven on model B, spawn with NO model from inside
    /// that step → the child launches on B (asserted on the child's
    /// actual provider request), the `subagent.started` descriptor
    /// discloses B, and the parent's registry row still says A (proving
    /// the flip in `resolve_parent_model` is what carries the switch).
    #[tokio::test]
    async fn runtime_model_switch_reaches_child_via_live_agent_model() {
        use crate::r#loop::runner::{AgentStepRequest, run_agent_step};

        // Two catalogued models (factual catalog ids, never invented):
        // A is the build-time model, B the runtime-switched step model.
        let model_a = "gpt-5.4-mini";
        let model_b = CATALOG_MODEL;
        assert_ne!(model_a, model_b, "test precondition");

        // The child's provider: captures the child's real request so the
        // launch model is asserted against ground truth.
        let child_captured = Arc::new(StdMutex::new(Vec::new()));
        let child_provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&child_captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "child done".to_string(),
                },
                done_event(),
            ]]),
        });

        // The parent is registered at build time on model A — the row
        // that goes stale across a runtime `/model` switch.
        let agent_registry = AgentRegistry::shared();
        let parent_guard = AgentRegistry::reserve(
            &agent_registry,
            "/parent".to_owned(),
            "orchestrator".to_owned(),
            model_a.to_owned(),
            None,
            test_envelope().child_policy,
            None,
        )
        .expect("register parent");
        let parent_id = parent_guard.id();
        parent_guard.confirm().expect("confirm parent");

        let ctx = parent_ctx(
            child_provider,
            parent_id,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        // The assembly-time stamp: model A, exactly what the builder
        // publishes at build.
        ctx.insert_extension(Arc::new(AgentModel {
            model: model_a.to_owned(),
            reasoning_effort: None,
        }));
        let ctx = Arc::new(ctx);

        // The parent step's tool surface carries the spawn tool; the
        // executor exposes the parent's context, so the step-start
        // refresh lands on it (the same seam every driver uses).
        let mut step_registry = ToolRegistry::new();
        step_registry.register(Box::new(SpawnAgentTool::new()));
        let executor = SubAgentExecutor::new(Arc::new(step_registry), None, Arc::clone(&ctx));

        // The parent's own step: one stream that calls spawn_agent with
        // NO model, then a closing stream.
        let parent_provider = MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-spawn".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({"task": "child work", "role": "worker"}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "spawned".to_string(),
                },
                done_event(),
            ],
        ]);

        let store = EventStore::new();
        let mut loop_context = LoopContext::new("base");
        let config = crate::r#loop::config::AgentLoopConfig::default();
        let _result = run_agent_step(AgentStepRequest {
            provider: &parent_provider,
            executor: &executor,
            store: &store,
            user_prompt: "delegate the work",
            tools: &[],
            output_schema: None,
            // The runtime-switched model for THIS step — exactly what
            // the CLI orchestrator passes after reading SlashState.model.
            model: model_b,
            config: &config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await
        .expect("parent step runs");

        // The step-start refresh landed on the parent's context.
        let live = ctx
            .get_extension::<AgentModel>()
            .expect("AgentModel stays published");
        assert_eq!(
            live.model, model_b,
            "the step-start refresh must re-publish the step's actual model",
        );
        // …while the registry row keeps its stale build-time stamp.
        assert_eq!(
            agent_registry
                .read()
                .get(parent_id)
                .expect("parent registered")
                .model,
            model_a,
            "the registry row is stamped at build and stays stale — the live \
             extension is what must carry the switch",
        );

        // Wait for the spawned child to finish its step.
        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed");
        let child_id = {
            let ids = handles.list_children();
            assert_eq!(ids.len(), 1, "exactly one child spawned: {ids:?}");
            ids[0]
        };
        let mut status_rx = handles
            .status_rx(child_id)
            .expect("status receiver stored for child");
        tokio::time::timeout(Duration::from_secs(5), async {
            status_rx
                .wait_for(|status| *status == AgentStatus::Idle || status.is_terminal())
                .await
        })
        .await
        .expect("child reaches idle or terminal status")
        .expect("status watch remains open");

        // The child's actual provider request runs on B, not A.
        let child_requests = child_captured.lock().unwrap().clone();
        assert!(!child_requests.is_empty(), "the child made a provider call");
        for request in &child_requests {
            assert_eq!(
                request.model, model_b,
                "the child must inherit the LIVE step model, not the stale \
                 build-time registry stamp",
            );
        }

        // The subagent.started descriptor discloses B durably.
        let infra = ctx
            .get_extension::<AgentToolInfra>()
            .expect("infra installed");
        let started = infra
            .event_store
            .events()
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::Custom {
                    event_type, data, ..
                } if event_type == crate::provider::agent_event::SUBAGENT_STARTED_EVENT_TYPE => {
                    Some(data)
                }
                _ => None,
            })
            .expect("subagent.started audit on the parent store");
        assert_eq!(
            started["descriptor"]["model"], model_b,
            "the started descriptor discloses the live model: {started}",
        );
    }

    /// Owner ruling 2026-07-07 (context window overrideable), end to
    /// end through the spawn tool: an explicit
    /// `child_policy.loop_config.context_window` above the catalogued
    /// child model's maximum is rejected as a typed error naming the
    /// child knob — never a silent clamp, never a launched child whose
    /// protections sit beyond the real wall.
    #[tokio::test]
    async fn oversized_explicit_child_window_is_rejected_at_spawn() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "t",
                    "role": "worker",
                    "model": "gpt-5.3-codex-spark",
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": { "remaining_depth": 0, "max_concurrent_children": 4 },
                        "inbound_capacity": 8,
                        "loop_config": { "context_window": 272_000 },
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("an oversized explicit child window must abort the spawn");
        let reason = err.to_string();
        assert!(reason.contains("272000"), "names the override: {reason}");
        assert!(reason.contains("128000"), "names the catalog max: {reason}");
        assert!(
            reason.contains("child_policy.loop_config.context_window"),
            "names the child knob: {reason}",
        );
        assert!(
            agent_registry.read().list().is_empty(),
            "the refused spawn leaves no registry entry",
        );
    }

    /// Re-review R2, end to end through the spawn tool: a variant whose
    /// EXPLICITLY configured reasoning effort is not supported by the
    /// child's resolved model is refused as a typed error naming the
    /// setting — BEFORE the reservation, so no registry entry and no
    /// audit residue survive the refusal ("none" is declared for no
    /// catalogued model — factual catalog content).
    #[tokio::test]
    async fn variant_effort_unsupported_by_child_model_is_rejected_at_spawn() {
        use std::collections::BTreeMap;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            crate::config::types::VariantSettings {
                prompt: Some("Scout the area.".to_owned()),
                reasoning_effort: Some("none".to_owned()),
                ..crate::config::types::VariantSettings::default()
            },
        );
        let catalog =
            crate::agent::variants::VariantCatalog::build(Some(&configured), &std::env::temp_dir())
                .expect("catalog builds");
        ctx.insert_extension(Arc::new(catalog));

        let tool = SpawnAgentTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({
                    "task": "t",
                    "variant": "scout",
                    "model": CATALOG_MODEL,
                })),
                &ctx,
            )
            .await
            .expect_err("an unsupported explicit variant effort must abort the spawn");
        let reason = err.to_string();
        assert!(
            reason.contains("variants.scout.reasoning_effort"),
            "names the setting: {reason}",
        );
        assert!(
            reason.contains("low, medium, high, xhigh"),
            "lists the model's catalogued efforts: {reason}",
        );
        assert!(
            agent_registry.read().list().is_empty(),
            "the refused spawn leaves no registry entry",
        );
    }

    /// Owner ruling 2026-07-07 (reasoning effort inherited): with no
    /// variant effort and no profile effort, the child inherits the
    /// parent's ACTIVE effort from the live per-step stamp — asserted on
    /// the child's actual provider requests. A parent running with no
    /// effort passes None through unchanged (today's behavior).
    #[tokio::test]
    async fn child_inherits_parents_active_reasoning_effort() {
        use crate::provider::request::ReasoningEffort;

        for (parent_effort, expected) in [
            (Some(ReasoningEffort::High), Some(ReasoningEffort::High)),
            (None, None),
        ] {
            let captured = Arc::new(StdMutex::new(Vec::new()));
            let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
                captured: Arc::clone(&captured),
                responses: StdMutex::new(vec![vec![
                    ProviderEvent::TextDelta {
                        text: "done".to_string(),
                    },
                    done_event(),
                ]]),
            });
            let agent_registry = AgentRegistry::shared();
            let ctx = parent_ctx(
                provider,
                Uuid::new_v4(),
                &agent_registry,
                Arc::new(ToolRegistry::new()),
                Arc::new(MessageRouter::new()),
            );
            // The parent's live per-step stamp: model + effort together.
            ctx.insert_extension(Arc::new(AgentModel {
                model: CATALOG_MODEL.to_owned(),
                reasoning_effort: parent_effort,
            }));

            let tool = SpawnAgentTool::new();
            spawn_and_join(
                &tool,
                &ctx,
                json!({"task": "inherit effort", "role": "worker"}),
            )
            .await;

            let requests = captured.lock().unwrap().clone();
            assert!(!requests.is_empty(), "the child made a provider call");
            for request in &requests {
                assert_eq!(
                    request.reasoning_effort, expected,
                    "the child inherits exactly the parent's active effort \
                     (parent: {parent_effort:?})",
                );
            }
        }
    }

    /// §3.6: a variant's reasoning effort reaches the child's actual
    /// provider requests.
    #[tokio::test]
    async fn variant_reasoning_effort_reaches_child_provider_requests() {
        use std::collections::BTreeMap;

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ]]),
        });
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        // Owner ruling 2026-07-07: the variant's effort WINS over the
        // parent's live effort — stamp a conflicting parent effort to
        // prove precedence, not just presence.
        ctx.insert_extension(Arc::new(AgentModel {
            model: CATALOG_MODEL.to_owned(),
            reasoning_effort: Some(crate::provider::request::ReasoningEffort::High),
        }));
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            crate::config::types::VariantSettings {
                prompt: Some("Scout the area.".to_owned()),
                reasoning_effort: Some("low".to_owned()),
                ..crate::config::types::VariantSettings::default()
            },
        );
        let catalog =
            crate::agent::variants::VariantCatalog::build(Some(&configured), &std::env::temp_dir())
                .expect("catalog builds");
        ctx.insert_extension(Arc::new(catalog));

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "scout it", "variant": "scout", "model": CATALOG_MODEL}),
        )
        .await;

        let requests = captured.lock().unwrap().clone();
        assert!(!requests.is_empty(), "the child made a provider call");
        for request in &requests {
            assert_eq!(
                request.reasoning_effort,
                Some(crate::provider::request::ReasoningEffort::Low),
                "the variant's reasoning effort must ride every child request \
                 (and win over the parent's live High effort)",
            );
        }
    }

    /// R5 (spawn side): the spawned child's own context carries ITS OWN
    /// base system instruction under `ParentSystemInstruction` — proven
    /// from inside the child via a probe tool — so the child's own forks
    /// inherit the child's context, not the grandparent's.
    #[tokio::test]
    async fn spawned_child_context_carries_its_own_base_instruction() {
        struct BaseProbe {
            seen: Arc<StdMutex<Option<String>>>,
        }
        #[async_trait]
        impl TestTool for BaseProbe {
            fn name(&self) -> &'static str {
                "base_probe"
            }
            fn description(&self) -> &'static str {
                "records the ParentSystemInstruction it sees"
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
                *self.seen.lock().unwrap() = ctx
                    .get_extension::<crate::agent::fork::ParentSystemInstruction>()
                    .map(|ext| ext.as_str().to_owned());
                Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-probe".to_string(),
                    call_id: None,
                    name: Some("base_probe".to_string()),
                    arguments_delta: "{}".to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "done".to_string(),
                },
                done_event(),
            ],
        ]));
        let seen = Arc::new(StdMutex::new(None));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BaseProbe {
            seen: Arc::clone(&seen),
        }));
        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );

        let tool = SpawnAgentTool::new();
        spawn_and_join(
            &tool,
            &ctx,
            json!({"task": "probe your base", "model": CATALOG_MODEL, "role": "worker"}),
        )
        .await;

        let base = seen
            .lock()
            .unwrap()
            .clone()
            .expect("the child's context must publish ParentSystemInstruction");
        assert!(
            base.contains("probe your base"),
            "the published value is the CHILD's own task-derived base: {base}",
        );
    }
}
