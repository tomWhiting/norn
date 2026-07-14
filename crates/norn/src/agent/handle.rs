//! [`AgentHandle`] — the cloneable control surface of a built agent —
//! and [`ResolvedAgentInfo`], the post-build introspection snapshot.
//!
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build)
//! produces an [`Agent`](crate::agent::Agent); calling
//! [`Agent::handle`](crate::agent::Agent::handle) before running yields
//! an [`AgentHandle`] that remains valid for the whole run. The handle
//! bundles everything an embedder previously hand-wired around the
//! builder:
//!
//! - **event subscription** — [`AgentHandle::subscribe`] returns fresh
//!   receivers on the agent's broadcast channel (configured with
//!   [`AgentBuilder::event_channel_capacity`](crate::agent::builder::AgentBuilder::event_channel_capacity));
//! - **cancellation** — [`AgentHandle::cancel`] fires the loop's
//!   cooperative cancellation token;
//! - **steering** — [`AgentHandle::inbound_sender`] sends mid-run
//!   messages drained at tool boundaries (configured with
//!   [`AgentBuilder::inbound_capacity`](crate::agent::builder::AgentBuilder::inbound_capacity));
//! - **introspection** — [`AgentHandle::info`] exposes the resolved
//!   model, profile, tool inventory, session id, working directory, and
//!   output schema.
//!
//! # Driving cancellation from a durable-workflow engine
//!
//! An activity that must honor engine cancellation (e.g. an aion
//! `ActivityContext` with `is_cancelled()` / `cancelled()`) takes the
//! handle before spawning the run and races the engine's cancellation
//! future against the run future:
//!
//! ```no_run
//! # use norn::agent::Agent;
//! # async fn engine_cancelled() { std::future::pending::<()>().await }
//! # async fn demo(agent: Agent) -> Result<(), norn::error::NornError> {
//! let handle = agent.handle();
//! let mut run = std::pin::pin!(agent.run("do the work"));
//! let outcome = tokio::select! {
//!     outcome = &mut run => outcome?,
//!     () = engine_cancelled() => {
//!         // Fire the cooperative token, then let the run wind down: it
//!         // returns promptly with the typed Cancelled outcome.
//!         handle.cancel();
//!         run.await?
//!     }
//! };
//! // A cancelled run returns Ok(RunOutcome::Stopped { reason: Cancelled, .. })
//! // with usage and the session event store intact — record it as a
//! // cancelled activity, never as success.
//! # let _ = outcome;
//! # Ok(())
//! # }
//! ```
//!
//! Cancellation is prompt: the loop observes the token at the next
//! iteration boundary or while awaiting the in-flight provider stream,
//! whichever comes first; a tool already executing finishes before the
//! run returns. Alternatively, link tokens at build time — pass the
//! engine's child token to
//! [`AgentBuilder::cancel_token`](crate::agent::builder::AgentBuilder::cancel_token)
//! and the handle and engine share one token. Timeouts compose the same
//! way: an outer engine timeout calls `handle.cancel()`, while
//! [`AgentLoopConfig::step_timeout`](crate::agent_loop::config::AgentLoopConfig::step_timeout)
//! bounds the run from the inside.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_loop::inbound::InboundSender;
use crate::integration::McpControlHandle;
use crate::provider::AgentEvent;

/// Post-build snapshot of everything the builder resolved: the facts an
/// embedder needs for logging, persistence, and cross-process
/// correlation, unreachable before this type existed (embedders emitted
/// `"model": ""` because the resolved model lived only inside the
/// agent).
///
/// Serializable so the snapshot can be persisted with a durable
/// activity's record or attached to telemetry.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResolvedAgentInfo {
    /// The agent's id (builder-supplied or freshly generated).
    pub agent_id: Uuid,
    /// The model the run will use, after profile resolution and the
    /// builder's `model` override.
    pub model: String,
    /// Name of the resolved profile, when the profile carries one.
    /// `None` for the unnamed default profile.
    pub profile_name: Option<String>,
    /// Names of every tool in the final gated registry — exactly the
    /// tools the model is shown.
    pub tool_names: Vec<String>,
    /// The session id: the persisted session's index-entry id when the
    /// builder opened one via
    /// [`AgentBuilder::open_session`](crate::agent::builder::AgentBuilder::open_session),
    /// otherwise the run-scoped id minted at build time. Matches the
    /// `session_id` the system prompt's environment block and the
    /// `{{session_id}}` variable expose to the model.
    pub session_id: String,
    /// The agent's resolved working directory.
    pub working_dir: PathBuf,
    /// The structured-output JSON schema the loop enforces, when one was
    /// configured. This is the same value as
    /// [`AgentLoopConfig::output_schema`](crate::agent_loop::config::AgentLoopConfig::output_schema)
    /// on the agent's effective config — already a serialized form
    /// (`serde_json::Value`) embedders can carry across process
    /// boundaries.
    pub output_schema: Option<Value>,
}

