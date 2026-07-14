//! [`Agent`] — a fully-assembled, single-use agent produced by
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! There is exactly one way to execute an agent: [`Agent::run`] with an
//! explicit, non-empty prompt. Streaming, cancellation, steering, and
//! introspection all live on the [`AgentHandle`] obtained from
//! [`Agent::handle`] before the run starts.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::handle::{AgentHandle, ResolvedAgentInfo};
use crate::agent::output::RunOutcome;
use crate::agent_loop::config::{AgentLoopConfig, ToolExecutor};
use crate::agent_loop::inbound::{InboundChannel, InboundSender};
use crate::agent_loop::loop_context::LoopContext;
use crate::agent_loop::runner::{AgentStepRequest, run_agent_step};
use crate::error::{ConfigError, NornError};
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::{AgentEvent, AgentEventSender};
use crate::session::SessionIndexEntry;
use crate::session::manager::ReplaySummary;
use crate::session::store::EventStore;
use crate::tool::ToolGenerationStore;
use crate::tool::registry::ToolRegistry;

/// A fully-assembled, single-use agent. Not [`Clone`]: it owns the session
/// event store and the runtime tool context. The cloneable control
/// surface is [`AgentHandle`], via [`Agent::handle`].
pub struct Agent {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) registry: Arc<ToolRegistry>,
    pub(super) tool_runtime: Arc<ToolGenerationStore>,
    pub(super) loop_context: LoopContext,
    pub(super) config: AgentLoopConfig,
    pub(super) model: String,
    pub(super) tool_defs: Vec<ToolDefinition>,
    pub(super) event_store: Arc<EventStore>,
    pub(super) event_sender: Option<AgentEventSender>,
    pub(super) events_tx: Option<tokio::sync::broadcast::Sender<AgentEvent>>,
    pub(super) cancel: CancellationToken,
    pub(super) inbound: Option<InboundChannel>,
    pub(super) inbound_tx: Option<InboundSender>,
    pub(super) id: Uuid,
    pub(super) info: Arc<ResolvedAgentInfo>,
    pub(super) session_entry: Option<SessionIndexEntry>,
    pub(super) replay: Option<ReplaySummary>,
}

/// Every assembled field of a built [`Agent`], handed to custom drivers
/// (the TUI's multi-turn REPL, the print step-loop) that run the
/// agent-step loop themselves instead of calling [`Agent::run`].
///
/// Produced by [`Agent::into_parts`], which consumes the agent. The
/// `event_store` is the same [`Arc`] the loop persists into, and
/// `registry` is the same [`Arc`] spawn/fork children dispatch through —
/// dropping either is what lets the store be handed back owned at the end
/// of a driver-run (matching [`Agent::run`]'s own reclamation contract).
pub struct AgentParts {
    /// The provider the agent step calls.
    pub provider: Arc<dyn Provider>,
    /// The gated tool registry, published with the assembled tool
    /// context; the same `Arc` spawn/fork children dispatch through.
    pub registry: Arc<ToolRegistry>,
    /// Atomically published tool generations used for provider requests and
    /// dispatch. Each request leases one immutable generation through its
    /// complete response/tool cycle.
    pub tool_runtime: Arc<ToolGenerationStore>,
    /// The fully-populated loop context (system sections, retry policy,
    /// variables, action log, coordination receivers).
    pub loop_context: LoopContext,
    /// The effective agent-loop config the step executes under.
    pub config: AgentLoopConfig,
    /// The resolved model identifier.
    pub model: String,
    /// The provider-facing tool definitions.
    pub tool_defs: Vec<ToolDefinition>,
    /// The session event store the loop persists into.
    pub event_store: Arc<EventStore>,
    /// The root event sender, present iff
    /// [`AgentBuilder::event_channel_capacity`](crate::agent::builder::AgentBuilder::event_channel_capacity)
    /// was set.
    pub event_sender: Option<AgentEventSender>,
    /// The raw broadcast channel, present iff
    /// [`AgentBuilder::event_channel_capacity`](crate::agent::builder::AgentBuilder::event_channel_capacity)
    /// was set; `subscribe()` yields receivers for driver-owned streams.
    pub events_tx: Option<tokio::sync::broadcast::Sender<AgentEvent>>,
    /// The agent's run-cancellation token — the same trigger the
    /// published `AgentCancellation` cascade and the handle observe.
    pub cancel: CancellationToken,
    /// The inbound steering receiver, present iff
    /// [`AgentBuilder::inbound_capacity`](crate::agent::builder::AgentBuilder::inbound_capacity)
    /// was set.
    pub inbound: Option<InboundChannel>,
    /// The inbound steering sender, present iff
    /// [`AgentBuilder::inbound_capacity`](crate::agent::builder::AgentBuilder::inbound_capacity)
    /// was set.
    pub inbound_tx: Option<InboundSender>,
    /// The agent's id.
    pub id: Uuid,
    /// The resolved-configuration snapshot (model, profile, tools,
    /// session id, working dir, output schema).
    pub info: Arc<ResolvedAgentInfo>,
    /// The persisted session's index entry, when a managed session was
    /// opened via
    /// [`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session).
    pub session_entry: Option<SessionIndexEntry>,
    /// What was recovered from disk while opening the persisted session.
    pub replay: Option<ReplaySummary>,
}

