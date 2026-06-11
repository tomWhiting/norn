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
use std::time::Duration;

use parking_lot::RwLock;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::assembly::{
    AgentInfraParts, ExtensionInstaller, OverlayOverrides, RuntimeOverlay, ToolContextParts,
    apply_base_to_loop_context, assemble_tool_context, build_base_tool_registry,
    collect_tool_definitions, effective_agent_config, install_agent_infra,
    install_runtime_base_extensions, install_system_prompt, install_tool_catalog,
    populate_loop_context, resolve_base_profile, resolve_runtime_overlay, resolve_working_dir,
    restore_session_state, validate_workspace_root,
};
use crate::agent::instance::Agent;
use crate::agent::output::AgentOutput;
use crate::agent::registry::AgentRegistry;
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode, ToolExecutor};
use crate::r#loop::inbound::InboundChannel;
use crate::r#loop::retry::RetryPolicy;
use crate::profile::{Capability, Profile, from_profile};
use crate::provider::AgentEventSender;
use crate::provider::request::ReasoningEffort;
use crate::provider::traits::Provider;
use crate::rules::engine::RuleEngine;
use crate::session::store::EventStore;
use crate::system_prompt::builder::ExecutionMode;
use crate::tool::context::SharedWorkingDir;
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::traits::Tool;
use crate::tools::diagnostics::DiagnosticInfra;
use crate::tools::lsp::{LspBackend, LspWorkspace};

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
    workspace_root: Option<PathBuf>,
    bash_drain_grace: Option<Duration>,
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
            workspace_root: None,
            bash_drain_grace: None,
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

    /// Confine the file tools (`read` / `write` / `edit` / `apply_patch`)
    /// to `root`: any path that resolves outside it after symlink-aware
    /// canonicalization is refused, including `..` traversal, absolute
    /// paths, and symlink escapes. `bash` checks its model-supplied
    /// `working_dir` argument against the root but cannot confine what the
    /// command itself does — a known, documented limitation.
    ///
    /// The root must exist and be a directory when [`Self::build`] runs,
    /// otherwise building fails with a configuration error. Unset (the
    /// default) leaves path resolution unconfined for embedders that
    /// operate across arbitrary directories.
    #[must_use]
    pub fn workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Override the grace period the `bash` tool grants its output drains
    /// after the shell exits — the bound on how long a backgrounded child
    /// (`server &`) can hold the output pipes before the tool returns the
    /// buffered output annotated with `streams_still_open`.
    ///
    /// Defaults to 2 seconds (the owner-approved default) when unset.
    /// Applies to the standard `bash` tool; building fails when this is
    /// set but `bash` is excluded from the final tool set.
    #[must_use]
    pub fn bash_drain_grace(mut self, grace: Duration) -> Self {
        self.bash_drain_grace = Some(grace);
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

    /// Publish a typed extension on the agent's shared
    /// [`ToolContext`](crate::tool::context::ToolContext) at build time,
    /// retrievable inside tools via
    /// [`ToolContext::get_extension`](crate::tool::context::ToolContext::get_extension).
    ///
    /// Extensions are installed after the standard and extra tools are
    /// registered and after profile gating, so embedding consumers can attach
    /// host-supplied infrastructure (identity, service handles, registries)
    /// that individual tools read at execution time. Inserting two values of
    /// the same type keeps only the last, matching
    /// [`ToolContext::insert_extension`](crate::tool::context::ToolContext::insert_extension)
    /// semantics.
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
    ///   determined, the workspace root is not an existing directory, the
    ///   named profile cannot be resolved, no tool remains after
    ///   exclusions, or [`Self::bash_drain_grace`] is set while `bash` is
    ///   excluded from the final tool set.
    pub fn build(mut self) -> Result<Agent, NornError> {
        let working_dir = resolve_working_dir(self.working_dir.take())?;
        let workspace_root = validate_workspace_root(self.workspace_root.take())?;
        let shared_wd = SharedWorkingDir::new(working_dir.clone());

        let mut profile = resolve_base_profile(
            self.profile.take(),
            self.profile_name.as_deref(),
            &working_dir,
        )?;
        if let Some(model) = self.model {
            profile.model = model;
        }
        // H13: the programmatic hook registry is taken exactly once. When the
        // runtime base is loaded it is *moved* into `load_runtime_base`, which
        // merges it with the settings-declared shell hooks (programmatic hooks
        // first, so they win first-`Block` conflicts); the merged registry
        // comes back as `base.hooks`. Without a runtime base the registry
        // passes through untouched. Either way nothing is merged twice and
        // nothing is silently dropped.
        let mut programmatic_hooks = self.hooks.take();
        let runtime_base = if self.load_runtime_base {
            let mut profile_for_base = profile.clone();
            let base = crate::runtime_init::load_runtime_base(
                &working_dir,
                &mut profile_for_base,
                programmatic_hooks.take(),
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

        let lsp_backend = self.lsp_backend.clone();
        let registry = build_base_tool_registry(
            lsp_backend.clone(),
            self.extra_tools,
            &self.without_tools,
            self.bash_drain_grace,
        );

        let RuntimeOverlay {
            runtime_base,
            diagnostics,
            diagnostic_infra,
            rules,
            hooks,
        } = resolve_runtime_overlay(
            runtime_base,
            OverlayOverrides {
                diagnostic_infra: self.diagnostic_infra.take(),
                diagnostics: self.diagnostics.take(),
                rules: self.rules.take(),
                hooks: programmatic_hooks,
                lsp_backend,
                lsp_workspace: self.lsp_workspace.take(),
            },
            &working_dir,
        );
        // H14: keep a handle on the final merged registry so it can be
        // published on the shared tool context — sub-agent tools must observe
        // exactly the registry the loop dispatches.
        let hooks_for_ctx = hooks.clone();
        let (mut loop_context, mut registry) = from_profile(&profile, registry, rules, hooks);

        if registry.is_empty() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "no tools available after exclusions; an agent needs at least one tool"
                    .to_string(),
            }));
        }
        if self.bash_drain_grace.is_some() && registry.get("bash").is_none() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "bash_drain_grace is set but the bash tool is not in the final \
                         tool set — remove the override or include bash"
                    .to_string(),
            }));
        }

        let session_id = populate_loop_context(
            &mut loop_context,
            self.retry_policy,
            runtime_base.as_ref(),
            diagnostics.as_ref(),
            &shared_wd,
            &model,
        );
        if let Some(base) = runtime_base.as_ref() {
            apply_base_to_loop_context(&mut loop_context, base);
        }
        let config_override = effective_agent_config(runtime_base.as_ref(), self.agent_config);
        // Both compaction fields are read from the same effective config:
        // the system prompt's compaction guidance must track exactly the
        // config the loop will actually compact under.
        let has_auto_compact = config_override.auto_compact_threshold_pct.is_some()
            && config_override.context_window_limit.is_some();
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

        let (event_store, action_log) =
            restore_session_state(self.session, &mut loop_context, shared_wd.clone());

        let ctx = assemble_tool_context(ToolContextParts {
            shared_wd,
            workspace_root,
            session_id,
            diagnostics: diagnostics.clone(),
            diagnostic_infra,
            hooks: hooks_for_ctx,
            post_checks: self.additional_post_checks,
            provider: Arc::clone(&self.provider),
            action_log: Arc::clone(&action_log),
            extensions: self.extensions,
        });
        registry.set_context(Arc::new(ctx));
        let Some(shared) = registry.shared_context() else {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "tool registry did not publish the assembled tool context".to_string(),
            }));
        };
        if let Some(base) = runtime_base.as_ref() {
            install_runtime_base_extensions(
                shared.as_ref(),
                base,
                diagnostics.as_ref(),
                &working_dir,
            );
        }
        install_tool_catalog(&registry, shared.as_ref());
        let registry = Arc::new(registry);

        // Share the same `Arc<ActionLog>` with the loop so dispatch recording
        // and the `action_log` tool's queries observe one ledger.
        loop_context.action_log = Some(Arc::clone(&action_log));

        let agent_id = self.agent_id.unwrap_or_else(Uuid::new_v4);

        if let Some(agent_registry) = self.agent_registry {
            let child_rx = install_agent_infra(
                &registry,
                shared.as_ref(),
                AgentInfraParts {
                    registry: agent_registry,
                    provider: Arc::clone(&self.provider),
                    event_store: Arc::clone(&event_store),
                    id: agent_id,
                },
            );
            // The runner drains child fork/spawn results at iteration
            // boundaries through this receiver; without it, spawned children
            // would complete into a channel nothing reads.
            loop_context.child_result_rx = Some(child_rx);
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::output::AgentStopReason;
    use crate::integration::hooks::{Hook, HookOutcome, StopHook};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;
    use crate::tool::context::ToolContext;
    use crate::tools::diagnostics::build_diagnostic_infra;

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

    /// H13 regression: a *shared* programmatic hook registry (the caller kept
    /// an `Arc` clone) plus diagnostic infrastructure used to make `build`
    /// fail with "hook registry is shared". The merge-based assembly accepts
    /// it and the caller's stop hook still wins first-`Block` conflicts over
    /// the diagnostic stop hook.
    #[tokio::test]
    async fn shared_hooks_arc_with_diagnostic_infra_keeps_user_hooks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
        let mut registry = HookRegistry::new();
        registry.register(Hook::Stop(Box::new(BlockingStopHook)));
        let shared_hooks = Arc::new(registry);
        let outstanding_clone = Arc::clone(&shared_hooks);

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .hooks(shared_hooks)
            .diagnostic_infra(infra)
            .build()
            .expect("a shared hook Arc must not fail the build");

        let hooks = agent
            .loop_context
            .hooks
            .as_ref()
            .expect("merged hook registry installed");
        let outcome = hooks.run_stop("done").await;
        match outcome {
            HookOutcome::Block { reason } => assert!(
                reason.starts_with("user-stop-hook"),
                "the caller's stop hook must keep precedence: {reason}",
            ),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => {
                panic!("the forwarded user stop hook must still block")
            }
        }
        drop(outstanding_clone);
    }

    /// H14 regression: the *final merged* hook registry is published on the
    /// shared tool context — same `Arc` the loop dispatches — so sub-agent
    /// tools can fire subagent hooks.
    #[tokio::test]
    async fn build_publishes_final_hook_registry_on_tool_context() {
        use crate::integration::hooks::SubagentHook;

        struct BlockingSubagentStop;

        #[async_trait::async_trait]
        impl SubagentHook for BlockingSubagentStop {
            async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
            async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
                HookOutcome::Block {
                    reason: "subagent-hook-fired".to_owned(),
                }
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
        let mut registry = HookRegistry::new();
        registry.register(Hook::Subagent(Box::new(BlockingSubagentStop)));

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .hooks(Arc::new(registry))
            .diagnostic_infra(infra)
            .build()
            .expect("build succeeds");

        let ctx_hooks = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context")
            .get_extension::<HookRegistry>()
            .expect("the merged hook registry must be published on the tool context");
        let loop_hooks = agent
            .loop_context
            .hooks
            .as_ref()
            .expect("loop context carries the merged registry");
        assert!(
            Arc::ptr_eq(&ctx_hooks, loop_hooks),
            "tool context and loop must dispatch the same hook registry",
        );
        let outcome = ctx_hooks.run_subagent_stop("child-1", "worker").await;
        assert!(
            matches!(outcome, HookOutcome::Block { .. }),
            "subagent hooks must fire through the published extension",
        );
    }

    /// A caller-supplied diagnostic collector must never be silently replaced
    /// by the runtime base's collector — on the loop context or on the tool
    /// context.
    #[test]
    fn caller_diagnostics_collector_survives_runtime_base() {
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = DiagnosticCollector::shared();

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .load_runtime_base()
            .diagnostics(Arc::clone(&custom))
            .build()
            .expect("build succeeds");

        let loop_diag = agent
            .loop_context
            .diagnostics
            .as_ref()
            .expect("loop context diagnostics populated");
        assert!(
            Arc::ptr_eq(loop_diag, &custom),
            "loop context must keep the caller's collector",
        );
        let ctx_diag = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context")
            .get_extension::<DiagnosticCollector>()
            .expect("tool context publishes a diagnostic collector");
        assert!(
            Arc::ptr_eq(&ctx_diag, &custom),
            "tool context must keep the caller's collector",
        );
    }

    /// `agent_registry` must wire the *complete* fork/spawn runtime:
    /// `AgentToolInfra`, `AgentHandles`, `ChildResultSender`, the loop's
    /// child-result receiver, and — because every builder-assembled agent
    /// is an embedded/headless runtime with no external status observer —
    /// the `ReclaimOnResultDelivery` marker.
    #[test]
    fn agent_registry_installs_complete_fork_spawn_infra() {
        use crate::agent::result_channel::ChildResultSender;
        use crate::tools::agent::{AgentHandles, AgentToolInfra, ReclaimOnResultDelivery};

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .build()
            .expect("build succeeds");

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        assert!(
            ctx.get_extension::<AgentToolInfra>().is_some(),
            "AgentToolInfra installed",
        );
        assert!(
            ctx.get_extension::<AgentHandles>().is_some(),
            "AgentHandles installed — spawn_agent refuses to run without it",
        );
        assert!(
            ctx.get_extension::<ChildResultSender>().is_some(),
            "ChildResultSender installed — child results need a destination",
        );
        assert!(
            ctx.get_extension::<ReclaimOnResultDelivery>().is_some(),
            "ReclaimOnResultDelivery installed — embedded runtimes reclaim \
             finished children on result delivery",
        );
        assert!(
            agent.loop_context.child_result_rx.is_some(),
            "the loop must hold the receiver that drains child results",
        );
    }

    /// Complete spawn path through a built agent: the child runs on the
    /// builder's provider, its result arrives on the loop's child-result
    /// receiver, and — embedded reclamation — once the result has been
    /// delivered, the child's registry entry and the parent-held handle
    /// are reclaimed. Completion is driven via the result receiver (not
    /// by joining the handle): the wrapper reclaims the handle after
    /// delivery, so holding it would race the reclamation under test.
    #[tokio::test]
    async fn spawned_child_result_reaches_loop_receiver() {
        use crate::tools::agent::AgentHandles;

        let agent_registry = AgentRegistry::shared();
        let mut agent = AgentBuilder::new(provider_with(text_completion("child finished")))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .agent_registry(Arc::clone(&agent_registry))
            .build()
            .expect("build succeeds");

        let executor: &dyn ToolExecutor = agent.registry.as_ref();
        let out = executor
            .execute(
                "spawn_agent",
                "spawn-call",
                serde_json::json!({"task": "report back", "model": "haiku", "role": "worker"}),
            )
            .await
            .expect("spawn_agent dispatches through the built context");
        let child_id = Uuid::parse_str(out["agent_id"].as_str().expect("agent_id string"))
            .expect("agent_id is a uuid");

        let rx = agent
            .loop_context
            .child_result_rx
            .as_mut()
            .expect("loop holds the child result receiver");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("child result must arrive without timing out")
            .expect("channel open");
        assert_eq!(result.agent_id, child_id);
        assert!(result.succeeded, "completed child reports success");
        assert!(
            result.formatted_message.contains("child finished"),
            "the child's output flows through: {}",
            result.formatted_message,
        );

        // Delivery-anchored reclamation: the wrapper reclaims after the
        // send completes, which can land just after `recv` returns —
        // poll briefly instead of asserting immediately.
        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while agent_registry.read().get(child_id).is_some() || handles.contains(child_id) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the delivered child's registry entry \
                 and handle to be reclaimed",
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Track B finding 1 (blocker): `workspace_root` must produce a built
    /// agent whose context denies out-of-root file access through a real
    /// tool call — previously `confine_to_workspace` had zero production
    /// callers, so the control could never be switched on.
    #[tokio::test]
    async fn workspace_root_confines_file_tools_through_built_context() {
        use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir ws");
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "outside the workspace").expect("write secret");
        let inside = root.join("inside.txt");
        std::fs::write(&inside, "inside the workspace").expect("write inside");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(&root)
            .workspace_root(&root)
            .build()
            .expect("build succeeds");
        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let tool = agent.registry.get("read").expect("read tool present");

        let read_envelope = |path: &std::path::Path| ToolEnvelope {
            tool_call_id: "tc-confine".to_owned(),
            tool_name: "read".to_owned(),
            model_args: serde_json::json!({ "path": path.display().to_string() }),
            runtime_inputs: RuntimeInputs::default(),
            metadata: Value::Null,
        };

        let denied = tool
            .execute(&read_envelope(&secret), ctx.as_ref())
            .await
            .expect("confinement refusal is a structured tool error");
        assert!(denied.is_error, "out-of-root read must be refused");
        assert!(
            denied.content["error"]
                .as_str()
                .is_some_and(|m| m.contains("refused")),
            "refusal must be explicit: {}",
            denied.content,
        );

        let allowed = tool
            .execute(&read_envelope(&inside), ctx.as_ref())
            .await
            .expect("in-root read executes");
        assert!(!allowed.is_error, "in-root read must succeed");
    }

    /// Finding 1 companion: without `workspace_root` the built context
    /// stays unconfined — the historical embedder behaviour.
    #[tokio::test]
    async fn builder_without_workspace_root_stays_unconfined() {
        use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};

        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir ws");
        let elsewhere = outer.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, "reachable").expect("write file");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(&root)
            .build()
            .expect("build succeeds");
        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        assert!(
            ctx.workspace_root().is_none(),
            "no workspace_root means no confinement root on the context",
        );
        let tool = agent.registry.get("read").expect("read tool present");
        let out = tool
            .execute(
                &ToolEnvelope {
                    tool_call_id: "tc-unconfined".to_owned(),
                    tool_name: "read".to_owned(),
                    model_args: serde_json::json!({
                        "path": elsewhere.display().to_string(),
                    }),
                    runtime_inputs: RuntimeInputs::default(),
                    metadata: Value::Null,
                },
                ctx.as_ref(),
            )
            .await
            .expect("unconfined read executes");
        assert!(!out.is_error, "unconfined context reads anywhere");
    }

    /// Finding 1: a `workspace_root` that does not exist fails the build
    /// with a typed configuration error instead of confining nothing.
    #[test]
    fn workspace_root_must_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .workspace_root(temp.path().join("does-not-exist"))
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("workspace_root"), "{reason}");
            }
            Err(other) => panic!("expected a config error, got: {other}"),
            Ok(_) => panic!("a missing workspace_root must fail the build"),
        }
    }

    /// Finding 1: a `workspace_root` that is a file (not a directory) fails
    /// the build with a typed configuration error.
    #[test]
    fn workspace_root_must_be_a_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("a-file.txt");
        std::fs::write(&file, "not a dir").expect("write file");
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .workspace_root(&file)
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("not a directory"), "{reason}");
            }
            Err(other) => panic!("expected a config error, got: {other}"),
            Ok(_) => panic!("a non-directory workspace_root must fail the build"),
        }
    }

    /// Track B finding 8: the builder's `bash_drain_grace` override reaches
    /// the registered bash tool — a backgrounded child holding the output
    /// pipes is cut off after the overridden grace, not the 2s default.
    #[tokio::test]
    async fn bash_drain_grace_override_reaches_the_bash_tool() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .bash_drain_grace(std::time::Duration::from_millis(200))
            .build()
            .expect("build succeeds");

        let executor: &dyn ToolExecutor = agent.registry.as_ref();
        let started = std::time::Instant::now();
        let out = executor
            .execute(
                "bash",
                "tc-grace",
                serde_json::json!({ "command": "sleep 5 & echo started" }),
            )
            .await
            .expect("bash executes");
        let elapsed = started.elapsed();
        assert_eq!(
            out["streams_still_open"],
            serde_json::json!(true),
            "the backgrounded sleep holds the pipe past the grace: {out}",
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "a 200ms drain grace must return well before the 2s default \
             (elapsed: {elapsed:?})",
        );
    }

    /// Finding 8: setting `bash_drain_grace` while excluding bash is a
    /// contradiction and must fail the build rather than be silently inert.
    #[test]
    fn bash_drain_grace_with_bash_excluded_fails_build() {
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .bash_drain_grace(std::time::Duration::from_secs(1))
            .without_tools(&["bash"])
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("bash_drain_grace"), "{reason}");
            }
            Err(other) => panic!("expected a config error, got: {other}"),
            Ok(_) => panic!("bash_drain_grace without bash must fail the build"),
        }
    }

    /// Track B finding 3 regression: the compaction guidance in the system
    /// prompt must consult the *effective* agent config (runtime base merged
    /// with explicit builder overrides) — not one field from each source.
    /// Here both compaction fields arrive via the explicit builder config;
    /// the guidance must be present even when the runtime base sets neither.
    #[test]
    fn auto_compact_guidance_follows_effective_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .load_runtime_base()
            .agent_config(AgentLoopConfig {
                context_window_limit: Some(200_000),
                auto_compact_threshold_pct: Some(0.8),
                ..AgentLoopConfig::default()
            })
            .build()
            .expect("build succeeds");

        assert_eq!(agent.config.context_window_limit, Some(200_000));
        assert!(
            (agent
                .config
                .auto_compact_threshold_pct
                .expect("threshold set")
                - 0.8)
                .abs()
                < f64::EPSILON
        );
        assert!(
            agent
                .loop_context
                .base_system_instruction()
                .contains("automatically summarised or cleared"),
            "compaction guidance must be in the system prompt when the \
             effective config enables auto-compaction",
        );
    }

    /// Companion to the finding 3 regression: with no compaction configured
    /// anywhere, the guidance must stay out of the system prompt.
    #[test]
    fn no_auto_compact_guidance_without_compaction_config() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        assert!(
            !agent
                .loop_context
                .base_system_instruction()
                .contains("automatically summarised or cleared"),
            "no compaction config means no compaction guidance",
        );
    }

    /// Track B finding 2 regression: with the runtime base loaded, the
    /// merged `settings.permissions` must compile into a
    /// [`crate::config::PermissionPolicy`] published on the registry's
    /// shared tool context — the embedded path previously installed
    /// nothing, so settings-declared deny rules were never enforced.
    #[test]
    fn runtime_base_installs_permission_policy_on_tool_context() {
        use crate::config::{PermissionDecision, PermissionPolicy};

        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join(".norn")).expect("mkdir .norn");
        std::fs::write(
            temp.path().join(".norn").join("settings.json"),
            r#"{"permissions": {"deny": ["bash"]}}"#,
        )
        .expect("write settings");

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
        let policy = ctx
            .get_extension::<PermissionPolicy>()
            .expect("settings.permissions must be installed on the embedded path");
        assert!(
            matches!(
                policy.evaluate("bash", &serde_json::json!({"command": "ls"})),
                PermissionDecision::Deny { .. }
            ),
            "the settings-declared deny rule must be active",
        );
    }

    /// Track B finding 2, end to end: a deny rule in the project settings
    /// blocks the tool through a real embedded dispatch — the loop's gating
    /// phase refuses the call and records the block as the tool result.
    #[tokio::test]
    async fn settings_deny_rule_blocks_tool_through_embedded_dispatch() {
        use crate::provider::request::ToolCallKind;
        use crate::session::events::SessionEvent;

        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join(".norn")).expect("mkdir .norn");
        std::fs::write(
            temp.path().join(".norn").join("settings.json"),
            r#"{"permissions": {"deny": ["bash"]}}"#,
        )
        .expect("write settings");

        let provider = provider_with(vec![
            vec![
                ProviderEvent::ToolCallComplete {
                    call_id: "call-denied".to_owned(),
                    name: "bash".to_owned(),
                    arguments: r#"{"command": "echo hi"}"#.to_owned(),
                    kind: ToolCallKind::Function,
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                    response_id: None,
                },
            ],
            text_completion("acknowledged")
                .pop()
                .expect("one scripted turn"),
        ]);
        let output = AgentBuilder::new(provider)
            .model("test-model")
            .working_dir(temp.path())
            .load_runtime_base()
            .run_with("run a command")
            .await
            .expect("run completes");

        let store = output.event_store.expect("event store returned");
        let blocked = store.events().iter().any(|event| {
            matches!(
                event,
                SessionEvent::ToolResult { tool_name, output, .. }
                    if tool_name == "bash"
                        && output
                            .get("error")
                            .and_then(Value::as_str)
                            .is_some_and(|m| m.contains("blocked by permissions"))
            )
        });
        assert!(
            blocked,
            "the bash call must be refused by the settings deny rule through \
             real dispatch; events: {:?}",
            store.events(),
        );
    }

    /// Fix 7 regression: resuming a session rebuilds the action log (and its
    /// mutation ledger) from the persisted events, restoring the
    /// session-lifetime queryability contract.
    #[test]
    fn resumed_session_rebuilds_action_log() {
        use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

        let store = EventStore::new();
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: String::new(),
                thinking: String::new(),
                tool_calls: vec![ToolCallEvent {
                    call_id: "tc-resume".to_owned(),
                    name: "read".to_owned(),
                    arguments: serde_json::json!({
                        "path": "src/lib.rs",
                        "tool_use_description": "inspect entry point",
                    }),
                    kind: crate::provider::request::ToolCallKind::Function,
                }],
                usage: EventUsage::default(),
                stop_reason: "tool_use".to_owned(),
                response_id: None,
            })
            .expect("append assistant message");
        store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc-resume".to_owned(),
                tool_name: "read".to_owned(),
                output: serde_json::json!({"lines": 12}),
                duration_ms: 4,
            })
            .expect("append tool result");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .session(store)
            .build()
            .expect("build succeeds");

        let log = agent
            .loop_context
            .action_log
            .as_ref()
            .expect("action log installed");
        let entry = log
            .entry("tc-resume")
            .expect("resumed tool call must be queryable again");
        assert_eq!(entry.tool_use_description, "inspect entry point");
        assert!(matches!(
            entry.outcome,
            crate::session::action_log::Outcome::Success
        ));
    }
}
