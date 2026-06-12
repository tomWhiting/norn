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

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use super::delegation::{
    auto_child_path, grant_child_policy, install_child_result_channel, resolve_spawner_policy,
};
use super::handle::{AgentHandles, ChildBranchMetadata, SharedSessionTree};
use super::infra::{
    AgentCancellation, SubAgentExecutor, infra_from, strip_signal_agent_from_allow_list,
};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use super::spawn_context::build_child_context;
use super::spawn_launch::{ChildLaunch, launch_child};
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope, MessagingScope};
use crate::agent::fork::ContextFilter;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::ChildResultSender;
use crate::error::ToolError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::loop_context::LoopContext;
use crate::profile::{default_scan_dirs, from_profile, resolve_profile};
use crate::provider::agent_event::{AgentEventSender, SubagentDescriptor, SubagentKind};
use crate::provider::request::ToolDefinition;
use crate::session::action_log::ActionLog;
use crate::session::store::EventStore;
use crate::session::tree::{BranchConfig, SessionMetadata, SessionStatus};
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
    model: String,
    role: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
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
/// Delegates to the shared registry → function-definition projection in
/// [`crate::provider::surface`] — the same projection `AgentBuilder`
/// assembly uses — filtered through `allow_list` (the same list that gates
/// the child's [`SubAgentExecutor`]). When `allow_list` is `None` every
/// available parent tool is included. The child's agent loop then resolves
/// these definitions against the live provider's capabilities per request,
/// exactly like the parent's loop, so hosted-tool replacement applies
/// identically to children.
fn build_tool_definitions(
    registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> Vec<ToolDefinition> {
    crate::provider::surface::collect_function_definitions(registry, allow_list)
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
                    "description": "Hierarchical registry path for the sub-agent (e.g. \"/workers/phase-1\"). Not a file path. Omit to auto-generate under your own registry path (\"{your_path}/spawn/{uuid}\")."
                },
                "output_schema": {
                    "type": "object",
                    "description": "Optional JSON Schema the sub-agent's final output must validate against. The sub-agent never inherits the caller's output schema implicitly — supply one here when the result must be structured. Omit for free-form output."
                },
                "child_policy": {
                    "type": "object",
                    "required": ["messaging", "delegation", "inbound_capacity"],
                    "additionalProperties": false,
                    "description": "Optional narrowed policy for this child. Omit to grant your own policy with delegation depth reduced by one level. Every field must be within your own granted budget — widening fails.",
                    "properties": {
                        "messaging": {
                            "type": "string",
                            "enum": ["siblings_and_parent", "parent_only", "none"],
                            "description": "Who the child may message; must not widen your own scope."
                        },
                        "delegation": {
                            "type": "object",
                            "required": ["remaining_depth", "max_concurrent_children"],
                            "additionalProperties": false,
                            "properties": {
                                "remaining_depth": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Levels of descendants the child may create below itself (0 = leaf). Must be at most your own remaining_depth - 1."
                                },
                                "max_concurrent_children": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Max non-terminal direct children the child may have at once. Must be at most your own cap."
                                }
                            }
                        },
                        "inbound_capacity": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Bounded capacity of the child's inbound message channel. Must be at most your own granted capacity."
                        }
                    }
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
        // definitions the child model is shown. A `MessagingScope::None`
        // grant removes `signal_agent` from that surface entirely
        // (defense-in-depth: the tool also refuses at execute).
        let mut allow_list: Option<Vec<String>> = args.tools.clone().or(profile_tools);
        if child_policy.messaging == MessagingScope::None {
            allow_list = Some(strip_signal_agent_from_allow_list(
                allow_list,
                parent_registry,
            ));
        }
        let tool_defs = build_tool_definitions(parent_registry, allow_list.as_deref());

        // Auto paths nest under the spawning agent's own registry path so
        // the agents tree reads as a real tree at every depth (W3.4).
        let path = args
            .path
            .unwrap_or_else(|| auto_child_path(&infra.registry, infra.agent_id, "spawn"));

        // The session-tree role label and the audit-trail provenance both
        // prefer the profile name, falling back to the role argument when no
        // profile was given. Computed before `reserve` consumes `args.role`.
        let role_label = args.profile.clone().unwrap_or_else(|| args.role.clone());

        // Provenance carried on both typed lifecycle phases. Captured
        // before `reserve` consumes `args.role`.
        let descriptor = SubagentDescriptor {
            kind: SubagentKind::Spawn,
            role: args.role.clone(),
            model: args.model.clone(),
            profile: args.profile.clone(),
        };

        // Two-phase reservation: the guard stays unconfirmed across the
        // fallible store resolution below, so an error rolls the
        // reservation back via RAII instead of leaking a confirmed entry
        // that no launch wrapper will ever transition to a terminal
        // status.
        let guard = AgentRegistry::reserve(
            &infra.registry,
            path.clone(),
            args.role,
            args.model.clone(),
            Some(infra.agent_id),
            child_policy.clone(),
            Some(&spawner_policy),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("spawn reservation failed: {e}"),
        })?;
        let child_id = guard.id();

        // Resolve the child's event store: a named branch under the parent's
        // session when an orchestrator published a SessionTree, otherwise a
        // standalone store (NA-008 R3). In tree mode `child_tree` carries the
        // child's own SessionId for grandchild branching.
        let (child_store, child_tree) = resolve_child_store(ctx, &args.model, &role_label)?;

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
            profile_name: args.profile.clone(),
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
            child_tree,
            child_policy.clone(),
            child_cancel.clone(),
        );
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
        // exists to anchor "delivered" to. The wrapper is the sole
        // reclaimer; the oneshot ack (resolved after `handles.insert`
        // below) tells it when the handle is guaranteed to be stored.
        // See `super::reclaim`.
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

        let child_event_sender = ctx
            .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
            .map(|ch| {
                AgentEventSender::new(ch.0.clone(), child_id, format!("spawn/{}", args.model))
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
        lifecycle.emit_started();

        let handle = launch_child(ChildLaunch {
            provider: Arc::clone(&infra.provider),
            executor: child_executor,
            store: child_store,
            loop_ctx: child_loop_ctx,
            tool_defs,
            task: args.task,
            output_schema: args.output_schema,
            model: args.model,
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
            cancel: child_cancel,
        });
        handles.insert(handle);

        // Handshake: the handle is stored — tell the wrapper its
        // reclamation pass may run. This closes the insert/finish race
        // without a second reclaimer: a child that finished before the
        // insert above is parked at this ack inside its wrapper, which
        // then reclaims both the handle and the registry entry itself. A
        // send error means the wrapper exited without entering its
        // reclaim pass (stop hook suppressed the terminal transition, or
        // something external killed the wrapper task); whoever ended it
        // owns any remaining cleanup, so there is nothing further for
        // this path to do.
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
    use parking_lot::RwLock;
    use serde_json::json;
    use uuid::Uuid;

    use super::super::infra::AgentToolInfra;
    use super::*;
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
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: parent_id,
            parent_id: None,
            grant: None,
            tool_registry: Some(tool_registry),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(test_envelope()));
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
        assert!(!out.is_error(), "{:?}", out.content);
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
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
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
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
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
            Arc::new(MessageRouter::new()),
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
            json!({"task": "do it", "model": "haiku", "role": "worker"}),
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
                assert_eq!(&*ev.agent_role, "spawn/haiku");
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
                assert_eq!(descriptor.model, "haiku");
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
            json!({"task": "will fail", "model": "haiku", "role": "worker"}),
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
            json!({"task": "boom", "model": "haiku", "role": "worker"}),
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
            error.contains("terminated without an outcome"),
            "error must be honest about the missing outcome: {error}",
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
                .contains("terminated without an outcome"),
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
                "model": "haiku",
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
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            Arc::new(MessageRouter::new()),
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
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
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

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
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
            Arc::new(MessageRouter::new()),
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
            json!({"task": "probe it", "model": "haiku", "role": "worker"}),
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
                &envelope_for(json!({"task": "long haul", "model": "haiku", "role": "worker"})),
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
                    runtime_inputs: RuntimeInputs::default(),
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
                    "model": "haiku",
                    "role": "worker",
                    "path": "/smoke/child",
                })),
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
                    runtime_inputs: RuntimeInputs::default(),
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
    /// inbound route at launch and the completion wrapper deregisters at
    /// the run's end — `signal_agent` reaches a live child without any
    /// tool-side registration, and a finished child is NotRouted.
    #[tokio::test]
    async fn spawn_registers_route_at_launch_and_deregisters_at_terminal() {
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
                &envelope_for(json!({"task": "wait", "model": "haiku", "role": "worker"})),
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
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(child_id)
            .expect("handle");
        handle.join_handle.await.expect("join");
        assert!(
            !router.is_routed(child_id),
            "the completion wrapper must deregister the route at the run's end",
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
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
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

            let mut args = json!({"task": "quiet work", "model": "haiku", "role": "worker"});
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
                provider,
                event_store: parent_event_store,
                agent_id: parent,
                parent_id: None,
                grant: None,
                tool_registry: Some(Arc::new(registry)),
            });
            let ctx = ToolContext::empty();
            ctx.insert_extension(infra);
            ctx.insert_extension(Arc::new(AgentHandles::new()));
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
            json!({"task": "introspect", "model": "haiku", "role": "worker"}),
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
                &envelope_for(json!({"task": "x", "model": "haiku", "role": "worker"})),
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
                    "task": "x", "model": "haiku", "role": "worker",
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
                    "task": "x", "model": "haiku", "role": "worker",
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
                    "task": "x", "model": "haiku", "role": "worker",
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
                    "task": "x", "model": "haiku", "role": "worker",
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
                "task": "x", "model": "haiku", "role": "worker",
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
            json!({"task": "x", "model": "haiku", "role": "worker"}),
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
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "grandchild", "model": "haiku", "role": "leaf",
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
            json!({"task": "try to delegate", "model": "haiku", "role": "worker"}),
        )
        .await;

        let reg = agent_registry.read();
        let entry = reg.get(child_id).expect("child entry retained");
        assert_eq!(entry.status, AgentStatus::Completed, "the child completed");
        assert_eq!(entry.policy.delegation.remaining_depth, 0, "leaf grant");
        assert_eq!(
            reg.len(),
            1,
            "the grandchild must never be registered: {:?}",
            reg.list(),
        );
        assert!(reg.tombstones().is_empty(), "nothing was reclaimed");
    }

    /// Routes provider scripts by conversation identity (the first user
    /// message) so a mid-tree child and its grandchild can share the one
    /// workspace provider deterministically; the child's would-stop turn
    /// is held until the registry shows the grandchild reclaimed, which
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
            let last = request
                .messages
                .last()
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
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "grandchild-task",
                            "model": "haiku",
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
                            let reclaimed = registry
                                .read()
                                .tombstones()
                                .iter()
                                .any(|t| t.path.matches("/spawn/").count() == 2);
                            if reclaimed {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        panic!("grandchild was never reclaimed — the test cannot proceed");
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
    /// every registry entry at every level is reclaimed.
    #[tokio::test]
    async fn grandchild_results_bubble_one_hop_and_reclaim_at_every_level() {
        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(TreeProvider {
            registry: Arc::clone(&agent_registry),
            child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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

        // Root result channel + delivery-anchored reclamation, so the
        // wrappers reclaim at every level (the recorded grandchild-leak
        // gap this step closes).
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

        // Session tree so the child's conversation store stays reachable
        // after reclamation.
        let tree = Arc::new(SessionTree::new(SessionMetadata {
            created_at: Utc::now(),
            model: "opus".to_string(),
            role: Some("root".to_string()),
            status: SessionStatus::Active,
        }));
        let root_session = tree.root();
        ctx.insert_extension(Arc::new(SharedSessionTree {
            tree: Arc::clone(&tree),
            session_id: root_session,
        }));

        let tool = SpawnAgentTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"task": "child-task", "model": "haiku", "role": "lead"})),
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

        // Reclamation at every level: both entries leave, both tombstones
        // stay, parent links and nested paths intact.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if agent_registry.read().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "registry entries leaked: {:?}",
                agent_registry.read().list(),
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let reg = agent_registry.read();
        let tombstones = reg.tombstones();
        assert_eq!(tombstones.len(), 2, "child + grandchild reclaimed");
        let child_tomb = tombstones
            .iter()
            .find(|t| t.id == child_id)
            .expect("child tombstone");
        assert_eq!(child_tomb.parent_id, Some(root_id));
        assert_eq!(child_tomb.status, AgentStatus::Completed);
        let grandchild_tomb = tombstones
            .iter()
            .find(|t| t.id != child_id)
            .expect("grandchild tombstone");
        assert_eq!(
            grandchild_tomb.parent_id,
            Some(child_id),
            "the grandchild's parent is the mid-tree child, not the root",
        );
        assert_eq!(grandchild_tomb.status, AgentStatus::Completed);
        assert!(
            grandchild_tomb
                .path
                .starts_with(&format!("{child_path}/spawn/")),
            "grandchild path nests under the child: {}",
            grandchild_tomb.path,
        );
        drop(reg);

        // One-hop delivery into the child's conversation: the child's
        // session branch holds the framed grandchild result.
        let child_sessions = tree.list_children(root_session);
        assert_eq!(child_sessions.len(), 1, "child branched under the root");
        let child_store = tree
            .get_store(child_sessions[0])
            .expect("child store survives reclamation in tree mode");
        let injected = child_store.events().iter().any(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_result")
                        && content.contains("grandchild says hi")
            )
        });
        assert!(
            injected,
            "the grandchild's framed result must be injected into the child's conversation",
        );
        // And the grandchild's session branched under the child's session.
        assert_eq!(
            tree.list_children(child_sessions[0]).len(),
            1,
            "grandchild session branches under the child's session",
        );
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
            let last = request
                .messages
                .last()
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
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "grandchild-task",
                            "model": "haiku",
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
                &envelope_for(json!({"task": "child-task", "model": "haiku", "role": "lead"})),
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
                    runtime_inputs: RuntimeInputs::default(),
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
                &envelope_for(json!({"task": "long haul", "model": "haiku", "role": "worker"})),
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
}
