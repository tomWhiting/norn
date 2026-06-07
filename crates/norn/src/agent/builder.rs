//! [`AgentBuilder`] — fluent API for in-process agent execution.
//!
//! The builder composes every Norn runtime internal (tool registry, event
//! store, loop context, agent-loop config, provider, profile resolution,
//! system prompt, hooks, rules, diagnostics, fork/spawn infra) from simple
//! inputs and exposes [`Agent::run`] / [`Agent::run_with`] /
//! [`Agent::run_stream`] for execution. This is the public library API that
//! workflow steps, tests, and embedding consumers call.
//!
//! Simple callers set three or four fields:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use norn::agent::builder::AgentBuilder;
//! # use norn::provider::traits::Provider;
//! # async fn demo(provider: Arc<dyn Provider>) -> Result<(), norn::error::NornError> {
//! let output = AgentBuilder::new(provider)
//!     .profile_name("dev")
//!     .working_dir("/repo")
//!     .run_with("Fix the failing tests")
//!     .await?;
//! println!("{:?}", output.text());
//! # Ok(())
//! # }
//! ```
//!
//! Advanced callers layer retry policy, hooks, rules, diagnostics, an
//! [`EventStore`] for session resume, a streaming event sink, a cancellation
//! token, and a fork/spawn agent registry onto the same builder — same type,
//! same code path.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::mailbox::Mailbox;
use crate::agent::output::AgentOutput;
use crate::agent::registry::AgentRegistry;
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{Hook, HookRegistry};
use crate::integration::variables::VariableStore;
use crate::internal::extraction::SharedProvider;
use crate::r#loop::config::ToolExecutor;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::r#loop::inbound::InboundChannel;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::RetryPolicy;
use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::profile::{Capability, Profile, default_scan_dirs, from_profile, resolve_profile};
use crate::provider::request::{ReasoningEffort, ToolDefinition};
use crate::provider::traits::Provider;
use crate::provider::{AgentEvent, AgentEventSender};
use crate::rules::engine::RuleEngine;
use crate::session::action_log::ActionLog;
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;
use crate::system_prompt::builder::{
    ExecutionMode, SystemPromptInputs, ToolPromptEntry, build_system_prompt,
};
use crate::system_prompt::environment::EnvironmentConfig;
use crate::tool::context::{SessionId, SharedWorkingDir, ToolContext};
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;
use crate::tool::wrap_schema_with_envelope;
use crate::tools::agent::AgentToolInfra;
use crate::tools::diagnostics::{
    DiagnosticInfra, DiagnosticStopHook, DiagnosticsPostCheck, build_diagnostic_infra,
};
use crate::tools::lsp::{LspBackend, LspWorkspace};
use crate::tools::registry_builder::register_standard_tools;
use crate::tools::tool_search::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};

/// A deferred installer that publishes a typed extension on the agent's
/// shared [`ToolContext`] at build time. Stored by [`AgentBuilder::extension`]
/// and run during [`AgentBuilder::build`].
type ExtensionInstaller = Box<dyn FnOnce(&ToolContext) + Send>;

/// Fluent builder for an in-process [`Agent`].
///
/// Construct with [`AgentBuilder::new`] (provider is the only required
/// input), chain fluent setters, then call [`AgentBuilder::build`] to obtain
/// an [`Agent`], or call [`AgentBuilder::run`] / [`AgentBuilder::run_with`] to
/// build and execute in one step.
pub struct AgentBuilder {
    provider: Arc<dyn Provider>,
    profile: Option<Profile>,
    profile_name: Option<String>,
    model: Option<String>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    capabilities: Vec<Capability>,
    working_dir: Option<PathBuf>,
    allowed_tools: Option<Vec<String>>,
    extra_tools: Vec<Box<dyn Tool + Send + Sync>>,
    without_tools: Vec<String>,
    lsp_backend: Option<Arc<dyn LspBackend>>,
    lsp_workspace: Option<Arc<LspWorkspace>>,
    prompt: Option<String>,
    output_schema: Option<Value>,
    execution_mode: ExecutionMode,
    agent_config: AgentLoopConfig,
    retry_policy: Option<RetryPolicy>,
    session: Option<EventStore>,
    event_sender: Option<AgentEventSender>,
    cancel: Option<CancellationToken>,
    inbound: Option<InboundChannel>,
    agent_id: Option<Uuid>,
    hooks: Option<Arc<HookRegistry>>,
    rules: Option<RuleEngine>,
    diagnostics: Option<Arc<DiagnosticCollector>>,
    diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    additional_post_checks: Vec<Box<dyn RuntimePostValidateCheck>>,
    agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
    extensions: Vec<ExtensionInstaller>,
    load_runtime_base: bool,
    task_group_slug: Option<String>,
}

