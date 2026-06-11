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

use crate::agent::assembly::snapshot_store;
use crate::agent::handle::{AgentHandle, ResolvedAgentInfo};
use crate::agent::output::RunOutcome;
use crate::agent_loop::config::AgentLoopConfig;
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
use crate::tool::registry::ToolRegistry;

/// A fully-assembled, single-use agent. Not [`Clone`]: it owns the session
/// event store and the runtime tool context. The cloneable control
/// surface is [`AgentHandle`], via [`Agent::handle`].
pub struct Agent {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) registry: Arc<ToolRegistry>,
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

impl Agent {
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
        let result = run_agent_step(AgentStepRequest {
            provider: self.provider.as_ref(),
            executor: self.registry.as_ref(),
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

        // Drop the registry first so that, in the no-fork/spawn case, its tool
        // context (and any extension) releases its references and the event
        // store can be handed back owned. When fork/spawn infra is installed
        // the registry participates in an Arc cycle (registry -> context ->
        // infra -> registry) inherited from `AgentToolInfra`, so `try_unwrap`
        // falls back to a content snapshot.
        drop(self.registry);
        // Release the loop's `Arc<ActionLog>` too: it holds the same
        // `Arc<EventStore>`, so leaving it set would keep a second strong
        // reference alive and force the snapshot fallback (losing the
        // persistence sink) even in the no-fork/spawn case.
        self.loop_context.action_log = None;
        let event_store = self.event_store;
        let store = Arc::try_unwrap(event_store).unwrap_or_else(|shared| snapshot_store(&shared));
        Ok(RunOutcome::from_step_result(result, Some(store)))
    }
}
