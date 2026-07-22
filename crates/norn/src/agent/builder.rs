//! [`AgentBuilder`] — fluent API for in-process agent execution.
//!
//! The builder composes every Norn runtime internal (tool registry, event
//! store, loop context, agent-loop config, provider, profile resolution,
//! system prompt, hooks, rules, diagnostics, fork/spawn infra) from simple
//! inputs. [`AgentBuilder::build`] yields an [`Agent`] whose
//! [`Agent::handle`] is the cloneable control surface (events, cancel,
//! steering, introspection) and whose [`Agent::run`] is the single way to
//! execute. This is the public library API that workflow steps, tests, and
//! embedding consumers call.
//!
//! Simple callers set three or four fields:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use norn::agent::builder::AgentBuilder;
//! # use norn::agent::RunOutcome;
//! # use norn::provider::traits::Provider;
//! # async fn demo(provider: Arc<dyn Provider>) -> Result<(), norn::error::NornError> {
//! let outcome = AgentBuilder::new(provider)
//!     .profile_name("dev")
//!     .working_dir("/repo")
//!     .run("Fix the failing tests")
//!     .await?;
//! match outcome {
//!     RunOutcome::Completed(output) => println!("{:?}", output.text()),
//!     RunOutcome::Stopped { reason, partial } => {
//!         eprintln!("run stopped early ({reason:?}): {:?}", partial.text());
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Advanced callers layer retry policy, hooks, rules, diagnostics, a
//! persisted session ([`AgentBuilder::open_session`] or
//! [`AgentBuilder::session`]), an event broadcast channel
//! ([`AgentBuilder::event_channel_capacity`]), an inbound steering channel
//! ([`AgentBuilder::inbound_capacity`]), a cancellation token, and a
//! fork/spawn agent registry onto the same builder — same type, same code
//! path.

mod build;
mod init;
mod prompt;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::assembly::{AgentConfigPresence, ExtensionInstaller};
use crate::agent::child_policy::ChildPolicy;
use crate::agent::mcp::McpAttachment;
use crate::agent::output::RunOutcome;
use crate::agent::registry::AgentRegistry;
use crate::agent::session_spec::SessionRequest;
use crate::agent_loop::config::AgentLoopConfig;
use crate::agent_loop::event_schemas::EventSchemaSet;
use crate::agent_loop::inbound::{InboundChannel, InboundSender};
use crate::agent_loop::retry::RetryPolicy;
use crate::error::NornError;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::integration::variables::VariableStore;
use crate::profile::{Capability, Profile, ProfileOrigin};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::provider::traits::Provider;
use crate::rules::engine::RuleEngine;
use crate::session::store::EventStore;
use crate::system_prompt::builder::ExecutionMode;
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::traits::Tool;
use crate::tools::diagnostics::DiagnosticInfra;
use crate::tools::lsp::{LspBackend, LspWorkspace};

/// Fluent builder for an in-process [`crate::agent::instance::Agent`].
///
/// Construct with [`AgentBuilder::new`] (provider is the only required
/// input), chain fluent setters, then call [`AgentBuilder::build`] to obtain
/// an agent, or call [`AgentBuilder::run`] to build and execute in one step.
pub struct AgentBuilder {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) profile: Option<Profile>,
    pub(super) profile_origin: Option<ProfileOrigin>,
    pub(super) profile_name: Option<String>,
    pub(super) model: Option<String>,
    pub(super) system_prompt: Option<String>,
    pub(super) append_system_prompt: Option<String>,
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    pub(super) service_tier: Option<ServiceTier>,
    pub(super) capabilities: Vec<Capability>,
    pub(super) working_dir: Option<PathBuf>,
    pub(super) workspace_root: Option<PathBuf>,
    pub(super) bash_drain_grace: Option<Duration>,
    pub(super) allowed_tools: Option<Vec<String>>,
    pub(super) extra_tools: Vec<Box<dyn Tool + Send + Sync>>,
    pub(super) without_tools: Vec<String>,
    pub(super) lsp_backend: Option<Arc<dyn LspBackend>>,
    pub(super) lsp_workspace: Option<Arc<LspWorkspace>>,
    pub(super) execution_mode: ExecutionMode,
    pub(super) agent_config: AgentLoopConfig,
    pub(super) agent_config_present: AgentConfigPresence,
    pub(super) retry_policy: Option<RetryPolicy>,
    pub(super) session: Option<Arc<EventStore>>,
    pub(super) session_request: Option<SessionRequest>,
    pub(super) event_channel_capacity: Option<usize>,
    pub(super) cancel: Option<CancellationToken>,
    pub(super) inbound_capacity: Option<usize>,
    pub(super) inbound: Option<InboundChannel>,
    pub(super) inbound_tx: Option<InboundSender>,
    pub(super) agent_id: Option<Uuid>,
    pub(super) hooks: Option<Arc<HookRegistry>>,
    pub(super) rules: Option<RuleEngine>,
    pub(super) diagnostics: Option<Arc<DiagnosticCollector>>,
    pub(super) diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    pub(super) additional_post_checks: Vec<Box<dyn RuntimePostValidateCheck>>,
    pub(super) agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
    pub(super) child_policy: Option<ChildPolicy>,
    pub(super) child_result_capacity: Option<usize>,
    pub(super) extensions: Vec<ExtensionInstaller>,
    pub(super) load_runtime_base: bool,
    pub(super) task_group_slug: Option<String>,
    pub(super) event_schemas: Option<EventSchemaSet>,
    pub(super) variables: Option<Arc<VariableStore>>,
    pub(super) variable_pairs: Vec<(String, String)>,
    pub(super) disallowed_tools: Vec<String>,
    pub(super) mcp: McpAttachment,
    pub(super) terminal_reclamation: bool,
    pub(super) register_root: Option<(String, String)>,
}

impl AgentBuilder {
    /// Build and run with an explicit prompt. Shorthand for
    /// `self.build()?.run(prompt).await`.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::build`] errors and any execution error,
    /// including the typed rejection of an empty prompt.
    pub async fn run(self, prompt: impl Into<String>) -> Result<RunOutcome, NornError> {
        self.build()?.run(prompt).await
    }
}

#[cfg(test)]
use crate::agent::instance::Agent;
#[cfg(test)]
use crate::agent_loop::config::ToolExecutor;
#[cfg(test)]
use crate::error::ConfigError;
#[cfg(test)]
use crate::provider::SharedAgentEventChannel;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests;