impl AgentBuilder {
    /// Start a builder for the given provider. Every other field is optional.
    #[must_use]
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            profile: None,
            profile_name: None,
            model: None,
            system_prompt: None,
            append_system_prompt: None,
            reasoning_effort: None,
            capabilities: Vec::new(),
            working_dir: None,
            allowed_tools: None,
            extra_tools: Vec::new(),
            without_tools: Vec::new(),
            lsp_backend: None,
            lsp_workspace: None,
            prompt: None,
            output_schema: None,
            execution_mode: ExecutionMode::Headless,
            agent_config: AgentLoopConfig::default(),
            retry_policy: None,
            session: None,
            event_sender: None,
            cancel: None,
            inbound: None,
            agent_id: None,
            hooks: None,
            rules: None,
            diagnostics: None,
            diagnostic_infra: None,
            additional_post_checks: Vec::new(),
            agent_registry: None,
            extensions: Vec::new(),
            load_runtime_base: false,
            task_group_slug: None,
        }
    }

    /// Load the same settings, NORN.md context, skill catalog, discovered
    /// rules, hook registry, retry policy, and agent-loop config used by the
    /// CLI before applying explicit builder overrides.
    #[must_use]
    pub fn load_runtime_base(mut self) -> Self {
        self.load_runtime_base = true;
        self
    }

    /// Select the task-store group slug used when [`Self::load_runtime_base`]
    /// installs the disk-backed task store.
    #[must_use]
    pub fn task_group_slug(mut self, slug: impl Into<String>) -> Self {
        self.task_group_slug = Some(slug.into());
        self
    }

    /// Use an already-loaded profile (capabilities, model, instructions).
    #[must_use]
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Resolve a profile by bare name at build time, searching
    /// `.norn/profiles` then `.meridian/profiles` then `~/.norn/profiles`
    /// relative to the working directory. Ignored when [`Self::profile`] is
    /// also set.
    #[must_use]
    pub fn profile_name(mut self, name: impl Into<String>) -> Self {
        self.profile_name = Some(name.into());
        self
    }

    /// Override the model the profile selects.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Override the profile's system instructions.
    #[must_use]
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Append additional instructions after the resolved profile instructions
    /// or caller-supplied [`Self::system_prompt`] override.
    #[must_use]
    pub fn append_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.append_system_prompt = Some(prompt.into());
        self
    }

    /// Override the profile's reasoning-effort hint.
    #[must_use]
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Add capability bundles to the resolved profile before tool gating and
    /// prompt construction.
    #[must_use]
    pub fn capabilities(mut self, capabilities: Vec<Capability>) -> Self {
        self.capabilities.extend(capabilities);
        self
    }

    /// Set the agent's working directory. Defaults to the process current
    /// directory when unset. All filesystem tools resolve relative paths
    /// against this, and `bash` `cd` directives update it.
    #[must_use]
    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Restrict the default/profile tool set to the named tools.
    #[must_use]
    pub fn allowed_tools(mut self, names: &[&str]) -> Self {
        self.allowed_tools
            .replace(names.iter().map(|s| (*s).to_string()).collect());
        self
    }

    /// Exclude specific tools from the default all-tools set (e.g. mutation
    /// tools for a read-only scout step). Names match the Norn tool registry
    /// names (`bash`, `write`, `edit`, …).
    #[must_use]
    pub fn without_tools(mut self, names: &[&str]) -> Self {
        self.without_tools
            .extend(names.iter().map(|s| (*s).to_string()));
        self
    }

    /// Add a custom tool alongside the standard set.
    #[must_use]
    pub fn tool(mut self, tool: Box<dyn Tool + Send + Sync>) -> Self {
        self.extra_tools.push(tool);
        self
    }

    /// Wire a live LSP backend for the `lsp` tool. Without one, `lsp` is
    /// registered but every call returns a "configure a backend" error.
    #[must_use]
    pub fn lsp_backend(mut self, backend: Arc<dyn LspBackend>) -> Self {
        self.lsp_backend = Some(backend);
        self
    }

    /// Wire a live LSP workspace for diagnostics post-checks. Without one,
    /// diagnostics still run through server / inline adapters but skip the LSP
    /// fast path.
    #[must_use]
    pub fn lsp_workspace(mut self, workspace: Arc<LspWorkspace>) -> Self {
        self.lsp_workspace = Some(workspace);
        self
    }

    /// Set the prompt used by [`Agent::run`] / [`Agent::run_stream`].
    #[must_use]
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Enforce a structured-output JSON schema on the final response.
    #[must_use]
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Interactive vs headless execution (shapes the system prompt). Defaults
    /// to [`ExecutionMode::Headless`] for library use.
    #[must_use]
    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    /// Replace the whole agent-loop config (schema budget, max iterations,
    /// step timeout, compaction, cache key).
    #[must_use]
    pub fn agent_config(mut self, config: AgentLoopConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Cap total provider round-trips per step.
    #[must_use]
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.agent_config.max_iterations = Some(max);
        self
    }

    /// Set an outer wall-clock cap on the whole step.
    #[must_use]
    pub fn step_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.agent_config.step_timeout = Some(timeout);
        self
    }

    /// Select how the provider carries conversation state between calls.
    #[must_use]
    pub fn conversation_state(mut self, mode: ConversationStateMode) -> Self {
        self.agent_config.conversation_state = mode;
        self
    }

    /// Set the provider-side compaction threshold in rendered tokens.
    #[must_use]
    pub fn server_compaction_threshold_tokens(mut self, tokens: u64) -> Self {
        self.agent_config.server_compaction_threshold_tokens = Some(tokens);
        self
    }

    /// Configure provider retry. Defaults to [`RetryPolicy::default`]
    /// (2 retries, 1s initial backoff, 2x multiplier) when unset.
    #[must_use]
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Resume a session from a prior run's [`EventStore`]. A fresh store is
    /// created when unset.
    #[must_use]
    pub fn session(mut self, store: EventStore) -> Self {
        self.session = Some(store);
        self
    }

    /// Receive [`AgentEvent`]s as they happen during execution. For an
    /// owned receiver, prefer [`Agent::run_stream`].
    #[must_use]
    pub fn event_sender(mut self, sender: AgentEventSender) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// Thread a cancellation token into the loop for cooperative abort.
    #[must_use]
    pub fn cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Wire the receiver half of an [`InboundChannel`] into the root agent
    /// step so mid-session messages (e.g. DMs while the agent is mid-turn)
    /// are drained at every tool boundary just like sub-agent inbound
    /// messages. The matching [`InboundSender`](crate::r#loop::inbound::InboundSender)
    /// is held by the embedding consumer (e.g. the assistant session loop)
    /// so it can `.send(..)` `ChannelMessage`s as user input arrives.
    ///
    /// Without an inbound channel the root step has no mid-session
    /// injection path — only the initial `run_with` prompt enters the
    /// conversation.
    #[must_use]
    pub fn inbound(mut self, inbound: InboundChannel) -> Self {
        self.inbound = Some(inbound);
        self
    }

    /// Set the agent's id (sender identity for messaging, parent id for
    /// spawned children). A fresh id is generated when unset.
    #[must_use]
    pub fn agent_id(mut self, id: Uuid) -> Self {
        self.agent_id = Some(id);
        self
    }

    /// Wire a programmatic hook registry.
    #[must_use]
    pub fn hooks(mut self, hooks: Arc<HookRegistry>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Wire a rules engine (context injection / guardrails).
    #[must_use]
    pub fn rules(mut self, rules: RuleEngine) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Wire a diagnostic collector. Published on the tool context and the
    /// loop context so post-validation checks can record diagnostics.
    #[must_use]
    pub fn diagnostics(mut self, diagnostics: Arc<DiagnosticCollector>) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    /// Wire diagnostic infrastructure and install the diagnostics post-check.
    #[must_use]
    pub fn diagnostic_infra(mut self, infra: Arc<DiagnosticInfra>) -> Self {
        self.diagnostic_infra = Some(infra);
        self
    }

    /// Add a runtime post-validation check to the agent's tool context.
    ///
    /// Checks added here run after the diagnostics post-check installed by
    /// [`Self::diagnostic_infra`], when diagnostic infrastructure is present.
    #[must_use]
    pub fn post_check(mut self, check: Box<dyn RuntimePostValidateCheck>) -> Self {
        self.additional_post_checks.push(check);
        self
    }

    /// Wire the shared agent registry so `fork` / `spawn_agent` /
    /// `signal_agent` / `close_agent` resolve their runtime instead of
    /// erroring with "agent runtime not configured".
    #[must_use]
    pub fn agent_registry(mut self, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Publish a typed extension on the agent's shared [`ToolContext`] at build
    /// time, retrievable inside tools via [`ToolContext::get_extension`].
    ///
    /// Extensions are installed after the standard and extra tools are
    /// registered and after profile gating, so embedding consumers can attach
    /// host-supplied infrastructure (identity, service handles, registries)
    /// that individual tools read at execution time. Inserting two values of
    /// the same type keeps only the last, matching
    /// [`ToolContext::insert_extension`] semantics.
    #[must_use]
    pub fn extension<T>(mut self, value: Arc<T>) -> Self
    where
        T: Any + Send + Sync,
    {
        self.extensions
            .push(Box::new(move |ctx| ctx.insert_extension(value)));
        self
    }

    /// Validate and assemble the [`Agent`].
    ///
    /// # Errors
    ///
    /// - [`NornError::Config`] when the working directory cannot be
    ///   determined, the named profile cannot be resolved, or no tool remains
    ///   after exclusions.
    pub fn build(mut self) -> Result<Agent, NornError> {
        let working_dir = match self.working_dir {
            Some(dir) => dir,
            None => std::env::current_dir().map_err(|e| {
                NornError::Config(ConfigError::InvalidConfig {
                    reason: format!("cannot determine working directory: {e}"),
                })
            })?,
        };
        let shared_wd = SharedWorkingDir::new(working_dir.clone());

        let mut profile = match self.profile {
            Some(profile) => profile,
            None => match self.profile_name {
                Some(ref name) => resolve_profile(name, &default_scan_dirs(&working_dir))?,
                None => Profile::default(),
            },
        };
        if let Some(model) = self.model {
            profile.model = model;
        }
        let runtime_base = if self.load_runtime_base {
            let mut profile_for_base = profile.clone();
            let base = crate::runtime_init::load_runtime_base(
                &working_dir,
                &mut profile_for_base,
                self.hooks.clone(),
                self.task_group_slug.as_deref(),
            )?;
            profile = profile_for_base;
            Some(base)
        } else {
            None
        };
        if let Some(reasoning_effort) = self.reasoning_effort {
            profile.reasoning_effort = Some(reasoning_effort);
        }
        if let Some(allowed_tools) = self.allowed_tools {
            profile.tools = Some(allowed_tools);
        }
        if !self.capabilities.is_empty() {
            profile.capabilities.extend(self.capabilities);
        }
        let model = profile.model.clone();
        if model.is_empty() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "no model specified — set .profile() or .model() on the builder"
                    .to_string(),
            }));
        }

        let mut registry = ToolRegistry::new();
        let lsp_backend = self.lsp_backend.clone();
        register_standard_tools(&mut registry, lsp_backend.clone());
        for tool in self.extra_tools {
            registry.register(tool);
        }
        for name in &self.without_tools {
            registry.remove(name);
        }

        let mut runtime_base = runtime_base;
        let runtime_rules = runtime_base.as_mut().and_then(|base| base.rules.take());
        let runtime_hooks = runtime_base.as_mut().and_then(|base| base.hooks.take());
        let diagnostic_infra = if let Some(infra) = self.diagnostic_infra.take() {
            Some(infra)
        } else if runtime_base.is_some() {
            Some(Arc::new(build_diagnostic_infra(
                &working_dir,
                lsp_backend.clone(),
                self.lsp_workspace.as_deref(),
            )))
        } else {
            None
        };
        let rules = self.rules.or(runtime_rules);
        let hook_source = self.hooks.or(runtime_hooks);
        let hooks =
            append_diagnostic_stop_hook(hook_source, diagnostic_infra.as_ref().map(Arc::clone))?;
        let (mut loop_context, mut registry) = from_profile(&profile, registry, rules, hooks);

        if registry.is_empty() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "no tools available after exclusions; an agent needs at least one tool"
                    .to_string(),
            }));
        }

        loop_context.retry_policy = self.retry_policy.unwrap_or_else(|| {
            runtime_base
                .as_ref()
                .map_or_else(RetryPolicy::default, |b| b.retry_policy.clone())
        });
        loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
        loop_context.context_edits = Some(ContextEdits::new());
        loop_context.diagnostics.clone_from(&self.diagnostics);
        loop_context.working_dir = shared_wd.clone();
        let variables =
            Arc::new(VariableStore::with_builtins().with_working_dir(shared_wd.clone()));
        let session_id = variables.session_id().to_owned();
        loop_context.variables = Some(Arc::clone(&variables));
        loop_context.environment = Some(EnvironmentConfig {
            session_id: Some(session_id.clone()),
            model: model.clone(),
        });
        let explicit_agent_config = self.agent_config.clone();
        if let Some(base) = runtime_base.as_ref() {
            loop_context.context_loader = Some(base.context_loader.clone());
            loop_context.base_suffix = base.skill_catalog.system_prompt_listing();
            loop_context
                .iteration_monitor
                .clone_from(&base.iteration_monitor);
            loop_context.diagnostics = Some(Arc::clone(&base.diagnostics));
            self.agent_config = base.agent_config.clone();
        }

        let config_override = if runtime_base.is_some() {
            merge_agent_config(self.agent_config.clone(), explicit_agent_config)
        } else {
            explicit_agent_config
        };
        let has_auto_compact = config_override.auto_compact_threshold_pct.is_some()
            && self.agent_config.context_window_limit.is_some();
        install_system_prompt(
            &mut loop_context,
            &registry,
            self.execution_mode,
            self.output_schema.is_some(),
            self.system_prompt,
            self.append_system_prompt,
            has_auto_compact,
        );

        let tool_defs = collect_tool_definitions(&registry);

        // The event store backs both the loop's `ToolResult` persistence and
        // the action log's Level 2/3 look-ups, so it must be created before the
        // `ToolContext` that publishes the action log and shared between them.
        let event_store = Arc::new(self.session.unwrap_or_default());
        if let Some(edits) = loop_context.context_edits.as_mut() {
            edits.apply_persisted_compactions(&event_store);
        }
        let action_log = Arc::new(ActionLog::new(Arc::clone(&event_store)));

        let mut ctx = ToolContext::with_working_dir(shared_wd);
        ctx.insert_extension(Arc::new(SessionId(session_id)));
        if let Some(diagnostics) = self.diagnostics {
            ctx.insert_extension(diagnostics);
        }
        if let Some(infra) = diagnostic_infra {
            ctx.insert_extension(infra);
            ctx.post_checks.push(Box::new(DiagnosticsPostCheck));
        }
        ctx.post_checks.extend(self.additional_post_checks);
        ctx.insert_extension(Arc::new(SharedProvider(Arc::clone(&self.provider))));
        ctx.insert_extension(Arc::clone(&action_log));
        // Install consumer-supplied extensions before publishing the tool
        // catalog so embedding runtimes can contribute subcommand entries.
        for install in self.extensions {
            install(&ctx);
        }
        registry.set_context(Arc::new(ctx));
        let Some(shared) = registry.shared_context() else {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "tool registry did not publish the assembled tool context".to_string(),
            }));
        };
        if let Some(base) = runtime_base.as_ref() {
            crate::runtime_init::install_runtime_extensions(
                shared.as_ref(),
                &base.shared_task_store,
                &base.diagnostics,
                base.hooks.as_ref(),
            );
            crate::runtime_init::install_skill_infra(
                shared.as_ref(),
                base.skill_paths.clone(),
                Arc::clone(&base.skill_catalog),
            );
            crate::runtime_init::install_context_search_paths(
                shared.as_ref(),
                &base.settings,
                &working_dir,
            );
            crate::runtime_init::install_tool_catalog(&registry);
        } else {
            install_tool_catalog(&registry, shared.as_ref());
        }
        let registry = Arc::new(registry);

        // Share the same `Arc<ActionLog>` with the loop so dispatch recording
        // and the `action_log` tool's queries observe one ledger.
        loop_context.action_log = Some(Arc::clone(&action_log));

        let agent_id = self.agent_id.unwrap_or_else(Uuid::new_v4);

        if let Some(agent_registry) = self.agent_registry {
            install_agent_infra(
                &registry,
                AgentInfraParts {
                    registry: agent_registry,
                    provider: Arc::clone(&self.provider),
                    event_store: Arc::clone(&event_store),
                    id: agent_id,
                },
            );
        }

        Ok(Agent {
            provider: self.provider,
            registry,
            loop_context,
            config: config_override,
            model,
            output_schema: self.output_schema,
            tool_defs,
            event_store,
            event_sender: self.event_sender,
            cancel: self.cancel,
            inbound: self.inbound,
            id: agent_id,
            prompt: self.prompt,
        })
    }

    /// Build and run with the prompt set via [`Self::prompt`]. Shorthand for
    /// `self.build()?.run().await`.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::build`] errors and any execution error.
    pub async fn run(self) -> Result<AgentOutput, NornError> {
        self.build()?.run().await
    }

    /// Build and run with an explicit prompt. Shorthand for
    /// `self.build()?.run_with(prompt).await`.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::build`] errors and any execution error.
    pub async fn run_with(self, prompt: impl Into<String>) -> Result<AgentOutput, NornError> {
        self.build()?.run_with(prompt).await
    }
}