impl AgentParts {
    /// Fire every registered
    /// [`SessionLifecycleHook::on_session_start`](crate::integration::hooks::SessionLifecycleHook::on_session_start)
    /// with the resolved session id (D1). A custom driver that runs the
    /// step loop itself — instead of [`Agent::run`], which fires these
    /// itself — calls this once after assembly, before its first step. A
    /// no-op when no hook registry is wired.
    pub async fn fire_session_start(&self) {
        if let Some(hooks) = self.loop_context.hooks.as_ref() {
            hooks.run_session_start(&self.info.session_id).await;
        }
    }

    /// Fire every registered
    /// [`SessionLifecycleHook::on_session_end`](crate::integration::hooks::SessionLifecycleHook::on_session_end)
    /// with the resolved session id (D1) — the counterpart to
    /// [`Self::fire_session_start`], called on a driver's normal-exit
    /// teardown path. A no-op when no hook registry is wired.
    pub async fn fire_session_end(&self) {
        if let Some(hooks) = self.loop_context.hooks.as_ref() {
            hooks.run_session_end(&self.info.session_id).await;
        }
    }
}

impl Agent {
    /// Decompose the agent into its assembled fields for a custom driver
    /// that runs the agent-step loop itself (the TUI's multi-turn REPL,
    /// the print step-loop) rather than calling [`Agent::run`]. Consumes
    /// the agent; mutually exclusive with [`Agent::run`].
    ///
    /// The returned `event_store` is the same [`Arc`] the loop persists
    /// into and `registry` is the same [`Arc`] spawn/fork children
    /// dispatch through, so the driver owns the identical wiring
    /// [`Agent::run`] would have executed against.
    #[must_use]
    pub fn into_parts(self) -> AgentParts {
        AgentParts {
            provider: self.provider,
            registry: self.registry,
            tool_runtime: self.tool_runtime,
            loop_context: self.loop_context,
            config: self.config,
            model: self.model,
            tool_defs: self.tool_defs,
            event_store: self.event_store,
            event_sender: self.event_sender,
            events_tx: self.events_tx,
            cancel: self.cancel,
            inbound: self.inbound,
            inbound_tx: self.inbound_tx,
            id: self.id,
            info: self.info,
            session_entry: self.session_entry,
            replay: self.replay,
        }
    }

    /// The cloneable control surface for this agent: event subscription,
    /// cancellation, steering, and introspection. Take it *before*
    /// calling [`Agent::run`] (running consumes the agent); the handle
    /// and all its clones stay valid for the whole run.
    #[must_use]
    pub fn handle(&self) -> AgentHandle {
        AgentHandle {
            info: Arc::clone(&self.info),
            cancel: self.cancel.clone(),
            events: self.events_tx.clone(),
            inbound: self.inbound_tx.clone(),
        }
    }

    /// The resolved-configuration snapshot: model, profile, tools,
    /// session id, working directory, output schema.
    #[must_use]
    pub fn info(&self) -> &ResolvedAgentInfo {
        &self.info
    }

    /// The agent's id.
    #[must_use]
    pub fn agent_id(&self) -> Uuid {
        self.id
    }