/// Cloneable control surface for a built agent: event subscription,
/// cancellation, mid-run steering, and resolved-configuration
/// introspection. Obtain via [`Agent::handle`](crate::agent::Agent::handle)
/// *before* running (running consumes the [`Agent`](crate::agent::Agent));
/// every clone stays valid for the whole run and after it ends.
#[derive(Clone)]
pub struct AgentHandle {
    pub(super) info: Arc<ResolvedAgentInfo>,
    pub(super) cancel: CancellationToken,
    pub(super) events: Option<broadcast::Sender<AgentEvent>>,
    pub(super) inbound: Option<InboundSender>,
    pub(super) mcp_control: Option<McpControlHandle>,
}

impl AgentHandle {
    /// The resolved-configuration snapshot for this agent.
    #[must_use]
    pub fn info(&self) -> &ResolvedAgentInfo {
        &self.info
    }

    /// Request cooperative cancellation of the run.
    ///
    /// Idempotent and immediate to call; the loop stops at the next
    /// iteration boundary (or mid-provider-stream), returning
    /// `Ok(RunOutcome::Stopped)` with
    /// [`AgentStopReason::Cancelled`](crate::agent::AgentStopReason::Cancelled)
    /// and the usage accumulated so far. Cancelling before the run
    /// starts makes the run stop on its first iteration check.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The cancellation token the loop honors — for linking with an
    /// embedder's own token tree (`CancellationToken::child_token`) or
    /// awaiting `cancelled()` elsewhere.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Subscribe to the agent's event broadcast.
    ///
    /// Returns a fresh receiver per call (each receives every event from
    /// subscription onward), or `None` when the builder configured no
    /// event channel — set
    /// [`AgentBuilder::event_channel_capacity`](crate::agent::builder::AgentBuilder::event_channel_capacity)
    /// to enable streaming. Slow consumers that fall more than the
    /// configured capacity behind observe
    /// [`broadcast::error::RecvError::Lagged`].
    ///
    /// Lifetime: the handle itself keeps the channel open, so a drain
    /// loop must treat *run completion* (the run future resolving) as its
    /// stop signal rather than waiting for
    /// [`broadcast::error::RecvError::Closed`], which only arrives once
    /// the agent and every handle clone have been dropped.
    #[must_use]
    pub fn subscribe(&self) -> Option<broadcast::Receiver<AgentEvent>> {
        self.events.as_ref().map(broadcast::Sender::subscribe)
    }

    /// Sender for mid-run steering messages, drained by the loop at tool
    /// boundaries.
    ///
    /// Returns `None` when the builder configured no inbound channel —
    /// set [`AgentBuilder::inbound_capacity`](crate::agent::builder::AgentBuilder::inbound_capacity)
    /// to enable steering.
    #[must_use]
    pub fn inbound_sender(&self) -> Option<InboundSender> {
        self.inbound.clone()
    }

    /// Live MCP configuration and connection control for this agent.
    #[must_use]
    pub fn mcp_control(&self) -> Option<McpControlHandle> {
        self.mcp_control.clone()
    }
}

impl std::fmt::Debug for AgentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandle")
            .field("info", &self.info)
            .field("cancelled", &self.cancel.is_cancelled())
            .field("has_event_channel", &self.events.is_some())
            .field("has_inbound", &self.inbound.is_some())
            .field("has_mcp_control", &self.mcp_control.is_some())
            .finish()
    }
}