/// A fully-assembled, single-use agent. Not [`Clone`]: it owns the session
/// event store and the runtime tool context.
pub struct Agent {
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    loop_context: LoopContext,
    config: AgentLoopConfig,
    model: String,
    output_schema: Option<Value>,
    tool_defs: Vec<ToolDefinition>,
    event_store: Arc<EventStore>,
    event_sender: Option<AgentEventSender>,
    cancel: Option<CancellationToken>,
    inbound: Option<InboundChannel>,
    id: Uuid,
    prompt: Option<String>,
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
    /// sink (replacing any [`AgentBuilder::event_sender`]) and returns the
    /// receiver alongside the run future. Await the future while draining the
    /// receiver concurrently (e.g. `tokio::join!` or a spawned reader).
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

/// Parts needed to install the fork/spawn runtime infrastructure.
struct AgentInfraParts {
    registry: Arc<RwLock<AgentRegistry>>,
    provider: Arc<dyn Provider>,
    event_store: Arc<EventStore>,
    id: Uuid,
}

/// Install [`AgentToolInfra`] on the registry's shared tool context so the
/// agent-coordination tools resolve their runtime.
fn install_agent_infra(registry: &Arc<ToolRegistry>, parts: AgentInfraParts) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    let infra = AgentToolInfra {
        registry: parts.registry,
        mailbox: Arc::new(Mailbox::new()),
        provider: parts.provider,
        event_store: parts.event_store,
        agent_id: parts.id,
        parent_id: None,
        tool_registry: Some(Arc::clone(registry)),
    };
    shared.insert_extension(Arc::new(infra));
}

