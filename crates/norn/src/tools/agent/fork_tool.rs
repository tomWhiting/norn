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
//! composes the child's source-aware [`LoopContext`] (inherited prompt plan +
//! one fresh fork policy), filters the parent registry's tool definitions through the
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

mod args;
mod prompt;
mod runtime;

use self::args::ForkArgs;
use super::delegation::{
    auto_child_path, grant_child_policy, install_child_result_channel, resolve_spawner_policy,
};
use super::fork_context::build_fork_context;
use super::fork_launch::{ForkLaunch, launch_fork};
use super::fork_seed::{seed_fork_events, truncate_seed_at_anchor};
use super::handle::AgentHandles;
use super::infra::{AgentCancellation, infra_from};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use crate::agent::child_policy::{ChildLoopConfig, CoordinationEnvelope};
use crate::agent::fork::build_fork_output_schema;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::ChildResultSender;
use crate::error::ToolError;
use crate::r#loop::inbound::inbound_channel;
use crate::provider::agent_event::{SubagentDescriptor, SubagentKind};
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
        args::input_schema()
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

        let runtime::ForkRuntime {
            loop_context: loop_ctx,
            requirement_names,
            user_task,
            allow_list,
            hooks,
        } = runtime::assemble(runtime::ForkRuntimeInputs {
            parent_context: ctx,
            parent_infra: &infra,
            child_context: &child_ctx,
            fork_id,
            path_address: &branched.path_address,
            fork_policy: &fork_policy,
            child_result_rx: fork_result_rx,
            args: &args,
            parent_registry,
        })?;
        let output_schema = build_fork_output_schema(&args.requirements);

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
                user_task,
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
mod tests;
