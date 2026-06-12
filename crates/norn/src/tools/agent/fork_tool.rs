//! `ForkTool` (NA-010) — async fork that mirrors `SpawnAgentTool`'s
//! `tokio::spawn` lifecycle.
//!
//! Fork is semantically distinct from spawn: **fork = same identity, different
//! model**, **spawn = fresh identity, configured through profile**. The
//! machinery is shared (per-child [`ToolContext`](crate::tool::context::ToolContext), status watch channel,
//! inbound steering channel, child result channel) so coordination
//! tools — `signal_agent`, `close_agent` — work uniformly across
//! both surfaces.
//!
//! The tool's `execute()` reserves a registry slot, builds the child's seed
//! event store (inheriting the parent's audit trail with a synthetic
//! tool-result for the fork call itself), branches the
//! [`SessionTree`](crate::session::tree::SessionTree) when one is published,
//! composes the child's [`LoopContext`](crate::r#loop::loop_context::LoopContext) (fork preamble + parent base system
//! instruction), filters the parent registry's tool definitions through the
//! per-fork allow-list, launches the child via [`tokio::spawn`], and returns
//! immediately with `{ agent_id, path, status: "active" }` — the same child-id
//! field name `spawn_agent` uses. On terminal status the
//! launcher marks the registry, appends a
//! [`SessionEvent::ForkComplete`](crate::session::events::SessionEvent::ForkComplete)
//! to the parent's timeline, and sends the formatted result through the
//! [`ChildResultSender`](crate::agent::result_channel::ChildResultSender).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::fork_pipeline::{
    FORK_INBOUND_BUFFER, ForkLaunch, build_fork_context, launch_fork, resolve_fork_store,
};
use super::fork_seed::seed_fork_events;
use super::handle::AgentHandles;
use super::infra::{SubAgentExecutor, infra_from};
use super::lifecycle::LifecycleEmitter;
use super::reclaim::{ReclaimHandshake, ReclaimOnResultDelivery};
use crate::agent::fork::{
    ForkRequirement, ParentSystemInstruction, build_fork_output_schema, combine_system_instruction,
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

/// Valid model identifiers for the fork tool.
const FORK_ALLOWED_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
];

#[derive(Debug, Deserialize)]
struct ForkArgs {
    request: String,
    model: String,
    requirements: Vec<ForkRequirement>,
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
                    "enum": FORK_ALLOWED_MODELS,
                    "description": "Model for the forked agent."
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