/// Build the Norn base system prompt from the gated registry and layer it
/// over the profile instructions (or the caller's `system_prompt` override)
/// into `loop_context.system_sections[0]`.
fn install_system_prompt(
    loop_context: &mut LoopContext,
    registry: &ToolRegistry,
    mode: ExecutionMode,
    has_output_schema: bool,
    system_prompt_override: Option<String>,
    append_system_prompt: Option<String>,
    has_auto_compact: bool,
) {
    let inputs = SystemPromptInputs {
        mode,
        tools: collect_tool_prompt_entries(registry),
        has_output_schema,
        event_schema_descriptions: Vec::new(),
        has_rules_engine: loop_context.rules.is_some(),
        has_auto_compact,
    };
    let base_prompt = build_system_prompt(&inputs);

    let profile_prefix = std::mem::take(&mut loop_context.system_sections);
    let mut instructions = system_prompt_override
        .unwrap_or_else(|| profile_prefix.into_iter().next().unwrap_or_default());
    if let Some(append) = append_system_prompt
        && !append.is_empty()
    {
        append_prompt(&mut instructions, &append);
    }

    loop_context.base_prefix = if instructions.is_empty() {
        base_prompt
    } else {
        format!("{base_prompt}\n\n{instructions}")
    };
    loop_context.rebuild_base_section();
}

