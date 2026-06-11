//! [`Agent`] — a fully-assembled, single-use agent produced by
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Execution entry points: [`Agent::run`] (builder-set prompt),
//! [`Agent::run_with`] (explicit prompt), and [`Agent::run_stream`]
//! (streaming [`AgentEvent`]s alongside the run future).

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::assembly::snapshot_store;
use crate::agent::output::AgentOutput;
use crate::error::{ConfigError, NornError};
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::inbound::InboundChannel;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::{AgentEvent, AgentEventSender};
use crate::session::store::EventStore;
use crate::tool::registry::ToolRegistry;

/// A fully-assembled, single-use agent. Not [`Clone`]: it owns the session
/// event store and the runtime tool context.
pub struct Agent {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) registry: Arc<ToolRegistry>,
    pub(super) loop_context: LoopContext,
    pub(super) config: AgentLoopConfig,
    pub(super) model: String,
    pub(super) output_schema: Option<Value>,
    pub(super) tool_defs: Vec<ToolDefinition>,
    pub(super) event_store: Arc<EventStore>,
    pub(super) event_sender: Option<AgentEventSender>,
    pub(super) cancel: Option<CancellationToken>,
    pub(super) inbound: Option<InboundChannel>,
    pub(super) id: Uuid,
    pub(super) prompt: Option<String>,
}

impl Agent {
    /// Run with the prompt configured on the builder.
    ///
    /// # Errors
    ///
    /// [`NornError::Config`] when no prompt was set; otherwise any execution
    /// error from the agent loop.
    pub async fn run(self) -> Result<AgentOutput, NornError> {
        let prompt = self.prompt.clone().ok_or_else(|| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: "no prompt set; call .prompt(..) or use run_with(prompt)".to_string(),
            })
        })?;
        self.run_with(prompt).await
    }

    /// Run with an explicit prompt, consuming the agent and returning the
    /// [`AgentOutput`] (final value, usage, event store, stop reason).
    ///
    /// # Errors
    ///
    /// Any execution error from the agent loop (provider failure, event-store
    /// failure, a blocking hook, or an unrecoverable tool error).
    pub async fn run_with(mut self, prompt: impl Into<String>) -> Result<AgentOutput, NornError> {
        let prompt = prompt.into();
        let result = run_agent_step(AgentStepRequest {
            provider: self.provider.as_ref(),
            executor: self.registry.as_ref(),
            store: self.event_store.as_ref(),
            user_prompt: &prompt,
            tools: &self.tool_defs,
            output_schema: self.output_schema.as_ref(),
            model: &self.model,
            config: &self.config,
            event_tx: self.event_sender.as_ref(),
            inbound: self.inbound.as_mut(),
            loop_context: &mut self.loop_context,
            cancel: self.cancel.clone(),
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
        Ok(AgentOutput::from_step_result(result, Some(store)))
    }

    /// Run with the builder-set prompt while streaming [`AgentEvent`]s.
    ///
    /// Installs a fresh broadcast channel of `channel_capacity` as the event
    /// sink (replacing any
    /// [`AgentBuilder::event_sender`](crate::agent::builder::AgentBuilder::event_sender))
    /// and returns the receiver alongside the run future. Await the future
    /// while draining the receiver concurrently (e.g. `tokio::join!` or a
    /// spawned reader).
    ///
    /// `channel_capacity` is explicit rather than defaulted: the right buffer
    /// depends on how fast the consumer drains relative to the model's output
    /// rate.
    pub fn run_stream(
        mut self,
        channel_capacity: usize,
    ) -> (
        broadcast::Receiver<AgentEvent>,
        impl std::future::Future<Output = Result<AgentOutput, NornError>>,
    ) {
        let (tx, rx) = broadcast::channel(channel_capacity);
        self.event_sender = Some(AgentEventSender::new(tx, self.id, "root".to_string()));
        (rx, async move { self.run().await })
    }

    /// The agent's id.
    #[must_use]
    pub fn agent_id(&self) -> Uuid {
        self.id
    }
}