    /// The *effective* agent-loop config the run will execute under —
    /// the runtime base's config (when loaded) with explicit builder
    /// overrides applied, including the `open_session` cache key and the
    /// output schema. Serializable
    /// ([`AgentLoopConfig`] derives serde), so embedders can persist the
    /// exact configuration a run executed with.
    #[must_use]
    pub fn loop_config(&self) -> &AgentLoopConfig {
        &self.config
    }

    /// The persisted session's index entry, when the builder opened one
    /// via [`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session).
    #[must_use]
    pub fn session_entry(&self) -> Option<&SessionIndexEntry> {
        self.session_entry.as_ref()
    }

    /// What was recovered from disk while opening the persisted session
    /// ([`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session)
    /// builds only). A non-zero
    /// [`ReplaySummary::skipped_lines`] means the replayed history is
    /// incomplete; the builder already logs it at warn level.
    #[must_use]
    pub fn session_replay(&self) -> Option<ReplaySummary> {
        self.replay
    }

    /// Run the agent with an explicit prompt, consuming it and returning
    /// the [`RunOutcome`] — [`RunOutcome::Completed`] with the final
    /// value, usage, and event store, or [`RunOutcome::Stopped`] with the
    /// typed [`AgentStopReason`](crate::agent::AgentStopReason) and
    /// whatever partial output the run produced.
    ///
    /// # Errors
    ///
    /// [`NornError::Config`] when `prompt` is empty or whitespace-only —
    /// an empty prompt has no defined model-facing meaning, so it is
    /// rejected at this boundary instead of producing undefined provider
    /// behaviour. Otherwise any execution error from the agent loop
    /// (provider failure, event-store failure, a blocking hook, or an
    /// unrecoverable tool error). Early stops (timeout, cancellation,
    /// truncation, exhausted budgets) are **not** errors: they return
    /// `Ok(RunOutcome::Stopped { .. })`.
    pub async fn run(mut self, prompt: impl Into<String>) -> Result<RunOutcome, NornError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "empty prompt: Agent::run requires a non-empty user prompt — \
                         pass the user's message (or task description) to run(..)"
                    .to_string(),
            }));
        }
        // Session-lifecycle hooks (D1): `Agent::run` fires them itself with
        // the resolved `info.session_id`, so every embedded / library caller
        // (including Meridian) gets them without hand-firing. The registry is
        // cloned out before the step borrows `loop_context` mutably; the end
        // hook fires only on the normal-exit path below (errors short-circuit
        // via `?` and skip it, matching the driver contract).
        let session_hooks = self.loop_context.hooks.clone();
        let session_id = self.info.session_id.clone();
        if let Some(hooks) = session_hooks.as_ref() {
            hooks.run_session_start(&session_id).await;
        }
        // Passed as `&Arc<dyn ToolExecutor>` (not `&*registry`) so the
        // loop's concurrent batch steps get an owned handle
        // (`ToolExecutor::owned_handle`) and can spawn each batch member
        // on its own task for true parallelism.
        let executor: Arc<dyn ToolExecutor> =
            Arc::clone(&self.tool_runtime) as Arc<dyn ToolExecutor>;
        let result = run_agent_step(AgentStepRequest {
            provider: self.provider.as_ref(),
            executor: &executor,
            store: self.event_store.as_ref(),
            user_prompt: &prompt,
            tools: &self.tool_defs,
            output_schema: self.config.output_schema.as_ref(),
            model: &self.model,
            config: &self.config,
            event_tx: self.event_sender.as_ref(),
            inbound: self.inbound.as_mut(),
            loop_context: &mut self.loop_context,
            cancel: Some(self.cancel.clone()),
        })
        .await?;

        // Hand back the LIVE store `Arc` — the same sink-equipped store the
        // run persisted into. The previous owned-store return de-arced via
        // `Arc::try_unwrap` with a sink-less content-snapshot fallback
        // whenever fork/spawn infra held a reference (an Arc cycle through
        // `AgentToolInfra`), so embedder appends after `run` silently
        // stopped persisting (session-fidelity inventory Gap 14). Sharing
        // the `Arc` closes that: anything appended to the returned store
        // still writes through to disk, cycle or no cycle.
        drop(executor);
        if let Some(hooks) = session_hooks.as_ref() {
            hooks.run_session_end(&session_id).await;
        }
        Ok(RunOutcome::from_step_result(result, Some(self.event_store)))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    /// Explicit window for test fixtures: "test-model" is deliberately
    /// uncatalogued, and `build` now hard-errors on an unarmed window
    /// (2026-07-05 incident guard). `272_000` is gpt-5.5's catalogued
    /// standard window (assets/models.json) — factual, not invented.
    const TEST_CONTEXT_WINDOW: u64 = 272_000;
    use super::*;
    use crate::agent::builder::AgentBuilder;
    use crate::provider::mock::MockProvider;

    fn mock_provider() -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(Vec::new()))
    }

    /// `into_parts` moves the agent's assembled fields verbatim: the
    /// returned `event_store` is the same `Arc` the built agent held (the
    /// one the loop persists into), the id is the builder-supplied id, the
    /// broadcast sender is present exactly when the event channel was
    /// configured, and the session id is always resolved.
    #[test]
    fn into_parts_returns_same_arcs() {
        let id = Uuid::new_v4();
        let agent = AgentBuilder::new(mock_provider())
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_id(id)
            .event_channel_capacity(16)
            .build()
            .expect("build succeeds");
        let store_before = Arc::clone(&agent.event_store);
        let registry_before = Arc::clone(&agent.registry);

        let parts = agent.into_parts();

        assert!(
            Arc::ptr_eq(&store_before, &parts.event_store),
            "into_parts hands back the same event store Arc the agent persisted into",
        );
        assert!(
            Arc::ptr_eq(&registry_before, &parts.registry),
            "into_parts hands back the same registry Arc children dispatch through",
        );
        assert_eq!(parts.id, id, "the builder-supplied id is preserved");
        assert!(
            parts.events_tx.is_some(),
            "events_tx is present because event_channel_capacity was set",
        );
        assert!(
            parts.event_sender.is_some(),
            "the root event sender is present alongside the broadcast channel",
        );
        assert!(
            !parts.info.session_id.is_empty(),
            "a session id is always resolved",
        );
    }

    /// D1: `Agent::run` fires the session-lifecycle hooks itself (start
    /// before the step, end on the normal-exit path), so every embedded /
    /// library caller gets them without hand-firing. Regression for §5.3
    /// (hooks never fired on the library path).
    #[tokio::test]
    async fn run_fires_session_lifecycle_hooks() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::integration::hooks::{Hook, HookRegistry, SessionLifecycleHook};
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::usage::Usage;

        struct Recorder {
            starts: Arc<AtomicUsize>,
            ends: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl SessionLifecycleHook for Recorder {
            async fn on_session_start(&self, session_id: &str) {
                assert!(!session_id.is_empty(), "session id must be resolved");
                self.starts.fetch_add(1, Ordering::SeqCst);
            }
            async fn on_session_end(&self, session_id: &str) {
                assert!(!session_id.is_empty(), "session id must be resolved");
                self.ends.fetch_add(1, Ordering::SeqCst);
            }
        }

        let starts = Arc::new(AtomicUsize::new(0));
        let ends = Arc::new(AtomicUsize::new(0));
        let mut registry = HookRegistry::new();
        registry.register(Hook::SessionLifecycle(Box::new(Recorder {
            starts: Arc::clone(&starts),
            ends: Arc::clone(&ends),
        })));

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "ok".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]]));

        let agent = AgentBuilder::new(provider)
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .hooks(Arc::new(registry))
            .build()
            .expect("build succeeds");
        let outcome = agent.run("go").await.expect("run succeeds");
        assert!(outcome.is_completed(), "text completion completes");

        assert_eq!(starts.load(Ordering::SeqCst), 1, "start hook fires once");
        assert_eq!(ends.load(Ordering::SeqCst), 1, "end hook fires once");
    }

    /// The broadcast sender is absent when the event channel was never
    /// configured — no silent dead channel.
    #[test]
    fn into_parts_has_no_event_channel_when_unconfigured() {
        let agent = AgentBuilder::new(mock_provider())
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        let parts = agent.into_parts();
        assert!(parts.events_tx.is_none());
        assert!(parts.event_sender.is_none());
        assert!(parts.inbound.is_none());
    }
}