fn append_prompt(prompt: &mut String, fragment: &str) {
    if prompt.is_empty() {
        *prompt = fragment.to_string();
    } else {
        prompt.push_str("\n\n");
        prompt.push_str(fragment);
    }
}

/// Snapshot a shared event store's events into a fresh owned store. Used only
/// when the original cannot be reclaimed (fork/spawn Arc cycle). The
/// persistence sink is not carried over — only the event content, which is
/// what session resume needs.
fn snapshot_store(store: &EventStore) -> EventStore {
    let snapshot = EventStore::new();
    for event in store.events() {
        if let Err(err) = snapshot.append(event) {
            tracing::warn!(error = %err, "snapshotting event store: append failed");
        }
    }
    snapshot
}

/// Tool metadata for the system prompt builder.
fn collect_tool_prompt_entries(registry: &ToolRegistry) -> Vec<ToolPromptEntry> {
    let names: Vec<String> = registry.names().map(str::to_owned).collect();
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        if let Some(tool) = registry.get(&name) {
            entries.push(ToolPromptEntry {
                name: tool.name().to_owned(),
                category: tool.category(),
                description: tool.description().to_owned(),
                usage_guidance: tool.usage_guidance().map(str::to_owned),
            });
        }
    }
    entries
}

fn append_diagnostic_stop_hook(
    hooks: Option<Arc<HookRegistry>>,
    diagnostic_infra: Option<Arc<DiagnosticInfra>>,
) -> Result<Option<Arc<HookRegistry>>, NornError> {
    let Some(infra) = diagnostic_infra else {
        return Ok(hooks);
    };

    let mut registry = match hooks {
        Some(hooks) => match Arc::try_unwrap(hooks) {
            Ok(registry) => registry,
            Err(_) => {
                return Err(NornError::Config(ConfigError::InvalidConfig {
                    reason:
                        "diagnostic stop hook cannot be appended because hook registry is shared"
                            .to_owned(),
                }));
            }
        },
        None => HookRegistry::new(),
    };
    registry.register(Hook::Stop(Box::new(DiagnosticStopHook::new(infra))));
    Ok(Some(Arc::new(registry)))
}

