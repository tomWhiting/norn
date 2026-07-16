//! `ForkTool` (NA-010) — async fork that mirrors `SpawnAgentTool`'s
//! `tokio::spawn` lifecycle.
//!
//! Fork is semantically distinct from spawn: **fork = same identity, different
//! model**, **spawn = fresh identity, configured through profile**. The
//! machinery is shared (per-child [`ToolContext`], status watch channel,
//! inbound steering channel, child result channel) so coordination
//! tools — `signal_agent`, `close_agent` — work uniformly across
//! both surfaces.
//!
//! The tool's `execute()` reserves a registry slot, mints the fork's
//! session through the parent's
//! [`SessionBinding::branch_child`](crate::session::SessionBinding::branch_child)
//! (a real write-through timeline under the root's `children/` dir for
//! persistent parents; honest ephemerality otherwise), seeds the child's
//! event store (inheriting the parent's audit trail with a synthetic
//! tool-result for the fork call itself),
//! composes the child's [`LoopContext`] (fork preamble + parent base system
//! instruction), filters the parent registry's tool definitions through the
//! per-fork allow-list, launches the child via [`tokio::spawn`], and returns
//! immediately with `{ agent_id, path, status: "active" }` — the same child-id
//! field name `spawn_agent` uses. On terminal status the
//! launcher marks the registry, appends a
//! [`SessionEvent::ForkComplete`](crate::session::events::SessionEvent::ForkComplete)
//! to the parent's timeline, and sends the formatted result through the
//! [`ChildResultSender`].

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use super::delegation::{
    auto_child_path, effective_child_tools, grant_child_policy, install_child_result_channel,
    resolve_spawner_policy,
};
use super::fork_context::build_fork_context;
use super::fork_launch::{ForkLaunch, launch_fork};
use super::fork_seed::{seed_fork_events, truncate_seed_at_anchor};
use super::handle::AgentHandles;
use super::infra::{AgentCancellation, AgentModel, infra_from};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use crate::agent::child_policy::{ChildLoopConfig, ChildPolicy, CoordinationEnvelope};
use crate::agent::fork::{
    ForkIdentity, ForkRequirement, ParentSystemInstruction, build_fork_output_schema,
    build_fork_preamble, combine_system_instruction,
};
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::ChildResultSender;
use crate::error::ToolError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::inbound::inbound_channel;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::{SubagentDescriptor, SubagentKind};
use crate::session::action_log::ActionLog;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Forks the parent session onto a (possibly different) model for a bounded
/// task. Same identity as the parent, runs concurrently, optionally
/// task-structured.
pub struct ForkTool;

impl ForkTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ForkTool {
    fn default() -> Self {
        Self::new()
    }
}

// deny_unknown_fields: a typo'd key (e.g. `child_polciy`) must fail
// loudly, not silently hand the fork a default grant where the caller
// intended a narrowing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkArgs {
    request: String,
    model: String,
    requirements: Vec<ForkRequirement>,
    /// Optional per-fork [`ChildPolicy`] narrowing (DECISION R2),
    /// mirroring the Rust type 1:1 at the JSON layer. Omitted → the fork
    /// inherits the caller's own granted policy with the delegation depth
    /// decremented one level. Supplied → must be a strict narrowing of
    /// the caller's own grant; widening is a typed failure naming the
    /// caller's budget.
    #[serde(default)]
    child_policy: Option<ChildPolicy>,
}

/// Public tool name for the Norn fork delegation tool.
pub const FORK_TOOL_NAME: &str = "fork";