        if !FORK_ALLOWED_MODELS.contains(&args.model.as_str()) {
            return Err(ToolError::ExecutionFailed {
                reason: format!(
                    "fork model '{}' is not available; must be one of: {}",
                    args.model,
                    FORK_ALLOWED_MODELS.join(", "),
                ),
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

        // R3: hierarchical fork path nests under the parent's registry path.
        let parent_prefix = infra
            .registry
            .read()
            .get(infra.agent_id)
            .map(|entry| entry.path)
            .unwrap_or_default();
        let path = format!("{parent_prefix}/fork/{}", Uuid::new_v4());

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
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork reservation failed: {e}"),
        })?;
        let fork_id = guard.id();

        let ((child_store, child_tree, forked_session_id), tree_seeded) =
            resolve_fork_store(ctx, &args.model).map_err(|e| ToolError::ExecutionFailed {
                reason: format!("fork: {e}"),
            })?;

        let parent_events = infra.event_store.events();
        let fork_call_id = if envelope.tool_call_id.is_empty() {
            None
        } else {
            Some(envelope.tool_call_id.as_str())
        };
        seed_fork_events(
            child_store.as_ref(),
            &parent_events,
            fork_call_id,
            fork_id,
            tree_seeded,
        )
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork: seeding child store failed: {e}"),
        })?;

        // All fallible setup is done — confirm the reservation. From here
        // the launch is unconditional and the completion wrapper owns the
        // entry's terminal transition.
        guard.confirm().map_err(|e| ToolError::ExecutionFailed {
            reason: format!("fork confirm failed: {e}"),
        })?;

        // R3: per-child ToolContext with fresh identity, forwarded shared
        // infrastructure.
        let child_ctx =
            build_fork_context(&infra, fork_id, Arc::clone(&child_store), ctx, child_tree);

        // R5: combined system instruction = fork preamble + parent base.
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
            combine_system_instruction(&parent_base),
            child_ctx.shared_working_dir(),
        );
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
        loop_ctx.environment = Some(crate::system_prompt::environment::EnvironmentConfig {
            session_id: None,
            model: args.model.clone(),
        });

        let output_schema = build_fork_output_schema(&args.requirements);

        let tool_defs =
            crate::provider::surface::collect_function_definitions(parent_registry, None);
        let executor =
            SubAgentExecutor::new(Arc::clone(parent_registry), None, Arc::clone(&child_ctx));

        // R6: inbound steering channel — the parent keeps the sender via the
        // AgentHandle for `Steer` / `FollowUp` injection at tool boundaries.
        let (inbound_tx, inbound_rx) = inbound_channel(FORK_INBOUND_BUFFER);

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
        let child_event_sender = ctx
            .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
            .map(|ch| {
                crate::provider::agent_event::AgentEventSender::new(
                    ch.0.clone(),
                    fork_id,
                    format!("fork/{}", args.model),
                )
            });
        let requirement_names: Vec<String> = args
            .requirements
            .iter()
            .map(|r| crate::agent::fork::slugify_requirement_name(&r.name))
            .collect();

        // NH-006 R5 parity with spawn (C56): fire
        // SubagentHook::on_subagent_start before launching the fork.
        // Observational — Block has no semantics on start (the trait
        // method returns `()`). Absent registry → no hook to fire.
        if let Some(hooks_arc) = hooks.as_ref() {
            hooks_arc
                .run_subagent_start(&fork_id.to_string(), "fork")
                .await;
        }

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
        lifecycle.emit_started();

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

    use chrono::Utc;
    use futures_util::{StreamExt, stream};
    use parking_lot::RwLock;
    use serde_json::json;

    use super::super::handle::SharedSessionTree;
    use super::super::infra::AgentToolInfra;
    use super::*;
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
    use crate::session::tree::{SessionMetadata, SessionStatus, SessionTree};
    use crate::tool::envelope::RuntimeInputs;
    use crate::tool::registry::ToolRegistry;
    use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "fork".to_string(),
            model_args: args,
            runtime_inputs: RuntimeInputs::default(),
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

    fn structured_response_provider(payload: serde_json::Value) -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
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
            provider,
            event_store: Arc::clone(&event_store),
            agent_id: parent_id,
            parent_id: None,
            tool_registry: Some(tool_registry),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
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
                base: EventBase::new(None),
                content: "running batch".to_string(),
                thinking: String::new(),
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
                duration_ms: 1,
            })
            .expect("seed read result");
        parent_store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc-search".to_string(),
                tool_name: "search".to_string(),
                output: serde_json::json!({"hits": []}),
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
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
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

    /// R4 (SessionTree mode): fork branches under the parent's session.
    #[tokio::test]
    async fn fork_branches_session_tree_when_extension_present() {
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

        let children = tree.list_children(root_id);
        assert_eq!(children.len(), 1, "fork must branch under the root");
    }

    /// R5: the helper returns a combined system instruction containing the
    /// fork preamble verbatim *plus* the parent's base, with the preamble
    /// first.
    #[test]
    fn fork_loop_context_inherits_parent_base_with_preamble() {
        let parent_base = "You are the parent. Be terse.";
        let combined = combine_system_instruction(parent_base);
        let loop_ctx = LoopContext::new(combined);
        let base = loop_ctx.base_system_instruction();
        assert!(
            base.contains(FORK_SYSTEM_PREAMBLE),
            "preamble missing: {base}"
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
                name: Some("victim".to_string()),
                arguments_delta: r#"{"command": "rm -rf /"}"#.to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
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
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
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

        // The result channel carries the same per-fork usage.
        let result = rx.try_recv().expect("result on the channel");
        assert_eq!(result.agent_id, fork_id);
        assert_eq!(result.usage.input_tokens, 5);
        assert_eq!(result.usage.output_tokens, 2);
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
            provider: closer_provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            tool_registry: None,
        });
        let closer_ctx = ToolContext::empty();
        closer_ctx.insert_extension(closer_infra);
        let close_out = CloseAgentTool::new()
            .execute(
                &ToolEnvelope {
                    tool_call_id: "close-1".to_string(),
                    tool_name: "close_agent".to_string(),
                    model_args: json!({"agent_id": fork_id.to_string()}),
                    runtime_inputs: RuntimeInputs::default(),
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
            runtime_inputs: RuntimeInputs::default(),
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
                    runtime_inputs: RuntimeInputs::default(),
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
                name: Some("action_log".to_string()),
                arguments_delta: json!({ "query": "list" }).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ];
        let turn2 = vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
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
                    runtime_inputs: RuntimeInputs::default(),
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
}