fn merge_agent_config(mut base: AgentLoopConfig, explicit: AgentLoopConfig) -> AgentLoopConfig {
    if explicit.schema_attempt_budget != AgentLoopConfig::default().schema_attempt_budget {
        base.schema_attempt_budget = explicit.schema_attempt_budget;
    }
    if explicit.max_iterations.is_some() {
        base.max_iterations = explicit.max_iterations;
    }
    if explicit.step_timeout.is_some() {
        base.step_timeout = explicit.step_timeout;
    }
    if explicit.context_window_limit.is_some() {
        base.context_window_limit = explicit.context_window_limit;
    }
    if explicit.auto_compact_threshold_pct.is_some() {
        base.auto_compact_threshold_pct = explicit.auto_compact_threshold_pct;
    }
    if explicit.auto_compact_keep_recent_turns
        != AgentLoopConfig::default().auto_compact_keep_recent_turns
    {
        base.auto_compact_keep_recent_turns = explicit.auto_compact_keep_recent_turns;
    }
    if explicit.schema_tool_name != AgentLoopConfig::default().schema_tool_name {
        base.schema_tool_name = explicit.schema_tool_name;
    }
    if explicit.cache_key.is_some() {
        base.cache_key = explicit.cache_key;
    }
    if explicit.conversation_state != ConversationStateMode::default() {
        base.conversation_state = explicit.conversation_state;
    }
    if explicit.server_compaction_threshold_tokens.is_some() {
        base.server_compaction_threshold_tokens = explicit.server_compaction_threshold_tokens;
    }
    base
}

fn install_tool_catalog(registry: &ToolRegistry, ctx: &ToolContext) {
    let mut entries: Vec<ToolCatalogEntry> = registry
        .names()
        .filter_map(|name| {
            registry
                .get(name)
                .map(|tool| ToolCatalogEntry::tool(tool.name(), tool.description()))
        })
        .collect();

    if let Some(extras) = ctx.get_extension::<ToolCatalogExtras>() {
        entries.extend(extras.0.iter().cloned());
    }

    ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries))));
}