#[async_trait]
impl Tool for ForkTool {
    fn name(&self) -> &'static str {
        FORK_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/fork.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/fork.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["request", "model", "requirements"],
            "additionalProperties": false,
            "properties": {
                "request": {
                    "type": "string",
                    "description": "What you need the forked agent to do. The fork inherits the full conversation context from the parent session."
                },
                "model": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Model identifier for the forked agent. Use a model supported by the current provider/backend."
                },
                "requirements": {
                    "type": "array",
                    "description": "Requirements the fork must satisfy. When provided, the fork's structured output includes a completion record for each requirement with name, completed (bool), and completion_notes.",
                    "items": {
                        "type": "object",
                        "required": ["name", "description"],
                        "additionalProperties": false,
                        "properties": {
                            "name": { "type": "string", "description": "Identifier for this requirement." },
                            "description": { "type": "string", "description": "What must be done to satisfy this requirement." }
                        }
                    }
                },
                "child_policy": {
                    "type": "object",
                    "required": ["messaging", "delegation", "inbound_capacity"],
                    "additionalProperties": false,
                    "description": "Optional narrowed policy for this fork. Omit to grant your own policy with delegation depth reduced by one level. Every field except loop_config must be within your own granted budget — widening fails. Supplying child_policy is a complete replacement: without loop_config it clears any inherited loop overrides — restate them to keep them.",
                    "properties": {
                        "messaging": {
                            "type": "string",
                            "enum": ["siblings_and_parent", "parent_only", "none"],
                            "description": "Who the fork may message; must not widen your own scope."
                        },
                        "delegation": {
                            "type": "object",
                            "required": ["remaining_depth", "max_concurrent_children"],
                            "additionalProperties": false,
                            "properties": {
                                "remaining_depth": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Levels of descendants the fork may create below itself (0 = leaf). Must be at most your own remaining_depth - 1."
                                },
                                "max_concurrent_children": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Max non-terminal direct children the fork may have at once. Must be at most your own cap."
                                }
                            }
                        },
                        "inbound_capacity": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Bounded capacity of the fork's inbound message channel. Must be at most your own granted capacity."
                        },
                        "loop_config": {
                            "type": "object",
                            "additionalProperties": false,
                            "description": "Optional loop-shaping overrides for the fork. Not a narrowing axis: any value is accepted regardless of your own loop config. Each field is optional; an unset field keeps the library default (today's behavior). Omit entirely to run the fork on default loop limits — and note that supplying child_policy without this key clears any loop overrides the fork would have inherited; restate them to keep them.",
                            "properties": {
                                "step_timeout_secs": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Wall-clock cap in seconds on each of the fork's steps. Unset = uncapped."
                                },
                                "linger_secs": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Linger deadline in seconds: the fork waits this long at each would-stop boundary for late messages and its own children's results before stopping. Grant this to a fork that delegates, so its children's late results are delivered instead of lost. Unset = the fork returns the moment its model stops."
                                },
                                "context_window": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Explicit context window for the fork, in tokens. Unset = filled from the model catalog for the fork's model. A value above a catalogued model's maximum is rejected; required for a deliberately uncatalogued model."
                                }
                            }
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
        let args: ForkArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        let infra = infra_from(ctx)?;

        if args.model.trim().is_empty() {
            return Err(ToolError::ExecutionFailed {
                reason: "fork model must be non-empty".to_string(),
            });
        }

        let parent_registry =
            infra
                .tool_registry
                .as_ref()
                .ok_or_else(|| ToolError::ExecutionFailed {
                    reason: "fork requires AgentToolInfra.tool_registry to be configured; \
                         orchestrator must provide a ToolRegistry so the fork has tools available"
                        .to_owned(),
                })?;
        // The forking agent's `AgentHandles` extension must be installed
        // before launch — `build_runtime` installs it during runtime
        // construction; a missing extension surfaces as a typed
        // `MissingExtension` error.
        let handles = ctx.require_extension::<AgentHandles>()?;

        // The coordination envelope is the runtime's deliberate child
        // policy (W3.0 made it builder-required; the CLI assembly
        // publishes its own). A context that can fork but carries no
        // envelope is a wiring error, surfaced as a typed
        // `MissingExtension` failure — fork never invents a policy for
        // the child.
        let coordination = ctx.require_extension::<CoordinationEnvelope>()?;

        // The fork's grant (W3.4): the caller's own granted policy
        // narrowed by the optional `child_policy` argument, or derived by
        // inherit-with-decrement when omitted. Depth exhaustion and
        // widening both fail typed here, naming the caller's own budget;
        // the registry re-validates from ground truth at reservation.
        let spawner_policy = resolve_spawner_policy(&infra, &coordination);
        let fork_policy = grant_child_policy(&spawner_policy, args.child_policy.clone(), "fork")?;

        // Fork context-window validation (spec §7): fill the fork's
        // window from the catalog for the FORK's model and validate it —
        // mirroring the root build's arm-then-validate sequence — BEFORE
        // anything is reserved or persisted, so a failure is a clean
        // typed error with no burned name and no dangling reservation.
        // The same resolved config rides into the launch below.
        let mut fork_config = ChildLoopConfig::resolve(fork_policy.loop_config);
        crate::agent::arming::arm_child_window(&mut fork_config, &args.model).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("fork: {e}"),
            }
        })?;

        // R3 / W3.4: hierarchical fork path nests under the spawning
        // agent's registry path at every depth.
        let path = auto_child_path(&infra.registry, infra.agent_id, "fork");

        // Two-phase reservation: the guard stays unconfirmed across every
        // fallible setup step below, so an error rolls the reservation
        // back via RAII instead of leaking a confirmed entry that no
        // launch wrapper will ever transition to a terminal status.
        let guard = AgentRegistry::reserve(
            &infra.registry,
            path.clone(),
            "fork".to_owned(),
            args.model.clone(),
            Some(infra.agent_id),
            fork_policy.clone(),
            Some(&spawner_policy),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork reservation failed: {e}"),
        })?;
        let fork_id = guard.id();

        // Mint the fork's session through the parent's branching binding
        // (V2-R2): a persistent parent yields a real write-through child
        // timeline under the root's children/ dir, with the ChildBranch
        // reservation durably on the parent's timeline PARENT-FIRST; an
        // ephemeral parent propagates ephemerality with the honest
        // `session: None` reservation. A failure after this point (seed,
        // started-audit) leaves a burned name + dangling reference —
        // exactly the crash residue resume paths already tolerate. The
        // mint's blocking file I/O runs off the executor (F5).
        let branched = super::delegation::branch_child_off_executor(
            &infra.session,
            &infra.event_store,
            &crate::session::ChildBranchRequest {
                child_session_id: fork_id.to_string(),
                name_stem: crate::session::slugify_name_stem("fork", "fork"),
                kind: crate::session::events::ChildBranchKind::Fork,
                durability: infra.session.child_durability(),
                model: args.model.clone(),
                working_dir: ctx.working_dir().display().to_string(),
            },
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork: session branch failed: {e}"),
        })?;
        let child_store = Arc::clone(&branched.store);
        let forked_session_id = branched.session_id.clone();

        // The seed copy is the parent history UP TO the anchor the
        // reservation recorded (captured inside the allocation lock):
        // the snapshot is taken after the mint and truncated at the
        // anchor, so neither the fork's own reservation nor anything a
        // concurrent task appended between mint and snapshot can leak
        // into the inherited history (F4). The fork's provenance header,
        // appended by branch_child to its own file, carries the same
        // record.
        let parent_events = truncate_seed_at_anchor(
            infra.event_store.events(),
            branched.parent_event_anchor.as_ref(),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork: {e}"),
        })?;

        let fork_call_id = if envelope.tool_call_id.is_empty() {
            None
        } else {
            Some(envelope.tool_call_id.as_str())
        };
        seed_fork_events(child_store.as_ref(), &parent_events, fork_call_id, fork_id).map_err(
            |e| ToolError::ExecutionFailed {
                reason: format!("fork: seeding child store failed: {e}"),
            },
        )?;

        let child_event_sender = ctx
            .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
            .map(|ch| {
                crate::provider::agent_event::AgentEventSender::new(
                    ch.0.clone(),
                    fork_id,
                    format!("fork/{}", args.model),
                )
            });

        // Typed lifecycle: `Started` is emitted before the fork task
        // launches, so it always precedes the fork's own provider events
        // on the broadcast channel; the wrapper task emits `Completed`.
        // Both phases also land as Custom audit events on the parent's
        // session store.
        let lifecycle = LifecycleEmitter::new(
            child_event_sender.clone(),
            Arc::clone(&infra.event_store),
            infra.agent_id,
            fork_id,
            SubagentDescriptor {
                kind: SubagentKind::Fork,
                role: "fork".to_owned(),
                model: args.model.clone(),
                profile: None,
            },
            Utc::now(),
        );
        // The Started audit joins the primary write-through contract
        // (session-fidelity Gap 10) and fires BEFORE the reservation is
        // confirmed: on a persist failure the guard's RAII rollback
        // reclaims the registry slot, so a refused fork can never leave
        // a phantom Active child pinning the parent's concurrency budget
        // (the only residue is the already-tolerated burned name +
        // dangling reservation).
        lifecycle
            .emit_started()
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!(
                    "failed to persist the subagent.started audit event; \
                     fork aborted before launch: {error}",
                ),
            })?;

        // All fallible setup is done — confirm the reservation. From here
        // the launch is unconditional and the completion wrapper owns the
        // entry's terminal transition.
        guard.confirm().map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork confirm failed: {e}"),
        })?;

        // Hierarchical cancellation (W3.5): the fork's run token is a
        // child of the forker's published token, so cancelling the forker
        // — or any ancestor above it — cascades to this fork and its
        // whole subtree, each run ending with its real `Cancelled`
        // outcome through its own wrapper. A parent context that
        // publishes no token (embedder roots assembled outside
        // `AgentBuilder`) yields a free-standing token — exactly the
        // pre-cascade behavior; see `AgentCancellation` for the boundary.
        let fork_cancel = ctx
            .get_extension::<AgentCancellation>()
            .map_or_else(tokio_util::sync::CancellationToken::new, |parent_cancel| {
                parent_cancel.0.child_token()
            });

        // R3: per-child ToolContext with fresh identity, forwarded shared
        // infrastructure, the granted policy stamped for signal_agent's
        // scope enforcement and the fork's own spawn-time budget reads.
        let child_ctx = build_fork_context(
            &infra,
            fork_id,
            Arc::clone(&child_store),
            ctx,
            Arc::clone(&branched.binding),
            fork_policy.clone(),
            fork_cancel.clone(),
        );
        // Per-agent result channel (W3.4): a fork whose grant lets it
        // delegate gets its own child-result channel — sender on its
        // context for its spawn/fork sites, receiver wired onto its loop
        // below — so its children's results deliver to *this fork*, one
        // hop at a time.
        let fork_result_rx = install_child_result_channel(
            &child_ctx,
            &fork_policy,
            coordination.child_result_capacity,
        );

        // R4/R5: combined system instruction = structured fork preamble
        // (identity, path address, requirements contract, delegation
        // rights — the child is TOLD its budget) + the parent's own base
        // system instruction from the ParentSystemInstruction extension
        // every assembly path publishes.
        let requirement_names: Vec<String> = args
            .requirements
            .iter()
            .map(|r| crate::agent::fork::slugify_requirement_name(&r.name))
            .collect();
        let parent_agent_id = infra.agent_id.to_string();
        let preamble = build_fork_preamble(&ForkIdentity {
            parent_agent_id: &parent_agent_id,
            path_address: &branched.path_address,
            requirement_slugs: &requirement_names,
            granted: &fork_policy,
        });
        let parent_base = ctx
            .get_extension::<ParentSystemInstruction>()
            .map_or_else(String::new, |ext| ext.as_str().to_owned());
        // The fork's LoopContext shares the SharedWorkingDir handle with
        // its own child_ctx so the fork's bash `cd`s update both the
        // child's tool path resolution and its loop-level command paths
        // (prompt commands, hooks, rules). The handle is a fresh Arc
        // initialised from the parent's current dir (see
        // `build_fork_context`), so the fork diverges from the parent
        // independently.
        let mut loop_ctx = LoopContext::with_working_dir(
            combine_system_instruction(&preamble, &parent_base),
            child_ctx.shared_working_dir(),
        );
        loop_ctx.agent_id = Some(fork_id);
        loop_ctx.pending_agent_messages = Some(Arc::clone(&infra.pending_messages));
        // Reasoning effort (owner rulings 2026-07-07: children inherit
        // the parent's active effort): the fork has no variant/effort
        // surface of its own, so it inherits the forker's LIVE effort
        // from the same per-step stamp parent-model inheritance reads,
        // validated against the model catalog for the FORK's model —
        // inherited-only, so an unsupported pairing degrades to None
        // with a warning, never a failed fork. A forker running with no
        // effort passes None through unchanged.
        loop_ctx.reasoning_effort = crate::agent::arming::arm_child_reasoning_effort(
            ctx.get_extension::<AgentModel>()
                .and_then(|live| live.reasoning_effort),
            &crate::agent::arming::ChildEffortSource::Inherited { child: "fork" },
            &args.model,
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork: {e}"),
        })?;
        // Hook coverage (parent → fork): the fork's loop dispatches
        // pre/post-tool hooks from its *own* LoopContext, so the parent's
        // shared registry must be installed here — otherwise operator
        // policy/observability hooks silently never see fork tool calls.
        // The same registry is also handed to the launch wrapper so the
        // subagent start/stop hooks fire around the fork exactly as they
        // do around a spawn (NH-006 R5 parity).
        let hooks = ctx.get_extension::<HookRegistry>();
        loop_ctx.hooks = hooks.as_ref().map(Arc::clone);
        // Per-agent action log: the fork's loop records its own tool
        // dispatches into the fork's log (installed on the fork context by
        // `build_fork_context`), so the fork's `action_log` queries work
        // and the parent's scoped queries see the fork's entries. The log
        // starts empty at the fork point — the seeded conversation is the
        // fork's memory; the action log records what the fork itself did.
        loop_ctx.action_log = child_ctx.get_extension::<ActionLog>();
        // Result delivery from the fork's own children: the loop drains
        // this receiver at the same step boundaries the root uses —
        // results bubble one hop per level (W3.4).
        loop_ctx.child_result_rx = fork_result_rx;
        loop_ctx.environment = Some(crate::system_prompt::environment::EnvironmentConfig {
            session_id: None,
            model: args.model.clone(),
        });

        let output_schema = build_fork_output_schema(&args.requirements);

        // Effective tools = full parent surface ∩ granted policy (R6):
        // `signal_agent` disappears under MessagingScope::None and a leaf
        // grant (remaining_depth == 0) strips spawn_agent AND fork — at
        // assembly, so the fork's tool definitions and its executor gate
        // agree (the call-rejection paths stay as defence-in-depth).
        let allow_list = effective_child_tools(parent_registry, None, &fork_policy, "fork");

        // Skill listing (parity with the root and spawn): advertise "#
        // Available Skills" on the fork's system prompt only when the parent
        // published a skill catalog AND the skill tool is on the fork's
        // resolved surface. Same shared mechanism + filtered listing the
        // root builder uses. Applied after the fork preamble + parent base
        // are composed into `loop_ctx`, so the listing lands after them.
        if let Some(catalog) = ctx.get_extension::<crate::skill::SkillCatalog>() {
            crate::agent::arming::install_child_skill_listing(
                &mut loop_ctx,
                &catalog,
                crate::agent::arming::child_skill_tool_available(
                    parent_registry,
                    allow_list.as_deref(),
                ),
            );
        }

        // Every agent's context carries its OWN launch model (parent-model
        // ground truth for the fork's own spawns) and the identity-free
        // base it COMPOSED WITH under `ParentSystemInstruction` — NOT its
        // own combined base. The combined base opens with this fork's
        // "Fork identity" preamble; publishing it would make a
        // fork-of-fork stack a second (stale) identity block under its
        // own fresh preamble. Publishing `parent_base` instead means
        // every fork level renders fresh preamble + the original
        // identity-free base, with exactly one identity block. (Spawn
        // children keep publishing their own base: theirs IS the
        // identity-free working instruction.)
        child_ctx.insert_extension(Arc::new(AgentModel {
            model: args.model.clone(),
            reasoning_effort: loop_ctx.reasoning_effort,
        }));
        child_ctx.insert_extension(Arc::new(ParentSystemInstruction::new(parent_base.clone())));

        let child_tools = super::live_tools::child_tool_snapshot(
            ctx,
            parent_registry,
            allow_list,
            None,
            Arc::clone(&child_ctx),
        )?;
        let executor = child_tools.executor;
        let tool_defs = child_tools.definitions;

        // R6: inbound steering channel — the parent keeps the sender via
        // the AgentHandle for `close_agent`'s shutdown steer; capacity
        // comes from the granted policy (DECISION M4 — never a hardcoded
        // library value).
        let (inbound_tx, inbound_rx) = inbound_channel(fork_policy.inbound_capacity);

        // `launch_fork` calls `tokio::spawn` to wrap `run_agent_step` so the
        // child runs concurrently and this function returns immediately —
        // matching `SpawnAgentTool`'s pattern (R1).
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

        // NH-006 R5 parity with spawn (C56): fire
        // SubagentHook::on_subagent_start before launching the fork.
        // Observational — Block has no semantics on start (the trait
        // method returns `()`). Absent registry → no hook to fire.
        if let Some(hooks_arc) = hooks.as_ref() {
            hooks_arc
                .run_subagent_start(&fork_id.to_string(), "fork")
                .await;
        }

        let handle = launch_fork(
            ForkLaunch {
                provider: Arc::clone(&infra.provider),
                executor,
                child_store,
                parent_store: Arc::clone(&infra.event_store),
                loop_ctx,
                tool_defs,
                output_schema,
                inbound_rx,
                request: args.request,
                model: args.model,
                agent_registry: Arc::clone(&infra.registry),
                result_sender: result_sender.map(|s| (*s).clone()),
                requirement_names,
                fork_id,
                parent_id: infra.agent_id,
                forked_session_id,
                event_sender: child_event_sender,
                reclaim: reclaim_handshake,
                lifecycle,
                hooks,
                router: Arc::clone(&infra.router),
                cancel: fork_cancel,
                config: fork_config,
            },
            inbound_tx,
        );
        handles.insert(handle);

        // Handshake: the handle is stored — tell the wrapper its
        // reclamation pass may run. This closes the insert/finish race
        // without a second reclaimer: a fork that finished before the
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
                fork_id = %fork_id,
                "fork: wrapper exited before the handle-installed ack; \
                 reclamation ownership lies with whoever ended the wrapper",
            );
        }

        Ok(ToolOutput::success(serde_json::json!({
            "agent_id": fork_id.to_string(),
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
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use chrono::Utc;
    use futures_util::{StreamExt, stream};
    use parking_lot::RwLock;
    use serde_json::json;
    use uuid::Uuid;

    use super::super::canonical_lifecycle_test_support::{
        canonical_item_values, contains_contiguous_items, stateless_payload_input,
        supported_non_audio_items, transcript_item,
    };
    use super::super::handle::AgentWakeRegistry;
    use super::super::infra::AgentToolInfra;
    use super::*;
    use crate::agent::child_policy::MessagingScope;
    use crate::agent::fork::{FORK_SYSTEM_PREAMBLE, ForkRequirement};
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentStatus;
    use crate::error::ProviderError;
    use crate::r#loop::inbound::{ChannelMessage, MessageKind};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::request::ProviderRequest;
    use crate::provider::traits::{Provider, ProviderStream};
    use crate::provider::usage::Usage;
    use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
    use crate::session::store::EventStore;
    use crate::tool::registry::ToolRegistry;
    use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "fork".to_string(),
            model_args: args,
            metadata: serde_json::Value::Null,
        }
    }

    fn done_event_tool_use() -> ProviderEvent {
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            },
            response_id: None,
        }
    }

    /// Provider that records every full [`ProviderRequest`] it receives
    /// while popping scripted response streams in order.
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

    fn structured_response_provider(payload: serde_json::Value) -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: payload.to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            // Fallback done-turn in case the runner loops after structured output.
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]))
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

    fn parent_ctx(
        provider: Arc<dyn Provider>,
        parent_id: Uuid,
        agent_registry: &Arc<RwLock<AgentRegistry>>,
        tool_registry: Arc<ToolRegistry>,
        router: Arc<MessageRouter>,
    ) -> (ToolContext, Arc<EventStore>) {
        let event_store = Arc::new(EventStore::new());
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(agent_registry),
            router,
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::clone(&event_store),
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
        (ctx, event_store)
    }

    struct GatedProvider {
        gate: Arc<tokio::sync::Notify>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }

    impl Provider for GatedProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            let mut lock = self.responses.lock().unwrap();
            let batch = if lock.is_empty() {
                vec![ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                }]
            } else {
                lock.remove(0)
            };
            drop(lock);
            let mut seq = Some(batch);
            let gate = Arc::clone(&self.gate);
            let s = stream::once(async move { gate.notified().await }).flat_map(move |()| {
                stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok))
            });
            Ok(Box::pin(s))
        }
    }

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

    /// R1: `fork.execute()` returns immediately while the child is still
    /// blocked behind a gated provider.
    #[tokio::test]
    async fn fork_returns_immediately_then_child_runs_async() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
            gate: Arc::clone(&gate),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ]]),
        });
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let started = std::time::Instant::now();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "summarise", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "fork must return within 50ms while child is gated; took {elapsed:?}",
        );
        assert_eq!(out.content["status"], "active");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        assert_eq!(
            agent_registry.read().get(fork_id).expect("entry").status,
            AgentStatus::Active,
        );

        gate.notify_one();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let mut status_rx = handle.status_rx.clone();
        handle.join_handle.await.expect("join");
        // Terminal transition retains the entry (status displays hold it)
        // with terminal status; the watch channel carries it too.
        assert_eq!(
            agent_registry
                .read()
                .get(fork_id)
                .expect("completed fork entry stays observable until reclaimed")
                .status,
            AgentStatus::Completed,
        );
        assert_eq!(*status_rx.borrow_and_update(), AgentStatus::Completed);
    }

    /// NH-006 R5 parity with spawn: `SubagentHook::on_subagent_start`
    /// fires before the fork launches and
    /// `SubagentHook::on_subagent_stop` fires from the fork's wrapper
    /// task once the run finishes — the pre-existing asymmetry (spawn
    /// fired both, fork fired neither) is closed.
    #[tokio::test]
    async fn subagent_hook_start_and_stop_fire_around_fork() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

        struct CountingSubagentHook {
            start_count: Arc<AtomicUsize>,
            stop_count: Arc<AtomicUsize>,
            seen_type: Arc<StdMutex<Option<String>>>,
        }

        #[async_trait]
        impl SubagentHook for CountingSubagentHook {
            async fn on_subagent_start(&self, _agent_id: &str, agent_type: &str) {
                self.start_count.fetch_add(1, AtomicOrdering::SeqCst);
                *self.seen_type.lock().unwrap() = Some(agent_type.to_owned());
            }
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                self.stop_count.fetch_add(1, AtomicOrdering::SeqCst);
                HookOutcome::Proceed
            }
        }

        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let seen_type = Arc::new(StdMutex::new(None));
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::Subagent(Box::new(CountingSubagentHook {
            start_count: Arc::clone(&start_count),
            stop_count: Arc::clone(&stop_count),
            seen_type: Arc::clone(&seen_type),
        })));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "summarise", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        assert_eq!(
            start_count.load(AtomicOrdering::SeqCst),
            1,
            "SubagentHook::on_subagent_start must fire exactly once per fork",
        );
        assert_eq!(
            stop_count.load(AtomicOrdering::SeqCst),
            1,
            "SubagentHook::on_subagent_stop must fire exactly once per fork",
        );
        assert_eq!(
            seen_type.lock().unwrap().as_deref(),
            Some("fork"),
            "the hook matcher input for forks is the literal role 'fork'",
        );
    }

    /// R2: fork running mid-turn — the parent's latest `AssistantMessage`
    /// has multiple `tool_calls`. The child store carries a synthetic
    /// `ToolResult` with `tool_name == "fork"` matching the fork's
    /// `tool_call_id`, and every other tool_call has a matching result.
    #[tokio::test]
    async fn fork_injects_synthetic_tool_result_for_orphan_fork_call() {
        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        parent_store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "go".to_string(),
            })
            .expect("seed user");
        parent_store
            .append(SessionEvent::AssistantMessage {
                response_items: Vec::new(),
                base: EventBase::new(None),
                content: "running batch".to_string(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: vec![
                    ToolCallEvent {
                        call_id: "tc-read".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({}),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    ToolCallEvent {
                        call_id: "tc-search".to_string(),
                        name: "search".to_string(),
                        arguments: serde_json::json!({}),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    ToolCallEvent {
                        call_id: "call-1".to_string(),
                        name: "fork".to_string(),
                        arguments: serde_json::json!({}),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                ],
                usage: EventUsage::default(),
                stop_reason: String::new(),
                response_id: None,
            })
            .expect("seed assistant");
        parent_store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc-read".to_string(),
                tool_name: "read".to_string(),
                output: serde_json::json!({"content": "x"}),
                spool_ref: None,
                duration_ms: 1,
            })
            .expect("seed read result");
        parent_store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc-search".to_string(),
                tool_name: "search".to_string(),
                output: serde_json::json!({"hits": []}),
                spool_ref: None,
                duration_ms: 1,
            })
            .expect("seed search result");

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "summarise", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let events = child_store.events();
        let synthetic = events.iter().find(|e| {
            matches!(
                e,
                SessionEvent::ToolResult {
                    tool_call_id,
                    tool_name,
                    ..
                } if tool_call_id == "call-1" && tool_name == "fork"
            )
        });
        assert!(
            synthetic.is_some(),
            "synthetic ToolResult with tool_name == 'fork' must be present",
        );

        let seeded_assistant = events.iter().rposition(|e| {
            matches!(
                e,
                SessionEvent::AssistantMessage { tool_calls, .. }
                    if tool_calls.iter().any(|tc| tc.call_id == "call-1")
            )
        });
        if let Some(idx) = seeded_assistant
            && let SessionEvent::AssistantMessage { tool_calls, .. } = &events[idx]
        {
            for tc in tool_calls {
                let has_result = events.iter().any(|e| {
                    matches!(
                        e,
                        SessionEvent::ToolResult { tool_call_id, .. } if tool_call_id == &tc.call_id
                    )
                });
                assert!(
                    has_result,
                    "tool_call {} must have a matching ToolResult in child seed events",
                    tc.call_id,
                );
            }
        }
    }

    /// R3: tools dispatched inside the fork see the *child's* `agent_id`,
    /// not the parent's. The fork's registry path nests under the parent's.
    #[tokio::test]
    async fn forked_child_has_correct_identity_and_hierarchical_path() {
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
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let seen_agent = Arc::new(StdMutex::new(None));
        let seen_parent = Arc::new(StdMutex::new(None));
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(IdentityStubTool {
            seen_agent: Arc::clone(&seen_agent),
            seen_parent: Arc::clone(&seen_parent),
        }));
        let tool_registry = Arc::new(tool_registry);

        let agent_registry = AgentRegistry::shared();
        let parent_guard = AgentRegistry::reserve(
            &agent_registry,
            "/parent".to_string(),
            "parent".to_string(),
            "opus".to_string(),
            None,
            test_envelope().child_policy,
            None,
        )
        .expect("reserve parent");
        let real_parent = parent_guard.id();
        parent_guard.confirm().expect("confirm parent");

        let (ctx, _parent_store) = parent_ctx(
            provider,
            real_parent,
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "introspect", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let entry = agent_registry.read().get(fork_id).expect("fork entry");
        assert!(
            entry.path.starts_with("/parent/fork/"),
            "fork path must nest under parent path: {}",
            entry.path,
        );

        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        assert_eq!(
            *seen_agent.lock().unwrap(),
            Some(fork_id),
            "child tool must observe the fork's own agent_id",
        );
        assert_eq!(
            *seen_parent.lock().unwrap(),
            Some(real_parent),
            "child tool must observe the parent as its parent_id",
        );
    }

    /// Defect 1 regression (critical): a forked child must be able to load a
    /// skill end-to-end. Previously `build_fork_context` never forwarded
    /// `SkillSearchPaths`/`SkillCatalog`, so the fork saw the `skill` tool
    /// but every call failed `MissingExtension`. Here the fork calls `skill`
    /// (then produces its structured output) and its store must carry a
    /// successful `skill` tool result containing the skill body.
    #[tokio::test]
    async fn forked_child_loads_a_skill_end_to_end() {
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

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-skill".to_string(),
                    call_id: None,
                    name: Some("skill".to_string()),
                    arguments_delta: json!({"name": "greet"}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
            crate::tools::skill::SkillToolConfig {
                shell_execution: false,
            },
        )));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(crate::tools::skill::SkillSearchPaths(paths)));
        ctx.insert_extension(catalog);

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "greet", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let loaded = child_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::ToolResult { tool_name, output, .. }
                    if tool_name == "skill" && output.to_string().contains("HELLO_FROM_GREET")
            )
        });
        assert!(
            loaded,
            "forked child must load the skill successfully (extensions forwarded): {:?}",
            child_store.events(),
        );
    }

    /// N-026 R6 (fork path): the fork's own tool context carries a
    /// `ScheduleHandle`, proven behaviorally — the fork calls the `cron`
    /// tool mid-run and the `schedule.created` event lands on the FORK's
    /// event store (never the parent's: a fork's schedules are its own).
    #[tokio::test]
    async fn forked_child_resolves_cron_tool_against_its_own_schedule_handle() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-cron".to_string(),
                    call_id: None,
                    name: Some("cron".to_string()),
                    arguments_delta:
                        json!({"op": "schedule", "every": "2h", "message": "fork check-in"})
                            .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]));

        let mut registry = ToolRegistry::new();
        crate::tools::registry_builder::register_cron_tool(&mut registry);
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(registry),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "request": "schedule a check-in", "model": "gpt-5.5", "requirements": [],
                })),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let created = |store: &EventStore| {
            store.events().into_iter().any(|e| {
                matches!(
                    &e,
                    SessionEvent::Custom { event_type, .. }
                        if event_type == crate::schedule::SCHEDULE_CREATED_EVENT_TYPE
                )
            })
        };
        assert!(
            created(&child_store),
            "the fork's cron call must persist schedule.created to the fork's own store",
        );
        assert!(
            !created(&parent_store),
            "the fork's schedule must never leak onto the parent's store",
        );
    }

    /// R4: `ForkComplete` event appended to parent's timeline with a
    /// round-trippable variant tag.
    #[tokio::test]
    async fn fork_complete_event_appended_to_parent_store() {
        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        let events = parent_store.events();
        let complete = events.iter().rev().find_map(|e| match e {
            SessionEvent::ForkComplete {
                forked_session_id,
                result_summary,
                ..
            } => Some((forked_session_id.clone(), result_summary.clone())),
            _ => None,
        });
        let (fsid, summary) = complete.expect("ForkComplete event present");
        assert_eq!(summary["response"], "done");
        // F9: this parent is EPHEMERAL (parent_ctx arms ephemeral_root),
        // so the fork has no session file and the completion reference
        // records honest absence — never a registry-id stand-in.
        assert!(
            fsid.is_none(),
            "an ephemeral fork's ForkComplete must carry forked_session_id: None, got {fsid:?}",
        );
        // The honest `session: None` reservation is on the parent's
        // (in-memory) timeline too — the ONLY trace an ephemeral child's
        // name allocation leaves.
        let reservation = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ChildBranch {
                    parent_session_id,
                    child_session_id,
                    path_address,
                    ..
                } => Some((
                    parent_session_id.clone(),
                    child_session_id.clone(),
                    path_address.clone(),
                )),
                _ => None,
            })
            .expect("the ephemeral parent's store carries the ChildBranch reservation");
        assert_eq!(
            reservation.0, None,
            "an ephemeral parent has no session id — honest None",
        );
        assert_eq!(
            reservation.1, None,
            "an ephemeral fork has no session id — honest None, never a fake id",
        );
        assert!(reservation.2.starts_with("root/fork-"), "{}", reservation.2);

        let event = SessionEvent::ForkComplete {
            base: EventBase::new(None),
            forked_session_id: fsid.clone(),
            result_summary: summary,
            usage: EventUsage::default(),
            duration_ms: 0,
        };
        let json_s = serde_json::to_string(&event).expect("serialize");
        let parsed: SessionEvent = serde_json::from_str(&json_s).expect("deserialize");
        match parsed {
            SessionEvent::ForkComplete {
                forked_session_id, ..
            } => {
                assert_eq!(forked_session_id, fsid);
            }
            other => panic!("expected ForkComplete, got {other:?}"),
        }
    }

    /// V2-R2 (persistent parent): fork mints a REAL on-disk child timeline
    /// under the root's `children/` dir, the parent's file carries the
    /// `ChildBranch` reservation (parent-first) and an honest
    /// `ForkComplete` pointing at the child session, the index row carries
    /// `rel_path` + `parent_id`, and the child session resumes through the
    /// manager like any other.
    #[tokio::test]
    async fn fork_under_persistent_parent_persists_child_timeline()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::session::manager::{CreateSessionOptions, SessionManager};
        use crate::session::persistence::io::read_session_events_for_entry;
        use crate::session::store::DurabilityPolicy;
        use crate::session::{SessionBinding, SessionBrancher};

        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "gpt-5.5".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .expect("create root session");
        let root_id = opened.entry.id.clone();
        let parent_store = Arc::new(opened.store);
        let inherited_items =
            supported_non_audio_items("fork_inherited", "Inherited canonical context.");
        let response_items = inherited_items
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, raw)| Ok(transcript_item(raw, u64::try_from(index)?)?))
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        parent_store.append(SessionEvent::AssistantMessage {
            response_items,
            base: EventBase::new(None),
            content: "stale inherited projection".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: Some("resp_fork_inherited".to_owned()),
        })?;
        parent_store.checkpoint()?;
        let binding = Arc::new(SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager.clone(),
                root_id.clone(),
                DurabilityPolicy::Flush,
            )),
            root_id.clone(),
            &[],
        ));

        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::clone(&parent_store),
            agent_id: parent,
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: binding,
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
        ctx.insert_extension(Arc::new(test_envelope()));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        // Index row: the fork's session is manifest-discoverable with
        // rel_path + parent linkage.
        let row = manager
            .resolve(&fork_id.to_string())
            .expect("fork session indexed");
        let rel = row.rel_path.as_deref().expect("child rows carry rel_path");
        assert!(
            rel.starts_with(&format!("{root_id}/children/fork-"))
                && std::path::Path::new(rel)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
            "child file must live under the root's children/ dir: {rel}",
        );
        assert_eq!(row.parent_id.as_deref(), Some(root_id.as_str()));

        // The child timeline is REAL on-disk write-through: its file exists
        // and replays the seeded history (provenance header + seed copy +
        // the fork's own run events).
        let child_file = tmp.path().join(rel);
        assert!(child_file.exists(), "fork timeline file must exist on disk");
        let child_read = read_session_events_for_entry(tmp.path(), &row).expect("child replays");
        assert!(
            child_read
                .events
                .iter()
                .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
            "the child's file carries its ChildBranch provenance header",
        );
        assert!(
            child_read
                .events
                .iter()
                .any(|e| matches!(e, SessionEvent::AssistantMessage { .. })),
            "the fork's own run events reach its on-disk timeline",
        );
        assert_eq!(
            canonical_item_values(&child_read.events),
            inherited_items,
            "the real fork seed must copy canonical items exactly and in order",
        );

        // Parent side, ON DISK: ChildBranch reservation (parent-first) and
        // the honest ForkComplete reference.
        let parent_entry = manager.resolve(&root_id).expect("root entry");
        let parent_read =
            read_session_events_for_entry(tmp.path(), &parent_entry).expect("parent replays");
        let branch = parent_read
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ChildBranch {
                    parent_session_id,
                    child_session_id,
                    path_address,
                    ..
                } => Some((
                    parent_session_id.clone(),
                    child_session_id.clone(),
                    path_address.clone(),
                )),
                _ => None,
            })
            .expect("parent file carries the ChildBranch reservation");
        assert_eq!(branch.0.as_deref(), Some(root_id.as_str()));
        assert_eq!(branch.1.as_deref(), Some(fork_id.to_string().as_str()));
        assert!(branch.2.starts_with("root/fork-"));
        let fork_complete = parent_read
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ForkComplete {
                    forked_session_id, ..
                } => Some(forked_session_id.clone()),
                _ => None,
            })
            .expect("parent file carries ForkComplete");
        assert_eq!(
            fork_complete.as_deref(),
            Some(fork_id.to_string().as_str()),
            "ForkComplete must reference the real child session, never a stand-in",
        );

        // And the child resumes through the manager like any session.
        let resumed = manager
            .resume(&fork_id.to_string(), DurabilityPolicy::Flush)
            .expect("child resumes");
        assert!(resumed.replay.replayed_events > 0);
        let resumed_events = resumed.store.events();
        assert_eq!(
            canonical_item_values(&resumed_events),
            inherited_items,
            "SessionManager::resume must retain the fork's inherited canonical history",
        );
        let replay_input = stateless_payload_input(&resumed_events)?;
        assert!(
            contains_contiguous_items(&replay_input, &inherited_items),
            "the resumed fork must replay inherited items without stream coordinates or reconstruction",
        );
        Ok(())
    }

    /// R5: the helper returns a combined system instruction containing the
    /// built fork preamble verbatim *plus* the parent's base, with the
    /// preamble first.
    #[test]
    fn fork_loop_context_inherits_parent_base_with_preamble() {
        let parent_base = "You are the parent. Be terse.";
        let slugs = vec!["check_code".to_owned()];
        let policy = test_envelope().child_policy;
        let preamble = build_fork_preamble(&ForkIdentity {
            parent_agent_id: "parent-agent-id",
            path_address: "root/fork-a",
            requirement_slugs: &slugs,
            granted: &policy,
        });
        let combined = combine_system_instruction(&preamble, parent_base);
        let loop_ctx = LoopContext::new(combined);
        let base = loop_ctx.base_system_instruction();
        assert!(
            base.contains(FORK_SYSTEM_PREAMBLE),
            "preamble missing: {base}"
        );
        assert!(
            base.contains("root/fork-a") && base.contains("check_code"),
            "structured identity missing: {base}",
        );
        assert!(base.contains(parent_base), "parent missing: {base}");
        assert!(
            base.find(FORK_SYSTEM_PREAMBLE) < base.find(parent_base),
            "preamble must precede parent base: {base}",
        );
    }

    struct SemaphoreProvider {
        sem: Arc<tokio::sync::Semaphore>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }

    impl Provider for SemaphoreProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            let mut lock = self.responses.lock().unwrap();
            let batch = if lock.is_empty() {
                vec![ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                }]
            } else {
                lock.remove(0)
            };
            drop(lock);
            let mut seq = Some(batch);
            let sem = Arc::clone(&self.sem);
            let s = stream::once(async move {
                let permit = sem.acquire().await.unwrap();
                permit.forget();
            })
            .flat_map(move |()| stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok)));
            Ok(Box::pin(s))
        }
    }

    /// R6: `AgentHandles::inbound_tx(fork_id)` returns `Some` and a message
    /// sent through it reaches the fork's inbound channel.
    ///
    /// The fork is held behind a semaphore-gated provider so the receiver
    /// is guaranteed to still be live when the test sends — making the
    /// inbound-delivery assertion deterministic. A semaphore (not Notify)
    /// is used because the runner may loop for a second provider call after
    /// the steer message, and each call needs its own independently
    /// consumable permit.
    #[tokio::test]
    async fn fork_inbound_channel_delivers_steer_message() {
        let sem = Arc::new(tokio::sync::Semaphore::new(0));
        let provider: Arc<dyn Provider> = Arc::new(SemaphoreProvider {
            sem: Arc::clone(&sem),
            responses: StdMutex::new(vec![
                vec![
                    ProviderEvent::ToolCallDelta {
                        item_id: "structured-out".to_string(),
                        call_id: None,
                        name: Some("structured_output".to_string()),
                        arguments_delta: json!({"response": "done", "requirements": {}})
                            .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    done_event_tool_use(),
                ],
                vec![ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                }],
                vec![ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                }],
            ]),
        });
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let inbound = handles.inbound_tx(fork_id).expect("inbound sender present");
        // Fork is gated — the receiver is still in `run_agent_step`, so the
        // bounded channel is live and the send is guaranteed to succeed.
        inbound
            .send(ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: Uuid::new_v4(),
                from: "test".to_string(),
                role: None,
                to_id: fork_id,
                content: "hello fork".to_string(),
                kind: MessageKind::Steer,
                seq: None,
                timestamp: Utc::now(),
            })
            .await
            .expect("send into fork inbound");

        // Release permits for all provider calls. The steer message causes
        // additional loop iterations; the provider returns EndTurn when
        // scripted batches are exhausted. Extra permits are harmless.
        sem.add_permits(10);
        let handle = handles.remove(fork_id).expect("handle");
        handle.join_handle.await.expect("join");
    }

    /// R7: fork with a tasks array produces structured output validating
    /// against the dynamically-built schema.
    #[tokio::test]
    async fn fork_with_requirements_produces_structured_output() {
        let valid = json!({
            "response": "all done",
            "requirements": {
                "a": {"completed": true, "completion_notes": "ok"},
                "b": {"completed": false, "completion_notes": "skipped"},
            },
        });
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: valid.to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            // Fallback done-turn in case the runner loops after structured output.
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "request": "split work",
                    "model": "gpt-5.5",
                    "requirements": [
                        {"name": "a", "description": "first"},
                        {"name": "b", "description": "second"},
                    ],
                })),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        let events = parent_store.events();
        let summary = events
            .iter()
            .rev()
            .find_map(|e| match e {
                SessionEvent::ForkComplete { result_summary, .. } => Some(result_summary.clone()),
                _ => None,
            })
            .expect("ForkComplete present");
        let schema = build_fork_output_schema(&[
            ForkRequirement {
                name: "a".to_string(),
                description: "first".to_string(),
            },
            ForkRequirement {
                name: "b".to_string(),
                description: "second".to_string(),
            },
        ]);
        let compiled = jsonschema::validator_for(&schema).expect("schema compiles");
        assert!(
            compiled.is_valid(&summary),
            "ForkComplete.result_summary must validate: {summary}",
        );
    }

    /// Unbounded-retention regression: with
    /// [`crate::tools::agent::ReclaimOnResultDelivery`] installed and a
    /// result channel present, a naturally-completed fork's registry
    /// entry AND parent-held handle are reclaimed once its result has
    /// been delivered.
    #[tokio::test]
    async fn fork_delivered_result_reclaims_when_marker_present() {
        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(crate::tools::agent::ReclaimOnResultDelivery));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("result within timeout")
            .expect("channel open");
        assert_eq!(result.agent_id, fork_id);

        let handles = ctx.get_extension::<AgentHandles>().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while agent_registry.read().get(fork_id).is_some() || handles.contains(fork_id) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for fork registry entry and handle reclamation",
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Hardening (owner ruling 2026-07-03): a fork must run with
    /// auto-compaction armed exactly like the root. The fork launch path
    /// calls the shared `arm_auto_compaction`, installing the token
    /// estimator and filling the fork's context window from the catalog
    /// for the fork's own model. This drives a fork whose first turn
    /// reports an oversized usage (setting the context-edit usage floor
    /// above the window) and asserts the fork's next preflight emitted a
    /// `loop.token_warning` on the fork's store — structurally impossible
    /// without the estimator and window the shared arming installs.
    #[tokio::test]
    async fn fork_child_arms_auto_compaction_preflight() {
        let catalog_model = crate::model_catalog::default_selection().model;
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(IdentityStubTool {
            seen_agent: Arc::new(StdMutex::new(None)),
            seen_parent: Arc::new(StdMutex::new(None)),
        }));
        let tool_registry = Arc::new(tool_registry);

        // Turn 1: a tool call (forces a second round-trip so a second
        // preflight runs) whose reported usage dwarfs any context window —
        // this becomes the usage floor. Turn 2: structured output so the
        // fork completes cleanly.
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-id".to_string(),
                    call_id: None,
                    name: Some("identity".to_string()),
                    arguments_delta: "{}".to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 100_000_000,
                        output_tokens: 0,
                        ..Usage::default()
                    },
                    response_id: None,
                },
            ],
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
        ]));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "run", "model": catalog_model, "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let fork_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let warned = fork_store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. } if event_type == "loop.token_warning"
            )
        });
        assert!(
            warned,
            "the fork's preflight must emit loop.token_warning, proving the \
             estimator and catalog window were armed on the fork",
        );
    }

    /// Hardening (owner ruling 2026-07-03), full trigger: a fork that
    /// inherits a long parent history and then reports an oversized usage
    /// must actually *compact* mid-run rather than die
    /// `ContextWindowExceeded`. The parent is seeded with more than
    /// `auto_compact_keep_recent_turns` (default 10) turns, the fork
    /// inherits them, and the first turn's oversized usage pushes the
    /// second preflight past the compaction threshold — proving the shared
    /// arming makes the trigger genuinely fire for a child, not merely warn.
    #[tokio::test]
    async fn fork_runs_auto_compaction_when_history_exceeds_window() {
        let catalog_model = crate::model_catalog::default_selection().model;
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(IdentityStubTool {
            seen_agent: Arc::new(StdMutex::new(None)),
            seen_parent: Arc::new(StdMutex::new(None)),
        }));
        let tool_registry = Arc::new(tool_registry);

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
            // Turn 1: tool call + oversized usage (sets the floor).
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-id".to_string(),
                    call_id: None,
                    name: Some("identity".to_string()),
                    arguments_delta: "{}".to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 100_000_000,
                        output_tokens: 0,
                        ..Usage::default()
                    },
                    response_id: None,
                },
            ],
            // The compaction summarization provider call (fired inside the
            // second preflight, before the second main turn).
            vec![
                ProviderEvent::TextDelta {
                    text: "summary of earlier turns".to_string(),
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ],
            // Turn 2: structured output so the fork completes.
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
        ]));
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );

        // Seed 12 user/assistant turns so the fork inherits more than the
        // default keep_recent_turns (10) — giving the compaction plan
        // something to elide.
        for i in 0..12 {
            parent_store
                .append(SessionEvent::UserMessage {
                    base: EventBase::new(None),
                    content: format!("q{i}"),
                })
                .expect("seed user");
            parent_store
                .append(SessionEvent::AssistantMessage {
                    response_items: Vec::new(),
                    base: EventBase::new(None),
                    content: format!("a{i}"),
                    thinking: String::new(),
                    reasoning: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: EventUsage::default(),
                    stop_reason: "end_turn".to_string(),
                    response_id: None,
                })
                .expect("seed assistant");
        }

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "run", "model": catalog_model, "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let fork_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let compacted = fork_store
            .events()
            .iter()
            .any(|e| matches!(e, SessionEvent::Compaction { .. }));
        assert!(
            compacted,
            "the fork must commit a Compaction event when its inherited \
             history and oversized usage cross the threshold",
        );
    }

    /// Permission-escape regression (blocker), end to end: a tool denied
    /// by the parent's policy must stay denied inside a fork — the fork
    /// model calls it, dispatch blocks it, and the tool body never runs.
    #[tokio::test]
    async fn denied_tool_stays_denied_inside_fork() {
        struct CountingStubTool {
            executions: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[async_trait]
        impl TestTool for CountingStubTool {
            fn name(&self) -> &'static str {
                "victim"
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
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(CountingStubTool {
            executions: Arc::clone(&executions),
        }));
        let tool_registry = Arc::new(tool_registry);

        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(
            crate::config::permissions::PermissionPolicy::from_patterns(&["victim"], &[], &[]),
        ));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "try the denied tool", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        assert_eq!(
            executions.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a tool denied in the parent must never execute inside a fork",
        );
    }

    /// R1 failure path: empty provider yields a run error — registry is
    /// marked `Failed` and the parent receives a failure result through the
    /// child result channel.
    #[tokio::test]
    async fn fork_failure_marks_failed_and_sends_result() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (ctx, _parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let sender = ChildResultSender(Arc::new(tx));
        ctx.insert_extension(Arc::new(sender));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "will-fail", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        // Terminal transition retains the entry with Failed status; the
        // result channel carries the failure.
        assert_eq!(
            agent_registry
                .read()
                .get(fork_id)
                .expect("failed fork entry stays observable until reclaimed")
                .status,
            AgentStatus::Failed,
        );
        let result = rx.try_recv().expect("failure result on the channel");
        assert_eq!(result.agent_id, fork_id);
        assert!(!result.succeeded, "fork must report failure");
        assert!(result.error.is_some(), "error message present on failure");
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

    /// Confinement-escape regression (blocker), end to end: a parent
    /// confined to a workspace root forks a child; the fork's `read` of
    /// an out-of-root file is REFUSED while an in-root read works.
    #[tokio::test]
    async fn forked_child_file_tools_respect_parent_confinement() {
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
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
        ]));

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(crate::tools::read::ReadTool::new()));
        let tool_registry = Arc::new(tool_registry);

        let agent_registry = AgentRegistry::shared();
        let (mut ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );
        ctx.confine_to_workspace(root.path().to_path_buf());

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "read files", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        let results: Vec<serde_json::Value> = child_store
            .events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::ToolResult {
                    tool_name, output, ..
                } if tool_name == "read" => Some(output.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 2, "both reads produced results: {results:?}");
        assert_eq!(
            results[0]["kind"], "confinement_refused",
            "the out-of-root read must be refused inside the fork: {}",
            results[0],
        );
        assert_eq!(
            results[1]["kind"], "text",
            "the in-root read must succeed inside the fork: {}",
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

    /// Hook-coverage regression (reviewer issue): a PreToolUse hook
    /// registered on the parent must observe a fork's tool calls — the
    /// fork's loop dispatches hooks from its own `LoopContext`, so the
    /// parent's registry must be forwarded.
    #[tokio::test]
    async fn parent_pre_tool_hook_fires_for_fork_tool_call() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};

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
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(IdentityStubTool {
            seen_agent: Arc::new(StdMutex::new(None)),
            seen_parent: Arc::new(StdMutex::new(None)),
        }));
        let tool_registry = Arc::new(tool_registry);

        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::PreTool(Box::new(CountingPreTool {
            tool_name: "identity",
            count: Arc::clone(&count),
        })));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "probe it", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            1,
            "a parent-registered PreToolUse hook must fire for the fork's tool call",
        );
    }

    /// Typed lifecycle: fork emits `SubagentLifecycle::Started` then
    /// `Completed` on the shared broadcast channel — child-tagged, with
    /// the fork descriptor, ordered wall-clock timestamps, and the
    /// fork's accumulated usage — appends the matching Custom audit
    /// events to the parent's store, and the result channel carries the
    /// same per-child usage.
    #[tokio::test]
    async fn fork_emits_typed_lifecycle_events_on_channel_and_parent_store() {
        use crate::agent::result_channel::ChildResultSender;
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
            SharedAgentEventChannel, SubagentKind, SubagentLifecycle,
        };

        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
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

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "summarise", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        assert!(
            out.content["path"].as_str().unwrap().contains("/fork/"),
            "fork output carries the registry path: {}",
            out.content,
        );
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        // Live carrier: child-tagged Started then Completed, with the
        // Started event preceding the fork's own provider events.
        let mut subagent_events = Vec::new();
        let mut first_child_event_is_started = None;
        while let Ok(ev) = brx.try_recv() {
            if ev.agent_id == fork_id && first_child_event_is_started.is_none() {
                first_child_event_is_started = Some(matches!(
                    ev.event,
                    AgentEventKind::Subagent(SubagentLifecycle::Started { .. })
                ));
            }
            if let AgentEventKind::Subagent(lifecycle) = ev.event {
                assert_eq!(ev.agent_id, fork_id, "lifecycle events are child-tagged");
                assert_eq!(&*ev.agent_role, "fork/gpt-5.5");
                subagent_events.push(lifecycle);
            }
        }
        assert_eq!(
            first_child_event_is_started,
            Some(true),
            "Started must precede the fork's own provider events",
        );
        assert_eq!(subagent_events.len(), 2, "exactly Started then Completed");
        match &subagent_events[0] {
            SubagentLifecycle::Started {
                parent_id,
                child_id,
                descriptor,
                ..
            } => {
                assert_eq!(*parent_id, parent);
                assert_eq!(*child_id, fork_id);
                assert_eq!(descriptor.kind, SubagentKind::Fork);
                assert_eq!(descriptor.role, "fork");
                assert_eq!(descriptor.model, "gpt-5.5");
                assert!(descriptor.profile.is_none(), "forks have no profile");
            }
            other => panic!("expected Started, got {other:?}"),
        }
        match &subagent_events[1] {
            SubagentLifecycle::Completed {
                parent_id,
                child_id,
                started_at,
                completed_at,
                usage,
                subtree_usage,
                succeeded,
                error,
                stop,
                ..
            } => {
                assert_eq!(*parent_id, parent);
                assert_eq!(*child_id, fork_id);
                assert!(*completed_at >= *started_at, "timestamps must be ordered");
                assert!(*succeeded);
                assert!(error.is_none());
                assert!(stop.is_none());
                assert_eq!(usage.input_tokens, 5, "per-fork usage must surface");
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(
                    subtree_usage.input_tokens, 5,
                    "a childless fork's subtree usage equals its own usage",
                );
                assert_eq!(subtree_usage.output_tokens, 2);
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        // Audit carrier: the parent store got both Custom events (in
        // addition to the existing ForkComplete completion reference).
        let custom: Vec<(String, serde_json::Value)> = parent_store
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
        assert_eq!(custom[0].1["descriptor"]["kind"], "fork");
        assert_eq!(custom[1].0, SUBAGENT_COMPLETED_EVENT_TYPE);
        assert_eq!(custom[1].1["succeeded"], true);
        assert!(
            parent_store
                .events()
                .iter()
                .any(|e| matches!(e, SessionEvent::ForkComplete { .. })),
            "the ForkComplete completion reference is still appended",
        );

        // The result channel carries the same per-fork usage, and the
        // childless fork's subtree total equals its own usage (W3.6).
        let result = rx.try_recv().expect("result on the channel");
        assert_eq!(result.agent_id, fork_id);
        assert_eq!(result.usage.input_tokens, 5);
        assert_eq!(result.usage.output_tokens, 2);
        assert_eq!(result.subtree_usage.input_tokens, 5);
        assert_eq!(result.subtree_usage.output_tokens, 2);
    }

    /// Terminal-transition race repro (production WARNs
    /// `fork: mark_completing failed ... agent not found` /
    /// `fork: mark_completed failed ... agent not found`):
    ///
    /// The fork's completion wrapper owns the terminal sequence
    /// mark → ForkComplete → lifecycle → delivery → reclaim. This test
    /// parks the wrapper deterministically *after* the fork's run has
    /// finished and *before* `mark_fork_terminal`, by gating
    /// `SubagentHook::on_subagent_stop` (the only await between the two).
    /// While the wrapper is parked, a `close_agent` issued by an agent
    /// that holds NO handle for the fork targets it.
    ///
    /// Before the fix, `close_agent` marked the still-Active entry
    /// Completing → Completed and removed it — stealing the wrapper's
    /// terminal transition — so the wrapper's own mark hit `NotFound`
    /// (the production WARN pair) and the closer falsified the fork's
    /// recorded outcome. After the fix, a closer that cannot stop the
    /// fork's task (no handle) must not touch its live registry entry:
    /// the entry survives the close, and the wrapper's terminal mark
    /// lands exactly once.
    #[tokio::test]
    async fn close_without_handle_cannot_steal_fork_terminal_transition() {
        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};
        use crate::tools::agent::coord::CloseAgentTool;

        struct GateStopHook {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }

        #[async_trait]
        impl SubagentHook for GateStopHook {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                self.entered.notify_one();
                self.release.notified().await;
                HookOutcome::Proceed
            }
        }

        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::Subagent(Box::new(GateStopHook {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "race", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        // The wrapper is now parked inside on_subagent_stop: the fork's
        // run is finished, the terminal mark has NOT happened yet.
        // (`notify_one` stores a permit, so this is race-free even if the
        // wrapper reached the gate before we subscribed.)
        entered.notified().await;

        // A different agent — same registry, no handle for the fork —
        // closes it while the wrapper still owes the terminal mark.
        let closer_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let closer_infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider: closer_provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: None,
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let closer_ctx = ToolContext::empty();
        closer_ctx.insert_extension(closer_infra);
        let close_out = CloseAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "close-1".to_string(),
                    tool_name: "close_agent".to_string(),
                    model_args: json!({"agent_id": fork_id.to_string()}),
                    metadata: serde_json::Value::Null,
                },
                &closer_ctx,
            )
            .await
            .expect("close executes");

        // INVARIANT: the wrapper still owes this entry a terminal
        // transition, so the close must not have removed it. Before the
        // fix the entry is gone here — the wrapper's subsequent
        // mark_completing/mark_completed hit NotFound (the WARN pair).
        assert!(
            agent_registry.read().get(fork_id).is_some(),
            "a closer without the fork's handle must never remove the live \
             registry entry out from under the completion wrapper; close output: {:?}",
            close_out.content,
        );
        assert_eq!(
            close_out.content["shut_down"][0]["status"], "unreachable",
            "close must report honestly that it cannot force-stop an agent \
             whose handle it does not hold: {:?}",
            close_out.content,
        );

        // Release the wrapper and let it finish its terminal sequence.
        release.notify_one();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        // The wrapper's mark landed exactly once: Completed, observable.
        assert_eq!(
            agent_registry
                .read()
                .get(fork_id)
                .expect("the wrapper's terminal transition must find its entry")
                .status,
            AgentStatus::Completed,
        );
    }

    /// Close-with-handle determinism: the closer triggers the fork's
    /// cooperative cancellation token and JOINS the wrapper before
    /// touching the registry, so exactly one owner performs the terminal
    /// transition — the wrapper itself, with the run's REAL outcome. With
    /// the wrapper parked pre-mark (gated stop hook) after its run
    /// already completed, the close waits for the wrapper (gate released
    /// concurrently), the wrapper's own mark lands exactly once
    /// (`Completed` — the run genuinely finished), and the closer's job
    /// reduces to reclaim: the entry is gone, a `Completed` tombstone
    /// preserves the real outcome, the handle is owned by the closer,
    /// and no "agent not found" race is possible. The closer never
    /// aborts the wrapper and never rewrites the recorded outcome.
    #[tokio::test]
    async fn close_with_handle_joins_wrapper_then_owns_terminal_transition() {
        use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};
        use crate::tools::agent::coord::CloseAgentTool;

        struct GateStopHook {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }

        #[async_trait]
        impl SubagentHook for GateStopHook {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                self.entered.notify_one();
                self.release.notified().await;
                HookOutcome::Proceed
            }
        }

        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::Subagent(Box::new(GateStopHook {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })));
        ctx.insert_extension(Arc::new(hook_registry));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "race", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        entered.notified().await;

        // The parent itself closes the fork: it holds the handle, so the
        // close cancels the run (already finished here), then JOINS the
        // parked wrapper. The join waits for the wrapper's own terminal
        // sequence, so the gate must be released concurrently —
        // `notify_one` stores a permit, making the join/release ordering
        // race-free.
        let close_tool = CloseAgentTool::new();
        let close_envelope = ToolEnvelope {
            tool_call_id: "close-1".to_string(),
            tool_name: "close_agent".to_string(),
            model_args: json!({"agent_id": fork_id.to_string(), "reason": "wrap up"}),
            metadata: serde_json::Value::Null,
        };
        let close_fut = close_tool.execute(&close_envelope, &ctx);
        let release_fut = async {
            release.notify_one();
        };
        let (close_result, ()) = tokio::join!(close_fut, release_fut);
        let close_out = close_result.expect("close executes");
        assert_eq!(
            close_out.content["shut_down"][0]["status"], "reclaimed",
            "the joined wrapper records the run's real outcome itself; the \
             closer's job is reclaim-only: {:?}",
            close_out.content,
        );

        let reg = agent_registry.read();
        assert!(reg.get(fork_id).is_none(), "the closer reclaims the entry");
        let tombstone = reg
            .tombstone(fork_id)
            .expect("the recorded outcome stays reportable via its tombstone");
        assert_eq!(
            tombstone.status,
            AgentStatus::Completed,
            "the run genuinely completed before the close — the wrapper's \
             real outcome is preserved, never rewritten by the closer",
        );
        drop(reg);
        assert!(
            !ctx.get_extension::<AgentHandles>()
                .unwrap()
                .contains(fork_id),
            "the closer takes ownership of the handle",
        );
    }

    /// Mid-run close terminates the fork's inner run (HIGH-fix
    /// regression, fork path — mirrors the spawn-side test): a fork
    /// parked inside an in-flight provider call is closed. The handle's
    /// cancellation token terminates the run itself, the wrapper records
    /// the real outcome (registry `Failed`, `AgentStopReason::Cancelled`
    /// on the result channel), and the run never reaches another
    /// provider iteration.
    #[tokio::test]
    async fn close_mid_run_cancels_fork_inner_run_and_records_cancelled_outcome() {
        use crate::agent::output::AgentStopReason;
        use crate::agent::result_channel::ChildResultSender;
        use crate::tools::agent::coord::CloseAgentTool;

        /// Provider whose stream never yields: the fork parks inside the
        /// in-flight provider call until cancelled. Counts `stream()`
        /// calls and notifies `entered` on each.
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

        let entered = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn Provider> = Arc::new(ParkedProvider {
            entered: Arc::clone(&entered),
            calls: Arc::clone(&calls),
        });
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "long haul", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();

        // Deterministic hook: the fork is inside its first in-flight
        // provider call (`notify_one` stores a permit — race-free).
        entered.notified().await;

        let close_out = CloseAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "close-1".to_string(),
                    tool_name: "close_agent".to_string(),
                    model_args: json!({
                        "agent_id": fork_id.to_string(),
                        "reason": "stand down",
                    }),
                    metadata: serde_json::Value::Null,
                },
                &ctx,
            )
            .await
            .expect("close executes");

        assert_eq!(
            close_out.content["shut_down"][0]["status"], "reclaimed",
            "cancellation lets the fork wrapper finish its own terminal sequence: {:?}",
            close_out.content,
        );
        let reg = agent_registry.read();
        assert!(reg.get(fork_id).is_none(), "entry reclaimed by the close");
        let tombstone = reg.tombstone(fork_id).expect("tombstone retained");
        assert_eq!(
            tombstone.status,
            AgentStatus::Failed,
            "a cancelled fork records Failed — never Completed",
        );
        drop(reg);

        let result = rx
            .try_recv()
            .expect("the wrapper delivered the cancelled outcome before the close returned");
        assert_eq!(result.agent_id, fork_id);
        assert!(!result.succeeded, "a cancelled fork is not a success");
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the fork's inner run must stop at the cancelled provider call, \
             not continue to further iterations",
        );
        assert!(
            !ctx.get_extension::<AgentHandles>()
                .unwrap()
                .contains(fork_id),
            "the closer takes ownership of the handle",
        );
    }

    /// Production regression (action-log tree): a fork inherits the
    /// `action_log` TOOL through the shared registry but previously
    /// received no `ActionLog` extension — every call inside the fork
    /// failed with `MissingExtension`. The fork now carries its own
    /// per-agent log, which starts EMPTY at the fork point (its seeded
    /// conversation is its memory; its action log records what it did) —
    /// even when the parent's own log already has entries. The parent
    /// federates over the fork's entries with `scope: "all"`.
    #[tokio::test]
    async fn fork_action_log_query_works_and_log_starts_at_fork_point() {
        use crate::session::action_log::{ActionLog, CompletionRecord, Outcome};
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
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(ActionLogTool::new()));
        let tool_registry = Arc::new(tool_registry);

        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            tool_registry,
            Arc::new(MessageRouter::new()),
        );
        // The parent's own log already holds an entry: the fork's log
        // must NOT inherit it.
        let parent_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        parent_log.record_completion(CompletionRecord {
            tool_name: "read",
            tool_call_id: "parent-call",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &json!({ "path": "x", "lines": 1 }),
            args: json!({}),
            duration_ms: 1,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });
        ctx.insert_extension(Arc::clone(&parent_log));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "inspect your log", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        let child_store = Arc::clone(&handle.event_store);
        handle.join_handle.await.expect("join");

        // The fork's action_log call succeeded — the MissingExtension
        // regression is pinned here — and saw an EMPTY log: the fork's
        // log starts at the fork point, not at the parent's history.
        let result = child_store
            .events()
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::ToolResult {
                    tool_name,
                    tool_call_id,
                    output,
                    ..
                } if tool_name == "action_log" && tool_call_id == "tc-log" => Some(output),
                _ => None,
            })
            .expect("the fork's action_log call produced a result");
        assert!(
            result.get("error").is_none(),
            "the fork's action_log query must succeed: {result}",
        );
        assert_eq!(
            result["count"], 0,
            "the fork's log starts empty at the fork point: {result}",
        );

        // Federation: the parent's scope=all sees the fork's recorded
        // call, labeled with the fork's registry path.
        let federated = ActionLogTool::new()
            .execute(
                &ToolEnvelope {
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
        let fork_entry = entries
            .iter()
            .find(|e| e["tool"] == "action_log")
            .expect("the fork's call surfaces in the parent's scope=all");
        assert!(
            fork_entry["agent"].as_str().unwrap().contains("/fork/"),
            "the fork's entry is labeled with its registry path: {fork_entry}",
        );
        assert!(
            entries.iter().any(|e| e["id"] == "parent-call"),
            "the parent's own entry interleaves into scope=all",
        );
    }

    /// Route ownership (W3.2): the fork launch registers the inbound
    /// route before the task starts and the completion wrapper
    /// deregisters at the run's end.
    #[tokio::test]
    async fn fork_registers_route_at_launch_and_deregisters_at_terminal() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
            gate: Arc::clone(&gate),
            responses: StdMutex::new(vec![vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ]]),
        });
        let agent_registry = AgentRegistry::shared();
        let router = Arc::new(MessageRouter::new());
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::clone(&router),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "wait", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        assert!(
            router.is_routed(fork_id),
            "the launch path must register the fork's inbound route",
        );

        gate.notify_one();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");
        assert!(
            !router.is_routed(fork_id),
            "the completion wrapper must deregister the route at the run's end",
        );
    }

    /// Missing-envelope boundary: a context that can fork but carries no
    /// [`CoordinationEnvelope`] is a wiring error — fork refuses with a
    /// typed `MissingExtension` naming the envelope, leaking no
    /// reservation.
    #[tokio::test]
    async fn fork_requires_coordination_envelope() {
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

        let tool = ForkTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({"request": "x", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect_err("fork without an envelope must fail typed");
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
            "no reservation may leak from the refused fork",
        );
    }

    // -- W3.4: budgeted recursive delegation (fork side) ---------------------

    /// A `child_policy` argument that widens the caller's grant is refused
    /// typed (nothing reserved); omitting it stamps the caller's policy
    /// with the delegation depth decremented one level, and the fork path
    /// nests under the spawner.
    /// A typo'd top-level key must fail loudly — silently dropping a
    /// misspelled `child_policy` would hand the fork a default grant
    /// where the caller intended a narrowing. Mirrors the spawn-side
    /// pin so `ForkArgs`' deny_unknown_fields cannot regress silently.
    #[tokio::test]
    async fn fork_rejects_unknown_arg_keys() {
        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        ctx.insert_extension(Arc::new(test_envelope()));
        let tool = ForkTool::new();

        let err = tool
            .execute(
                &envelope_for(json!({
                    "request": "r", "model": "gpt-5.5", "requirements": [],
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

    #[tokio::test]
    async fn fork_stamps_decremented_grant_and_refuses_widening() {
        let provider =
            structured_response_provider(json!({"response": "done", "requirements": {}}));
        let parent = Uuid::new_v4();
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            parent,
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope.clone()));
        let tool = ForkTool::new();

        let err = tool
            .execute(
                &envelope_for(json!({
                    "request": "r", "model": "gpt-5.5", "requirements": [],
                    "child_policy": {
                        "messaging": "siblings_and_parent",
                        "delegation": {"remaining_depth": 2, "max_concurrent_children": 32},
                        "inbound_capacity": 32,
                    },
                })),
                &ctx,
            )
            .await
            .expect_err("widening must be refused");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("remaining_depth = 2 exceeds"),
                    "names the caller's budget: {reason}",
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
        assert!(
            agent_registry.read().is_empty(),
            "a refused fork reserves nothing",
        );

        let out = tool
            .execute(
                &envelope_for(json!({"request": "r", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        assert!(!out.is_error(), "{:?}", out.content);
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let path = out.content["path"].as_str().unwrap();
        assert!(path.starts_with("/fork/"), "{path}");
        let entry = agent_registry.read().get(fork_id).expect("entry");
        assert_eq!(
            entry.policy.delegation.remaining_depth, 1,
            "default derivation decrements the caller's depth 2 to 1",
        );
        assert_eq!(entry.policy.messaging, envelope.child_policy.messaging);
        assert_eq!(
            entry.policy.inbound_capacity,
            envelope.child_policy.inbound_capacity,
        );
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");
    }

    /// DECISIONS §0.6(c) on the fork surface: the model-suppliable
    /// `loop_config.max_iterations` grant is removed. It is absent from the
    /// fork schema and, because `loop_config` is `deny_unknown_fields`, a
    /// fork that still passes it is rejected loudly at the argument
    /// boundary — never silently dropped, and nothing is reserved.
    #[tokio::test]
    async fn fork_rejects_removed_max_iterations_grant() {
        use crate::agent::result_channel::ChildResultSender;

        // The fork schema no longer advertises the knob under loop_config.
        let tool = ForkTool::new();
        let loop_config = &tool.input_schema()["properties"]["child_policy"]["properties"]["loop_config"]
            ["properties"];
        assert!(
            loop_config.get("max_iterations").is_none(),
            "max_iterations must be absent from the fork loop_config schema: {loop_config:?}",
        );

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let err = tool
            .execute(
                &envelope_for(json!({
                    "request": "r", "model": "gpt-5.5", "requirements": [],
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
            "a refused fork reserves nothing",
        );
    }

    /// Routes provider scripts so a mid-tree fork and the grandchild it
    /// spawns share the one workspace provider deterministically; the
    /// fork's would-stop turn is held until the registry shows the
    /// grandchild reclaimed (which guarantees its result is already in
    /// the fork's own channel).
    struct ForkTreeProvider {
        registry: Arc<RwLock<AgentRegistry>>,
        fork_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for ForkTreeProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            use std::sync::atomic::Ordering as AtomicOrdering;
            // The managed dynamic-context Developer message now rides at the
            // tail of every request (prompt-cache fix), so route on the last
            // non-Developer message — the turn content that actually seeds
            // this fork.
            let last = request
                .messages
                .iter()
                .rev()
                .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            let end_turn = ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..Usage::default()
                },
                response_id: None,
            };
            if last == "fork-grandchild-task" {
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "fork grandchild says hi".to_string(),
                    }),
                    Ok(end_turn),
                ])));
            }
            let call = self.fork_calls.fetch_add(1, AtomicOrdering::SeqCst);
            match call {
                0 => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-grandchild".to_string(),
                        call_id: None,
                        name: Some("spawn_agent".to_string()),
                        arguments_delta: json!({
                            "task": "fork-grandchild-task",
                            "model": crate::model_catalog::default_selection().model,
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
                                .any(|t| t.path.contains("/spawn/"));
                            if reclaimed {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        let snapshot = registry.read().list();
                        let tombstones = registry.read().tombstones();
                        panic!(
                            "fork grandchild was never reclaimed — test cannot proceed; \
                             entries={snapshot:?}; tombstones={tombstones:?}",
                        );
                    })
                    .flat_map(move |()| {
                        stream::iter(vec![
                            Ok(ProviderEvent::TextDelta {
                                text: "waited for grandchild".to_string(),
                            }),
                            Ok(end_turn.clone()),
                        ])
                    });
                    Ok(Box::pin(s))
                }
                _ => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "structured-out".to_string(),
                        call_id: None,
                        name: Some("structured_output".to_string()),
                        arguments_delta: json!({
                            "response": "fork done after grandchild",
                            "requirements": {},
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_event_tool_use()),
                ]))),
            }
        }
    }

    /// W3.4 end-to-end on the fork surface: a fork granted depth ≥ 1
    /// spawns a grandchild; the grandchild's result is delivered into the
    /// **fork's** conversation (one hop — never to the root), the fork's
    /// structured result reaches the root's channel, and every registry
    /// entry is reclaimed at every level.
    #[tokio::test]
    async fn fork_drains_its_childrens_results_one_hop_and_reclaims() {
        let agent_registry = AgentRegistry::shared();
        let provider: Arc<dyn Provider> = Arc::new(ForkTreeProvider {
            registry: Arc::clone(&agent_registry),
            fork_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(crate::tools::agent::SpawnAgentTool::new()));
        let root_id = Uuid::new_v4();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            root_id,
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        let mut envelope = test_envelope();
        envelope.child_policy.delegation.remaining_depth = 2;
        ctx.insert_extension(Arc::new(envelope));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
        ctx.insert_extension(Arc::new(
            crate::tools::agent::reclaim::ReclaimOnResultDelivery,
        ));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "fork-task", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        assert!(!out.is_error(), "{:?}", out.content);
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let fork_path = out.content["path"].as_str().unwrap().to_string();

        // Take the handle now, before the wrapper's reclamation pass, so
        // the fork's store stays inspectable. Registry reclamation is
        // unaffected — the wrapper's handle removal is idempotent.
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle stored");
        let fork_store = Arc::clone(&handle.event_store);

        let result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("fork result must arrive")
            .expect("channel open");
        assert_eq!(result.agent_id, fork_id);
        assert!(result.succeeded, "{:?}", result.error);
        assert!(
            rx.try_recv().is_err(),
            "the grandchild's result must never reach the root directly",
        );
        // W3.6 rollup on the fork surface: the grandchild's run made one
        // provider call at (5, 2) (`ForkTreeProvider`'s grandchild turn),
        // so the fork's subtree total is exactly its own usage plus that
        // one delivered subtree — each level counted once, never folded
        // into the fork's own `usage`.
        assert_eq!(
            result.subtree_usage.input_tokens,
            result.usage.input_tokens + 5,
            "fork subtree = own + grandchild, exactly once",
        );
        assert_eq!(
            result.subtree_usage.output_tokens,
            result.usage.output_tokens + 2,
        );

        let injected = fork_store.events().iter().any(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("<agent_result")
                        && content.contains("fork grandchild says hi")
            )
        });
        assert!(
            injected,
            "the grandchild's framed result must be injected into the fork's conversation",
        );

        // Reclamation at every level, with the grandchild nested under
        // the fork's path.
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
        assert_eq!(tombstones.len(), 2, "fork + grandchild reclaimed");
        let grandchild_tomb = tombstones
            .iter()
            .find(|t| t.id != fork_id)
            .expect("grandchild tombstone");
        assert_eq!(grandchild_tomb.parent_id, Some(fork_id));
        assert!(
            grandchild_tomb
                .path
                .starts_with(&format!("{fork_path}/spawn/")),
            "grandchild path nests under the fork: {}",
            grandchild_tomb.path,
        );
    }

    /// W3.5: a fork's run token is created as a child of the forker's
    /// published [`AgentCancellation`] token, so cancelling the PARENT
    /// token alone — never touching the fork's handle — terminates the
    /// fork's in-flight run, whose wrapper records the real Cancelled
    /// outcome through its normal terminal sequence (mirrors the spawn
    /// cascade tests).
    #[tokio::test]
    async fn cancelling_parent_token_cascades_to_in_flight_fork() {
        use crate::agent::output::AgentStopReason;
        use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
        use crate::tools::agent::AgentCancellation;

        /// Never-yielding provider: the fork parks inside its first
        /// in-flight call and notifies `entered`.
        struct ParkedForkProvider {
            entered: Arc<tokio::sync::Notify>,
        }
        impl Provider for ParkedForkProvider {
            fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
                self.entered.notify_one();
                Ok(Box::pin(stream::pending::<
                    Result<ProviderEvent, ProviderError>,
                >()))
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let provider: Arc<dyn Provider> = Arc::new(ParkedForkProvider {
            entered: Arc::clone(&entered),
        });
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );
        let parent_cancel = tokio_util::sync::CancellationToken::new();
        ctx.insert_extension(Arc::new(AgentCancellation(parent_cancel.clone())));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(16);
        ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(
                    json!({"request": "summarise", "model": "gpt-5.5", "requirements": []}),
                ),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().expect("id")).expect("uuid");
        entered.notified().await;

        // Cancel the PARENT's token only; the fork's child token observes
        // it through tokio_util's cascade.
        parent_cancel.cancel();

        let handle = ctx
            .get_extension::<AgentHandles>()
            .expect("handles")
            .remove(fork_id)
            .expect("handle stored");
        handle
            .join_handle
            .await
            .expect("wrapper joins after the cascaded cancel");

        let result = rx
            .try_recv()
            .expect("the fork's wrapper delivered the real outcome before it ended");
        assert_eq!(result.agent_id, fork_id);
        assert!(!result.succeeded, "a cancelled fork is not a success");
        assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
        assert_eq!(
            agent_registry
                .read()
                .get(fork_id)
                .expect("entry observable (no reclaim marker installed)")
                .status,
            AgentStatus::Failed,
            "a cancelled fork records Failed — never Completed",
        );
    }

    // ----- agent-variants (R4/R5/R6/§7) -----------------------------------

    /// §7 (fork side): a fork on an uncatalogued model is rejected BEFORE
    /// anything is reserved — a typed error naming the model, no registry
    /// entry, no burned name.
    #[tokio::test]
    async fn fork_with_uncatalogued_model_is_rejected_before_reservation() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_registry = AgentRegistry::shared();
        let (ctx, parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(ToolRegistry::new()),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let err = tool
            .execute(
                &envelope_for(json!({
                    "request": "r", "model": "not-in-catalog-model-xyz", "requirements": [],
                })),
                &ctx,
            )
            .await
            .expect_err("an uncatalogued fork model must be rejected");
        assert!(
            err.to_string().contains("not-in-catalog-model-xyz"),
            "the rejection names the model: {err}",
        );
        assert!(
            agent_registry.read().is_empty(),
            "the rejection precedes the reservation",
        );
        assert!(
            parent_store.events().is_empty(),
            "nothing was persisted for the refused fork",
        );
    }

    /// R4 + R5: the fork's context carries the identity-free parent base
    /// it COMPOSED WITH under `ParentSystemInstruction` — never its own
    /// combined base (whose leading "Fork identity" preamble would stack
    /// stale under a fork-of-fork's fresh preamble).
    #[tokio::test]
    async fn fork_child_context_carries_the_composed_with_parent_base() {
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
                    .get_extension::<ParentSystemInstruction>()
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
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta:
                        json!({"response": "done", "requirements": {"check_code": {}}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]));

        let seen = Arc::new(StdMutex::new(None));
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(BaseProbe {
            seen: Arc::clone(&seen),
        }));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        // The forker's own base, as its assembly path would publish it.
        ctx.insert_extension(Arc::new(ParentSystemInstruction::new("PARENT-BASE-MARKER")));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "request": "probe your base",
                    "model": "gpt-5.5",
                    "requirements": [
                        {"name": "check code", "description": "check the code"}
                    ],
                })),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        let base = seen
            .lock()
            .unwrap()
            .clone()
            .expect("the fork's context must publish ParentSystemInstruction");
        assert_eq!(
            base, "PARENT-BASE-MARKER",
            "the fork publishes the identity-free base it composed with — \
             not its own combined base, whose fork-identity preamble would \
             stack under a fork-of-fork's fresh preamble",
        );
        assert!(
            !base.contains(FORK_SYSTEM_PREAMBLE),
            "the published base must carry NO fork preamble: {base}",
        );
    }

    /// Fork-of-fork regression: the grandchild's actual base instruction
    /// (its provider request's system content) renders a fresh preamble
    /// plus the ORIGINAL identity-free base — exactly one "Fork identity"
    /// block, never the parent fork's stale identity stacked under it.
    #[tokio::test]
    async fn fork_of_fork_base_instruction_has_exactly_one_identity_block() {
        // Provider shared by every level (the fork forwards the parent's
        // provider): captures each request so the grandchild's system
        // instruction can be asserted from ground truth. Scripted
        // streams: level-1 fork calls `fork` (the nested launch), then
        // both forks return the identical requirement-less structured
        // output, so the concurrent pop order between level 1's closing
        // stream and level 2's only stream cannot skew the script.
        let structured_done = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![
                vec![
                    ProviderEvent::ToolCallDelta {
                        item_id: "tc-nested-fork".to_string(),
                        call_id: None,
                        name: Some("fork".to_string()),
                        arguments_delta: json!({
                            "request": "go one level deeper",
                            "model": "gpt-5.5",
                            "requirements": [],
                        })
                        .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    done_event_tool_use(),
                ],
                structured_done.clone(),
                structured_done,
            ]),
        });

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(ForkTool::new()));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );
        // Depth-2 envelope so the level-1 fork (depth 1) still carries
        // the fork tool for the nested launch. Deliberate test values.
        {
            use crate::agent::child_policy::DelegationBudget;
            ctx.insert_extension(Arc::new(CoordinationEnvelope {
                child_policy: ChildPolicy {
                    messaging: MessagingScope::SiblingsAndParent,
                    delegation: DelegationBudget {
                        remaining_depth: 2,
                        max_concurrent_children: 4,
                    },
                    inbound_capacity: 8,
                    loop_config: None,
                },
                child_result_capacity: 16,
            }));
        }
        ctx.insert_extension(Arc::new(ParentSystemInstruction::new("PARENT-BASE-MARKER")));

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({
                    "request": "fork twice",
                    "model": "gpt-5.5",
                    "requirements": [],
                })),
                &ctx,
            )
            .await
            .expect("level-1 fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join level-1 fork");

        // The grandchild runs concurrently with (and possibly beyond)
        // level 1's join: poll until BOTH fork levels' requests are
        // captured — two distinct system instructions carrying the fork
        // preamble.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let fork_bases: Vec<String> = loop {
            let mut bases: Vec<String> = captured
                .lock()
                .unwrap()
                .iter()
                .filter_map(|request| {
                    let system = request.messages.iter().find_map(|message| {
                        (message.role == crate::provider::request::MessageRole::System)
                            .then(|| message.content.clone())
                            .flatten()
                    })?;
                    system.contains(FORK_SYSTEM_PREAMBLE).then_some(system)
                })
                .collect();
            bases.sort();
            bases.dedup();
            if bases.len() >= 2 {
                break bases;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "both fork levels must issue provider requests; saw {bases:?}",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        for base in &fork_bases {
            assert_eq!(
                base.matches("## Fork identity").count(),
                1,
                "every fork level renders exactly ONE identity block: {base}",
            );
            assert!(
                base.contains("PARENT-BASE-MARKER"),
                "…over the ORIGINAL identity-free base: {base}",
            );
        }
    }

    /// R6 (fork side): a leaf fork (granted remaining_depth == 0 — the
    /// default derivation from a depth-1 forker) is shown NEITHER
    /// spawn_agent nor fork in its provider payload.
    #[tokio::test]
    async fn leaf_fork_provider_tool_list_omits_spawn_and_fork() {
        struct ToolsCapturingProvider {
            captured: Arc<StdMutex<Vec<crate::provider::tools::ProviderToolDefinition>>>,
            responses: StdMutex<Vec<Vec<ProviderEvent>>>,
        }
        impl Provider for ToolsCapturingProvider {
            fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
                self.captured.lock().unwrap().clone_from(&request.tools);
                let seq = self.responses.lock().unwrap().remove(0);
                Ok(Box::pin(stream::iter(seq.into_iter().map(Ok))))
            }
        }

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ToolsCapturingProvider {
            captured: Arc::clone(&captured),
            responses: StdMutex::new(vec![
                vec![
                    ProviderEvent::ToolCallDelta {
                        item_id: "structured-out".to_string(),
                        call_id: None,
                        name: Some("structured_output".to_string()),
                        arguments_delta: json!({"response": "done", "requirements": {}})
                            .to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    done_event_tool_use(),
                ],
                vec![ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                }],
            ]),
        });

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(crate::tools::agent::SpawnAgentTool::new()));
        tool_registry.register(Box::new(ForkTool::new()));
        tool_registry.register(Box::new(crate::tools::read::ReadTool::new()));
        let agent_registry = AgentRegistry::shared();
        let (ctx, _parent_store) = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            Arc::new(tool_registry),
            Arc::new(MessageRouter::new()),
        );

        let tool = ForkTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"request": "r", "model": "gpt-5.5", "requirements": []})),
                &ctx,
            )
            .await
            .expect("fork");
        let fork_id = Uuid::parse_str(out.content["agent_id"].as_str().unwrap()).unwrap();
        let handle = ctx
            .get_extension::<AgentHandles>()
            .unwrap()
            .remove(fork_id)
            .expect("handle");
        handle.join_handle.await.expect("join");

        let names: Vec<String> = captured
            .lock()
            .unwrap()
            .iter()
            .map(|def| match def {
                crate::provider::tools::ProviderToolDefinition::Function(function) => {
                    function.name.clone()
                }
                other => format!("{other:?}"),
            })
            .collect();
        assert!(
            !names.iter().any(|n| n == "spawn_agent") && !names.iter().any(|n| n == "fork"),
            "a leaf fork must not SEE delegation tools: {names:?}",
        );
        assert!(
            names.iter().any(|n| n == "read"),
            "non-delegation tools survive: {names:?}",
        );
    }
}