/// Tool definitions (envelope-wrapped schemas) for the provider call.
fn collect_tool_definitions(registry: &ToolRegistry) -> Vec<ToolDefinition> {
    let names: Vec<String> = registry.names().map(str::to_owned).collect();
    let mut defs = Vec::with_capacity(names.len());
    for name in names {
        if let Some(tool) = registry.get(&name) {
            defs.push(ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: wrap_schema_with_envelope(tool.input_schema()),
            });
        }
    }
    defs
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::output::AgentStopReason;
    use crate::integration::hooks::{HookOutcome, StopHook};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;

    fn provider_with(events: Vec<Vec<ProviderEvent>>) -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(events))
    }

    struct BlockingStopHook;

    #[async_trait::async_trait]
    impl StopHook for BlockingStopHook {
        async fn on_stop(&self, final_text: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: format!("user-stop-hook: {} bytes", final_text.len()),
            }
        }
    }

    fn text_completion(text: &str) -> Vec<Vec<ProviderEvent>> {
        vec![vec![
            ProviderEvent::TextDelta {
                text: text.to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]]
    }

    #[test]
    fn build_includes_all_standard_tools_by_default() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        for name in [
            "read",
            "write",
            "edit",
            "bash",
            "apply_patch",
            "search",
            "action_log",
        ] {
            assert!(
                agent.registry.get(name).is_some(),
                "tool '{name}' must be present by default",
            );
        }
    }

    #[test]
    fn build_with_runtime_base_and_diagnostic_override_installs_one_post_check() {
        let temp = tempfile::tempdir().expect("tempdir");
        let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .load_runtime_base()
            .diagnostic_infra(infra)
            .build()
            .expect("build succeeds");

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        assert_eq!(
            ctx.post_checks.len(),
            1,
            "runtime base plus diagnostic override must install exactly one diagnostics post-check",
        );
    }

    #[test]
    fn build_applies_embedding_profile_overrides() {
        let temp = tempfile::tempdir().expect("tempdir");
        let capability = Capability {
            name: "extra".to_owned(),
            tools: vec!["bash".to_owned()],
            system_instructions: vec!["Capability instruction.".to_owned()],
            disallowed_patterns: Vec::new(),
        };

        let agent = AgentBuilder::new(provider_with(vec![]))
            .profile(Profile {
                name: "base".to_owned(),
                model: "test-model".to_owned(),
                tools: Some(vec!["read".to_owned(), "write".to_owned()]),
                system_instructions: vec!["Base instruction.".to_owned()],
                ..Profile::default()
            })
            .working_dir(temp.path())
            .reasoning_effort(ReasoningEffort::High)
            .allowed_tools(&["read"])
            .without_tools(&["write"])
            .capabilities(vec![capability])
            .append_system_prompt("Appended instruction.")
            .build()
            .expect("build succeeds");

        assert_eq!(
            agent.loop_context.reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert!(agent.registry.get("read").is_some());
        assert!(
            agent.registry.get("bash").is_some(),
            "capability tools remain additive"
        );
        assert!(agent.registry.get("write").is_none());
        let base = agent.loop_context.base_system_instruction();
        assert!(base.contains("Base instruction."));
        assert!(base.contains("Capability instruction."));
        assert!(base.contains("Appended instruction."));
    }

    #[test]
    fn build_with_diagnostic_infra_registers_stop_hook() {
        let temp = tempfile::tempdir().expect("tempdir");
        let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .diagnostic_infra(infra)
            .build()
            .expect("build succeeds");

        let hooks = agent
            .loop_context
            .hooks
            .as_ref()
            .expect("diagnostic infra installs hook registry");
        assert_eq!(hooks.stop_len(), 1);
    }

    #[test]
    fn build_without_diagnostic_infra_does_not_register_stop_hook() {
        let temp = tempfile::tempdir().expect("tempdir");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .build()
            .expect("build succeeds");

        assert!(agent.loop_context.hooks.is_none());
    }

    #[tokio::test]
    async fn diagnostic_stop_hook_runs_after_user_stop_hooks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
        let mut registry = HookRegistry::new();
        registry.register(Hook::Stop(Box::new(BlockingStopHook)));

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .hooks(Arc::new(registry))
            .diagnostic_infra(infra)
            .build()
            .expect("build succeeds");

        let outcome = agent
            .loop_context
            .hooks
            .as_ref()
            .expect("hooks installed")
            .run_stop("done")
            .await;

        match outcome {
            HookOutcome::Block { reason } => assert!(reason.starts_with("user-stop-hook")),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => {
                panic!("user hook should block first")
            }
        }
    }

    #[test]
    fn build_with_runtime_base_publishes_shared_task_store_on_active_context() {
        use crate::tools::task::SharedTaskStore;

        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .load_runtime_base()
            .build()
            .expect("build succeeds");

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        ctx.get_extension::<SharedTaskStore>()
            .expect("runtime base task store is installed on the active tool context");
    }

    #[test]
    fn build_publishes_action_log_on_both_contexts_with_same_arc() {
        use crate::session::action_log::ActionLog;

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");

        let loop_log = agent
            .loop_context
            .action_log
            .clone()
            .expect("loop context action log is populated after build");

        let ctx_log = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context")
            .get_extension::<ActionLog>()
            .expect("tool context publishes the ActionLog extension");

        assert!(
            Arc::ptr_eq(&loop_log, &ctx_log),
            "loop context and tool context must share the same ActionLog instance",
        );
    }

    #[tokio::test]
    async fn built_action_log_tool_runs_list_query() {
        use crate::session::action_log::ActionLog;
        use crate::tool::envelope::ToolEnvelope;

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let log = ctx
            .get_extension::<ActionLog>()
            .expect("tool context publishes the ActionLog extension");

        // Seed one completion so the list query has something to return.
        log.record_completion(crate::session::action_log::CompletionRecord {
            tool_name: "read",
            tool_call_id: "tc-built",
            tool_use_description: "",
            outcome: crate::session::action_log::Outcome::Success,
            output: &serde_json::json!({ "path": "src/a.rs", "lines": 3 }),
            args: serde_json::json!({ "path": "src/a.rs" }),
            duration_ms: 1,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let tool = agent.registry.get("action_log").expect("action_log tool");
        let envelope = ToolEnvelope {
            tool_call_id: "self-call".to_string(),
            tool_name: "action_log".to_string(),
            model_args: serde_json::json!({ "query": "list" }),
            runtime_inputs: crate::tool::envelope::RuntimeInputs::default(),
            metadata: Value::Null,
        };
        let out = tool
            .execute(&envelope, ctx.as_ref())
            .await
            .expect("action_log list query runs through the built context");
        assert!(!out.is_error);
        assert_eq!(out.content["query"], "list");
        assert_eq!(out.content["count"], 1);
        assert_eq!(out.content["entries"][0]["id"], "tc-built");
    }

    #[test]
    fn extension_is_published_on_tool_context() {
        #[derive(Debug, PartialEq, Eq)]
        struct Marker(u32);

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .extension(Arc::new(Marker(7)))
            .build()
            .expect("build succeeds");

        let marker = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context")
            .get_extension::<Marker>()
            .expect("custom extension is retrievable through the builder hook");
        assert_eq!(*marker, Marker(7));
    }

    #[test]
    fn without_tools_excludes_named_tools() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .without_tools(&["bash", "write"])
            .build()
            .expect("build succeeds");
        assert!(agent.registry.get("bash").is_none(), "bash excluded");
        assert!(agent.registry.get("write").is_none(), "write excluded");
        assert!(agent.registry.get("read").is_some(), "read remains");
    }

    #[test]
    fn build_errors_when_all_tools_excluded() {
        // Exclude the entire standard set; build must reject an agent with no
        // tools rather than launch one that can do nothing.
        let names = [
            "read",
            "write",
            "edit",
            "bash",
            "apply_patch",
            "search",
            "lsp",
            "task",
            "tool_search",
            "action_log",
            "web_fetch",
            "web_search",
            "spawn_agent",
            "fork",
            "signal_agent",
            "close_agent",
        ];
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .without_tools(&names)
            .build();
        assert!(result.is_err(), "build must reject an empty tool set");
    }

    #[test]
    fn model_override_wins_over_profile() {
        let profile = Profile {
            model: "from-profile".to_string(),
            ..Profile::default()
        };
        let agent = AgentBuilder::new(provider_with(vec![]))
            .working_dir(std::env::temp_dir())
            .profile(profile)
            .model("override-model")
            .build()
            .expect("build succeeds");
        assert_eq!(agent.model, "override-model");
    }

    #[tokio::test]
    async fn run_with_executes_and_returns_output() {
        let output = AgentBuilder::new(provider_with(text_completion("Hello from the agent")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .run_with("say hello")
            .await
            .expect("run succeeds");
        assert!(
            output.is_success(),
            "no-schema text completion is a success"
        );
        assert_eq!(output.text().as_deref(), Some("Hello from the agent"));
        assert!(output.event_store.is_some(), "event store is returned");
    }

    #[tokio::test]
    async fn run_uses_builder_prompt() {
        let output = AgentBuilder::new(provider_with(text_completion("answer")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .prompt("the question")
            .run()
            .await
            .expect("run succeeds");
        assert!(output.is_success());
    }

    #[tokio::test]
    async fn run_without_prompt_errors() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        let result = agent.run().await;
        assert!(result.is_err(), "run with no prompt must error");
    }

    #[tokio::test]
    async fn run_stream_delivers_events() {
        let (mut rx, fut) = AgentBuilder::new(provider_with(text_completion("streamed")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .prompt("go")
            .build()
            .expect("build succeeds")
            .run_stream(64);
        let (output, drained) = tokio::join!(fut, async move {
            let mut seen = 0usize;
            while rx.recv().await.is_ok() {
                seen += 1;
            }
            seen
        });
        assert!(output.expect("run succeeds").is_success());
        assert!(drained > 0, "stream must deliver at least one event");
    }

    #[test]
    fn custom_tool_is_added_alongside_defaults() {
        use crate::error::ToolError;
        use crate::tool::envelope::ToolEnvelope;
        use crate::tool::scheduling::ToolEffect;
        use crate::tool::traits::ToolOutput;

        struct CustomTool;
        #[async_trait::async_trait]
        impl Tool for CustomTool {
            fn name(&self) -> &'static str {
                "custom_probe"
            }
            fn description(&self) -> &'static str {
                "a custom probe tool"
            }
            fn input_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput {
                    content: Value::Null,
                    is_error: false,
                    duration: std::time::Duration::ZERO,
                })
            }
        }

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .tool(Box::new(CustomTool))
            .build()
            .expect("build succeeds");
        assert!(agent.registry.get("custom_probe").is_some());
        assert!(
            agent.registry.get("read").is_some(),
            "defaults still present"
        );
    }

    #[test]
    fn default_retry_policy_is_two_one_second_two_x() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        assert_eq!(agent.loop_context.retry_policy.max_retries, 2);
        assert_eq!(
            agent.loop_context.retry_policy.initial_backoff,
            std::time::Duration::from_secs(1),
        );
        assert!(
            (agent.loop_context.retry_policy.backoff_multiplier - 2.0).abs() < f64::EPSILON,
            "default multiplier must be 2x",
        );
    }

    #[test]
    fn retry_policy_setter_applies_to_loop_context() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .retry_policy(RetryPolicy {
                max_retries: 7,
                ..RetryPolicy::default()
            })
            .build()
            .expect("build succeeds");
        assert_eq!(agent.loop_context.retry_policy.max_retries, 7);
    }

    #[tokio::test]
    async fn cancelled_token_yields_cancelled_stop_reason() {
        let token = CancellationToken::new();
        token.cancel();
        let output = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .cancel_token(token)
            .run_with("go")
            .await
            .expect("cancelled run returns Ok with a Cancelled stop reason");
        assert!(matches!(output.stop_reason, AgentStopReason::Cancelled));
        assert!(!output.is_success());
    }

    #[tokio::test]
    async fn session_resume_accumulates_events() {
        let first = AgentBuilder::new(provider_with(text_completion("first")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .run_with("question one")
            .await
            .expect("first run succeeds");
        let store = first.event_store.expect("event store returned");
        let after_first = store.events().len();
        assert!(after_first > 0, "first run records events");

        let second = AgentBuilder::new(provider_with(text_completion("second")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .session(store)
            .run_with("question two")
            .await
            .expect("resumed run succeeds");
        let store = second.event_store.expect("event store returned");
        assert!(
            store.events().len() > after_first,
            "resumed run appends onto the prior session's events",
        );
    }

    #[tokio::test]
    async fn agent_registry_wires_fork_spawn_infra() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .build()
            .expect("build succeeds");
        let executor: &dyn ToolExecutor = agent.registry.as_ref();
        let result = executor
            .execute(
                "spawn_agent",
                "test-call",
                serde_json::json!({"task": "do x", "model": "gpt-5.5", "role": "worker"}),
            )
            .await;
        if let Err(err) = result {
            assert!(
                !err.to_string().contains("agent runtime not configured"),
                "spawn_agent must get past infra resolution once agent_registry is wired: {err}",
            );
        }
    }
}
