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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::assembly::{
    AgentConfigPresence, AgentInfraParts, ExtensionInstaller, OverlayOverrides, RuntimeOverlay,
    ToolContextParts, apply_base_to_loop_context, assemble_tool_context, collect_tool_definitions,
    effective_agent_config, install_agent_infra, install_runtime_base_extensions,
    install_tool_catalog, populate_loop_context, resolve_base_profile, resolve_runtime_overlay,
    resolve_working_dir, restore_session_state, validate_workspace_root,
};
use crate::agent::build_support::{
    compute_read_exempt_roots, resolve_coordination, resolve_root_agent_id, validate_build_inputs,
};
use crate::agent::child_policy::ChildPolicy;
use crate::agent::handle::ResolvedAgentInfo;
use crate::agent::instance::Agent;
use crate::agent::output::RunOutcome;
use crate::agent::prompt_install::{SystemPromptInstall, install_system_prompt};
use crate::agent::registry::AgentRegistry;
use crate::agent::registry_assembly::build_base_tool_registry;
use crate::agent::session_spec::SessionRequest;
use crate::agent_loop::config::{AgentLoopConfig, ToolExecutor};
use crate::agent_loop::event_schemas::EventSchemaSet;
use crate::agent_loop::inbound::{InboundChannel, InboundSender};
use crate::agent_loop::retry::RetryPolicy;
use crate::error::{ConfigError, NornError, SessionError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::integration::variables::{SessionVariable, VariableSource, VariableStore};
use crate::profile::{Capability, Profile, from_profile};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::provider::traits::Provider;
use crate::provider::{AgentEvent, AgentEventSender, SharedAgentEventChannel};
use crate::rules::engine::RuleEngine;
use crate::session::manager::ReplaySummary;
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
/// an [`Agent`], or call [`AgentBuilder::run`] to build and execute in one
/// step.
pub struct AgentBuilder {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) profile: Option<Profile>,
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
    pub(super) terminal_reclamation: bool,
    pub(super) register_root: Option<(String, String)>,
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
            service_tier: None,
            capabilities: Vec::new(),
            working_dir: None,
            workspace_root: None,
            bash_drain_grace: None,
            allowed_tools: None,
            extra_tools: Vec::new(),
            without_tools: Vec::new(),
            lsp_backend: None,
            lsp_workspace: None,
            execution_mode: ExecutionMode::Headless,
            agent_config: AgentLoopConfig::default(),
            agent_config_present: AgentConfigPresence::default(),
            retry_policy: None,
            session: None,
            session_request: None,
            event_channel_capacity: None,
            cancel: None,
            inbound_capacity: None,
            inbound: None,
            inbound_tx: None,
            agent_id: None,
            hooks: None,
            rules: None,
            diagnostics: None,
            diagnostic_infra: None,
            additional_post_checks: Vec::new(),
            agent_registry: None,
            child_policy: None,
            child_result_capacity: None,
            extensions: Vec::new(),
            load_runtime_base: false,
            task_group_slug: None,
            event_schemas: None,
            variables: None,
            variable_pairs: Vec::new(),
            disallowed_tools: Vec::new(),
            // D3: terminal reclamation defaults on, preserving today's
            // unconditional install for every headless / embedded runtime;
            // a status-panel driver (the TUI) opts out with
            // `.terminal_reclamation(false)`.
            terminal_reclamation: true,
            register_root: None,
        }
    }

    /// Validate and assemble the [`Agent`].
    ///
    /// # Errors
    ///
    /// - [`NornError::Config`] when the working directory cannot be
    ///   determined, the workspace root is not an existing directory, the
    ///   named profile cannot be resolved, neither a profile model nor an
    ///   explicit [`Self::model`] is set, no tool remains after
    ///   exclusions, [`Self::bash_drain_grace`] is set while `bash` is
    ///   excluded from the final tool set,
    ///   [`Self::event_channel_capacity`] or [`Self::inbound_capacity`]
    ///   is zero, [`Self::open_session`] conflicts with
    ///   [`Self::session`] / an explicit `cache_key`, the coordination
    ///   envelope is incomplete while [`Self::agent_registry`] is wired
    ///   ([`Self::child_policy`] and [`Self::child_result_capacity`] are
    ///   both required — Norn never assumes a default child policy or
    ///   channel capacity), the envelope is set without
    ///   [`Self::agent_registry`] (it would be silently ignored), or
    ///   [`Self::child_result_capacity`] /
    ///   [`ChildPolicy::inbound_capacity`] is zero.
    /// - [`NornError::Session`] when [`Self::open_session`] fails to
    ///   create, resume, or fork the persisted session.
    pub fn build(mut self) -> Result<Agent, NornError> {
        let invalid = |reason: String| NornError::Config(ConfigError::InvalidConfig { reason });
        validate_build_inputs(
            self.event_channel_capacity,
            self.inbound_capacity,
            self.session.is_some(),
            self.session_request.is_some(),
            self.child_result_capacity,
            self.child_policy.as_ref(),
        )?;
        // Coordination envelope: required exactly when the agent-coordination
        // runtime is wired, rejected when it could only be silently ignored.
        let coordination = resolve_coordination(
            self.agent_registry.take(),
            self.child_policy.take(),
            self.child_result_capacity,
        )?;

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
        if let Some(service_tier) = self.service_tier {
            profile.service_tier = Some(service_tier);
        }
        if let Some(allowed_tools) = self.allowed_tools {
            profile.tools = Some(allowed_tools);
        }
        if !self.capabilities.is_empty() {
            profile.capabilities.extend(self.capabilities);
        }
        let model = profile.model.clone();
        if model.is_empty() {
            return Err(invalid(
                "no model resolved: set .model(\"<model-id>\") on the builder, or supply \
                 a profile that specifies one via .profile(..) / .profile_name(..); Norn \
                 never assumes a default model"
                    .to_string(),
            ));
        }
        let profile_name = (!profile.name.is_empty()).then(|| profile.name.clone());

        // Open the managed persisted session now that the model and
        // working directory are resolved — the index entry records the
        // values the agent actually runs with.
        let mut opened_session: Option<(crate::session::SessionIndexEntry, ReplaySummary)> = None;
        // The root's session-branching identity (child-persistence V2): a
        // disk-persisted session yields a persistent binding — the single
        // allocation authority every spawn/fork/rhai child mint under this
        // agent routes through — while the in-memory `.session(store)` path
        // and the no-session default stay deliberately ephemeral (the
        // `--no-session` honesty axis: children then run memory-only, with
        // the typed `session: None` branch event on the parent timeline).
        let mut session_binding =
            Arc::new(crate::session::SessionBinding::ephemeral_root());
        if let Some(request) = self.session_request.take() {
            // The manager and fsync cadence survive the open: the child
            // brancher applies the SAME data dir, lock deadline, and
            // durability the root itself runs with — inherited, never
            // invented here.
            let manager = request.manager.clone();
            let durability = request.durability;
            let opened = request
                .open(&model, &working_dir.display().to_string())
                .map_err(|e| {
                    NornError::Session(SessionError::StorageError {
                        reason: format!("open_session failed: {e}"),
                    })
                })?;
            if opened.replay.skipped_lines > 0 {
                tracing::warn!(
                    session_id = %opened.entry.id,
                    skipped_lines = opened.replay.skipped_lines,
                    "open_session: tolerant reader skipped lines — the replayed \
                     session history is incomplete",
                );
            }
            // The children/ directory is keyed by the ROOT session id: for
            // a flat (root) session that is the session's own id; resuming
            // a nested child session directly keeps branching into the same
            // root-keyed directory its `rel_path` points into, so
            // grandchild files keep their full-path slugs across restarts.
            let root_for_children = opened
                .entry
                .rel_path
                .as_deref()
                .and_then(|rel| rel.split('/').next())
                .map_or_else(|| opened.entry.id.clone(), str::to_owned);
            let brancher = Arc::new(crate::session::SessionBrancher::new(
                manager,
                root_for_children,
                durability,
            ));
            // Replayed history seeds the ever-used child-name set (Q2
            // for-all-time uniqueness) and recovers a resumed child's own
            // path address from its provenance header.
            session_binding = Arc::new(crate::session::SessionBinding::persistent_root(
                brancher,
                opened.entry.id.clone(),
                &opened.store.events(),
            ));
            self.session = Some(Arc::new(opened.store));
            opened_session = Some((opened.entry, opened.replay));
        }

        let lsp_backend = self.lsp_backend.clone();
        let mut registry = build_base_tool_registry(
            lsp_backend.clone(),
            self.extra_tools,
            &self.without_tools,
            self.bash_drain_grace,
        );
        // D5: register the skill tool on the `load_runtime_base` path when a
        // non-empty skill catalog was discovered, matching the CLI's
        // `!skill_catalog.is_empty()` gate. It is registered before
        // `from_profile` gating so the allow-list/deny-list apply to it
        // exactly as they do in the CLI. Library agents built without a
        // runtime base carry no catalog, so they get no skill tool.
        if let Some(base) = runtime_base.as_ref()
            && !base.skill_catalog.is_empty()
        {
            registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
                crate::agent::registry_assembly::skill_tool_config_from_settings(&base.settings),
            )));
        }

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
        loop_context.event_schemas = self.event_schemas.take();
        // Deny-wins gating: applied after `from_profile`'s allow-list
        // gating so a disallowed name stays unavailable even when the
        // allow-list names it (mirrors `build_runtime`'s `set_disallowed`).
        registry.set_disallowed(std::mem::take(&mut self.disallowed_tools));

        // A zero-tool agent is a legitimate configuration (a pure text
        // transform step: `--allowed-tools ""`, or a profile with
        // `tools = []`): the system prompt omits its `# Tools` section and
        // the provider request carries no tool definitions. Owner decision
        // 2026-07-02 (docs/DECISIONS-2026-07.md) removed the former
        // ≥1-tool build rejection here.
        if self.bash_drain_grace.is_some() && registry.get("bash").is_none() {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "bash_drain_grace is set but the bash tool is not in the final \
                         tool set — remove the override or include bash"
                    .to_string(),
            }));
        }

        // Reconcile a caller-supplied variable store's session id with the
        // resolved one. `open_session` pins the persisted id as
        // authoritative; otherwise the supplied store's id becomes the
        // resolved session id so `{{session_id}}`, the environment, and the
        // store never silently diverge.
        let variables_session_id = self.variables.as_ref().map(|v| v.session_id().to_owned());
        let session_id_override = opened_session
            .as_ref()
            .map(|(entry, _)| entry.id.clone())
            .or_else(|| variables_session_id.clone());
        let session_id = populate_loop_context(
            &mut loop_context,
            self.retry_policy,
            runtime_base.as_ref(),
            diagnostics.as_ref(),
            &shared_wd,
            &model,
            session_id_override.as_deref(),
        );
        if let Some(variables) = self.variables.take() {
            if variables.session_id() != session_id {
                return Err(invalid(format!(
                    "variables store session_id ('{}') disagrees with the resolved \
                     session id ('{session_id}') — open_session pins the persisted id \
                     as authoritative; build the variable store with that id or drop \
                     open_session",
                    variables.session_id(),
                )));
            }
            loop_context.variables = Some(variables);
        }
        // Raw variable pairs (e.g. norn-cli's `--variables KEY=VALUE`) are
        // added to the store `build` already minted with the *resolved*
        // session id — never handed in as a separate store carrying its own
        // independently-minted id, which the reconciliation above would
        // reject against an `open_session`-pinned id. The store uses
        // interior mutability, so the pairs land on whichever store is now
        // installed (the minted one, or a caller-supplied `.variables`).
        if !self.variable_pairs.is_empty()
            && let Some(store) = loop_context.variables.as_ref()
        {
            for (name, value) in std::mem::take(&mut self.variable_pairs) {
                store.set(SessionVariable {
                    name,
                    source: VariableSource::Static { value },
                });
            }
        }
        if let Some(base) = runtime_base.as_ref() {
            apply_base_to_loop_context(&mut loop_context, base);
            // Advertise the skill listing only when the fully-gated registry
            // still carries the `skill` tool — a `without_tools`/deny that
            // removes it must also remove the "# Available Skills" section,
            // matching the child paths (never advertise what cannot be
            // called). `registry` here is post-`from_profile` and
            // post-`set_disallowed`, so `get` reflects the final surface.
            crate::agent::arming::apply_skill_listing(
                &mut loop_context,
                &base.skill_catalog,
                registry.get("skill").is_some(),
            );
        }
        let mut config_override = effective_agent_config(
            runtime_base.as_ref(),
            self.agent_config,
            self.agent_config_present,
        );
        if let Some((entry, _)) = opened_session.as_ref() {
            // The persisted session's id is the prompt cache key on this
            // path: an explicitly configured cache_key would silently
            // contradict it, so the ambiguity is rejected loudly.
            if let Some(existing) = config_override.cache_key.as_ref() {
                return Err(invalid(format!(
                    "open_session wires the session id ('{}') as the prompt cache_key, \
                     but the agent config already sets cache_key ('{existing}') — \
                     remove the explicit cache_key or drop open_session",
                    entry.id,
                )));
            }
            config_override.cache_key = Some(entry.id.clone());
        }
        // Both compaction fields are read from the same effective config:
        // the system prompt's compaction guidance must track exactly the
        // config the loop will actually compact under. The output schema
        // is read from the same source for the same reason.
        // Arm auto-compaction through the one shared mechanism every launch
        // path uses (root here; spawn/fork/rhai children at their own
        // construction sites): it installs the token estimator and the
        // context-edit tracker on the loop context and fills an unset
        // context window from the model catalog for the resolved model, so
        // the loop's auto-compaction trigger, the system prompt's compaction
        // guidance, and the tool-output budget all read one effective value.
        // The fill runs only when the merged window is still None — every
        // explicit source (settings, `-c` overrides) has already been
        // overlaid above, so an explicit window stays authoritative even
        // when it equals the catalogued value. The validation that follows
        // is the 2026-07-05 incident guard (owner-ruled): an explicit
        // window the model cannot honour is a hard error (a global 272k
        // settings override silently mis-armed a 128k model — protections
        // armed beyond the real wall never fire), and a model absent from
        // the catalog with no explicit window is a hard error rather than
        // a silently-unprotected run.
        crate::agent::arming::arm_auto_compaction(&mut loop_context, &mut config_override, &model);
        crate::agent::arming::validate_context_window(&config_override, &model)
            .map_err(NornError::Config)?;
        // The system prompt only promises compaction the loop will actually
        // perform. The runtime trigger (`maybe_auto_compact`) disables itself
        // when the reserve is at or above the window — every step would
        // otherwise fire — so a build with that shape must not emit the
        // compaction guidance that would then never come true. Both values
        // are known here, so the contradiction is caught (and warned) once at
        // build; the per-preflight warn in `compaction.rs` stays for library
        // callers who mutate the config after `build`.
        let has_auto_compact = match (
            config_override.context_window_limit,
            config_override.auto_compact_reserve_tokens,
        ) {
            (Some(limit), Some(reserve)) if reserve >= limit => {
                tracing::warn!(
                    reserve_tokens = reserve,
                    context_window_limit = limit,
                    "auto_compact_reserve_tokens is at or above context_window; \
                     the runtime trigger disables in this configuration, so the \
                     system prompt will not claim auto-compaction is active",
                );
                false
            }
            (Some(_), Some(_)) => true,
            _ => false,
        };
        let tool_output_context_window = config_override.context_window_limit;
        // The provider is bound here, so the prompt's tools section is
        // resolved against its capabilities — hosted-replaced tools are
        // described as provider-native, never as callable functions. A
        // session resume re-enters this build with the provider being
        // re-bound, so the section is recomputed rather than carried over.
        install_system_prompt(
            &mut loop_context,
            SystemPromptInstall {
                registry: &registry,
                mode: self.execution_mode,
                has_output_schema: config_override.output_schema.is_some(),
                system_prompt_override: self.system_prompt,
                append_system_prompt: self.append_system_prompt,
                has_auto_compact,
                capabilities: self.provider.capabilities(),
            },
        );

        let tool_defs = collect_tool_definitions(&registry);

        let (event_store, action_log) =
            restore_session_state(self.session, &mut loop_context, shared_wd.clone());

        // Read carve-out (DECISIONS §0.6(b)): under confinement, a confined
        // agent may READ the well-known, convention-defined skill / profile /
        // config locations that lie OUTSIDE the workspace root. Computed in
        // `build_support` — empty when no confinement root is set.
        let read_exempt_roots = compute_read_exempt_roots(workspace_root.as_deref(), &working_dir);

        let ctx = assemble_tool_context(ToolContextParts {
            shared_wd,
            workspace_root,
            read_exempt_roots,
            session_id: session_id.clone(),
            diagnostics: diagnostics.clone(),
            diagnostic_infra,
            hooks: hooks_for_ctx,
            post_checks: self.additional_post_checks,
            provider: Arc::clone(&self.provider),
            action_log: Arc::clone(&action_log),
            context_window_limit: tool_output_context_window,
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

        // Root registry registration (D2): opt-in and effective only
        // alongside `.agent_registry(..)`; the reservation mints the id so
        // the registered root entry and the running agent share one id.
        let agent_id = resolve_root_agent_id(
            self.register_root.take(),
            coordination.as_ref(),
            &model,
            self.agent_id,
        )?;

        // Event channel: the builder owns the broadcast channel and the
        // root sender, and publishes the raw channel on the tool context
        // so fork/spawn children stream their events through the same
        // channel the embedder subscribes to.
        let (events_tx, event_sender) = match self.event_channel_capacity {
            Some(capacity) => {
                let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(capacity);
                shared.insert_extension(Arc::new(SharedAgentEventChannel(tx.clone())));
                let sender = AgentEventSender::new(tx.clone(), agent_id, "root".to_string());
                (Some(tx), Some(sender))
            }
            None => (None, None),
        };

        // The root's run-cancellation token is resolved once — the
        // builder's explicit `cancel_token`, or a fresh token — and used
        // for both the Agent (run + handle) and, when coordination is
        // installed, the published `AgentCancellation` extension, so
        // cancelling the handle cascades to every spawned descendant's
        // child token (W3.5). Two tokens here would silently sever the
        // cascade from the control surface.
        let cancel = self.cancel.unwrap_or_default();

        // The agent-registry handle survives the coordination move below so
        // the schedule executor can consult live status (idle-child
        // detection is a child-path concern, but the handle is cheap and
        // honest to thread through for the root too).
        let agent_registry_for_schedule = coordination
            .as_ref()
            .map(|(agent_registry, _)| Arc::clone(agent_registry));
        if let Some((agent_registry, envelope)) = coordination {
            let child_rx = install_agent_infra(
                &registry,
                shared.as_ref(),
                AgentInfraParts {
                    registry: agent_registry,
                    provider: Arc::clone(&self.provider),
                    event_store: Arc::clone(&event_store),
                    session: Arc::clone(&session_binding),
                    id: agent_id,
                    envelope,
                    // Children address "parent" through the router even at
                    // the top level: the root's inbound sender (when the
                    // builder configured one) is registered under the
                    // root's id. Without an inbound channel, messaging the
                    // root fails honestly as NotRouted.
                    root_inbound: self.inbound_tx.clone(),
                    cancel: cancel.clone(),
                    terminal_reclamation: self.terminal_reclamation,
                },
            );
            // The runner drains child fork/spawn results at iteration
            // boundaries through this receiver; without it, spawned children
            // would complete into a channel nothing reads.
            loop_context.child_result_rx = Some(child_rx);
        }
        loop_context.agent_id = Some(agent_id);
        if let Some(infra) = shared.get_extension::<crate::tools::agent::AgentToolInfra>() {
            loop_context.pending_agent_messages = Some(Arc::clone(&infra.pending_messages));
        }
        // In-session cron (N-026): the shared arming mechanism — rebuild
        // from session events (resume restore), install the ScheduleHandle
        // the cron tool resolves, arm the live executor, and bind its guard
        // to the loop context so dropping the agent aborts the timer task.
        crate::agent::arming::arm_root_schedule_executor(
            shared.as_ref(),
            &mut loop_context,
            &event_store,
            agent_id,
            self.inbound_tx.clone(),
            agent_registry_for_schedule.clone(),
        );
        // NP-001 background-process manager: install it on the shared context
        // (the `process` tool resolves it) and bind its shutdown guard to the
        // loop context. Ordered after scheduling so the durable pending store
        // exists — the completion sink queues completions into it.
        crate::agent::arming::arm_process_manager(
            shared.as_ref(),
            &mut loop_context,
            &event_store,
            agent_id,
            self.inbound_tx.clone(),
            agent_registry_for_schedule,
        );

        let (session_entry, replay) = match opened_session {
            Some((entry, replay)) => (Some(entry), Some(replay)),
            None => (None, None),
        };
        let info = Arc::new(ResolvedAgentInfo {
            agent_id,
            model: model.clone(),
            profile_name,
            tool_names: tool_defs.iter().map(|def| def.name.clone()).collect(),
            session_id,
            working_dir,
            output_schema: config_override.output_schema.clone(),
        });

        Ok(Agent {
            provider: self.provider,
            registry,
            loop_context,
            config: config_override,
            model,
            tool_defs,
            event_store,
            event_sender,
            events_tx,
            cancel,
            inbound: self.inbound,
            inbound_tx: self.inbound_tx,
            id: agent_id,
            info,
            session_entry,
            replay,
        })
    }

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
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    /// Explicit window for test fixtures: "test-model" is deliberately
    /// uncatalogued, and `build` now hard-errors on an unarmed window
    /// (2026-07-05 incident guard). `272_000` is gpt-5.5's catalogued
    /// standard window (assets/models.json) — factual, not invented.
    const TEST_CONTEXT_WINDOW: u64 = 272_000;
    use serde_json::Value;

    use super::*;
    use crate::agent::child_policy::CoordinationEnvelope;
    use crate::agent::output::AgentStopReason;
    use crate::agent::session_spec::SessionSpec;
    use crate::integration::hooks::{Hook, HookOutcome, StopHook};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;
    use crate::session::SessionManager;
    use crate::session::store::DurabilityPolicy;
    use crate::tool::context::ToolContext;
    use crate::tools::diagnostics::build_diagnostic_infra;

    fn provider_with(events: Vec<Vec<ProviderEvent>>) -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(events))
    }

    /// The documented-proposal coordination envelope used by tests that
    /// wire `.agent_registry(..)` — a deliberate test-caller choice, not
    /// a library default.
    fn test_child_policy() -> ChildPolicy {
        use crate::agent::child_policy::{DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        }
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

    /// A zero-tool agent (empty allow-list) is a supported configuration —
    /// a pure text-transform step. Build must succeed, the gated registry
    /// must expose no tools, and the assembled system prompt must omit the
    /// `# Tools` section entirely. Regression for the former ≥1-tool build
    /// rejection (owner decision 2026-07-02).
    #[test]
    fn zero_tool_agent_builds_for_transform_only_use() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .allowed_tools(&[])
            .build()
            .expect("a zero-tool agent must build");
        assert_eq!(
            agent.registry.names().count(),
            0,
            "empty allow-list must gate out every tool",
        );
        let parts = agent.into_parts();
        let prompt = parts
            .loop_context
            .system_sections
            .first()
            .expect("system prompt section assembled");
        assert!(
            !prompt.contains("# Tools"),
            "zero-tool system prompt must omit the # Tools section, got:\n{prompt}",
        );
    }

    /// N-026 R6 (root path): `build` registers the `cron` tool, installs
    /// the `ScheduleHandle` extension on the shared tool context, and binds
    /// the live executor's guard to the agent's loop context — without any
    /// tool call having run.
    #[tokio::test]
    async fn build_registers_cron_tool_and_arms_schedule_executor() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        assert!(
            agent.registry.get("cron").is_some(),
            "the builder path registers the cron tool",
        );
        let handle = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::schedule::ScheduleHandle>()
            .expect("ScheduleHandle installed at assembly");
        assert_eq!(handle.agent_id, agent.id, "the handle carries the root id");
        assert!(handle.store.is_empty(), "a fresh session arms empty");
        assert!(
            agent.loop_context.schedule_executor.is_some(),
            "the executor guard rides on the loop context",
        );
    }

    /// N-026: `.without_tools(["cron"])` removes the scheduling tool like
    /// any other exclusion — the arming stays (harmless), the surface gates.
    #[tokio::test]
    async fn without_tools_removes_cron_from_the_surface() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .without_tools(&["cron"])
            .build()
            .expect("build succeeds");
        assert!(agent.registry.get("cron").is_none());
    }

    /// N-026 R5/R6: a resumed root rebuilds its schedule store from the
    /// session's `schedule.created` events and arms the executor from it —
    /// no tool call involved. The recurring survivor re-arms from resume
    /// time (a single next fire within its interval, no backfill).
    #[tokio::test]
    async fn resumed_root_arms_executor_from_rebuilt_store() {
        let session = EventStore::new();
        let record = crate::schedule::ScheduleRecord::new(
            uuid::Uuid::new_v4(),
            crate::schedule::ScheduleSpec::Every {
                duration: std::time::Duration::from_hours(1),
            },
            "hourly triage".to_string(),
            uuid::Uuid::new_v4(),
            chrono::Utc::now() - chrono::TimeDelta::hours(5),
        )
        .expect("record");
        let id = record.id;
        crate::schedule::append_schedule_event(
            &session,
            &crate::schedule::ScheduleLifecycle::Created { record },
        )
        .expect("persist created");

        let before = chrono::Utc::now();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .session(Arc::new(session))
            .build()
            .expect("resume build succeeds");
        let handle = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::schedule::ScheduleHandle>()
            .expect("ScheduleHandle installed");
        let restored = handle.store.get(id).expect("pending schedule restored");
        assert!(!restored.late, "recurring schedules never fire late");
        assert!(
            restored.next_fire > before
                && restored.next_fire
                    <= before + chrono::TimeDelta::hours(1) + chrono::TimeDelta::seconds(5),
            "one natural next fire within the hour, no backfill: {}",
            restored.next_fire,
        );
    }

    /// N-026 R6: dropping the agent instance aborts the schedule executor —
    /// a short `Every` schedule stops firing after the drop (virtual-time
    /// advance well past several intervals, `next_fire` frozen).
    #[tokio::test(start_paused = true)]
    async fn dropping_agent_aborts_schedule_executor() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        let handle = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::schedule::ScheduleHandle>()
            .expect("ScheduleHandle installed");
        let record = crate::schedule::ScheduleRecord::new(
            uuid::Uuid::new_v4(),
            crate::schedule::ScheduleSpec::Every {
                duration: std::time::Duration::from_secs(1),
            },
            "tick".to_string(),
            agent.id,
            chrono::Utc::now(),
        )
        .expect("record");
        let id = record.id;
        let armed_fire = record.next_fire;
        handle.store.insert(record);

        drop(agent);
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let frozen = handle.store.get(id).expect("record still pending");
        assert_eq!(
            frozen.next_fire, armed_fire,
            "no fire after the agent dropped — the executor died with it",
        );
        assert!(
            !handle.event_store.events().iter().any(|e| matches!(
                e,
                crate::session::events::SessionEvent::Custom { event_type, .. }
                    if event_type == crate::schedule::SCHEDULE_FIRED_EVENT_TYPE
            )),
            "no schedule.fired persisted after the drop",
        );
    }

    #[test]
    fn build_includes_all_standard_tools_by_default() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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

    /// Guard that swaps `NORN_HOME` for the duration of a test and restores
    /// the prior value on drop. Consumers must be `#[serial]`.
    #[allow(unsafe_code)]
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    #[allow(unsafe_code)]
    impl NornHomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial]` on the sole consumer, so no
            // concurrent reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", path) };
            Self { prior }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            // SAFETY: see [`Self::set`].
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    /// Findings 1 & 2 (security): under confinement the computed read-exempt
    /// set is drawn ONLY from well-known, non-model-writable convention
    /// locations. It contains the NORN_HOME-aware home skills dir and the
    /// narrow `~/.norn/{skills,profiles,rules}` subdirs; it never contains a
    /// settings-declared `skills.search_paths` entry (Finding 1: those are
    /// model-writable and would be a persistent escape), nor the `~/.norn/`
    /// root itself or `~/.norn/sessions/` (Finding 2: sessions hold
    /// transcripts for ALL workspaces).
    #[test]
    #[serial_test::serial]
    #[allow(clippy::unwrap_used)]
    fn confined_read_exempt_set_excludes_settings_paths_and_sessions() {
        let norn_home = tempfile::tempdir().expect("norn_home");
        let _guard = NornHomeGuard::set(norn_home.path());

        // The convention subdirs must exist on disk — the context setter
        // canonicalizes and drops non-existent exempt roots.
        for sub in ["skills", "profiles", "rules", "sessions"] {
            std::fs::create_dir(norn_home.path().join(sub)).expect("mk norn subdir");
        }

        // A settings-declared skill search path that lives OUTSIDE the
        // workspace and exists on disk — under the pre-fix code this would
        // have been canonicalized into the exempt set.
        let outside = tempfile::tempdir().expect("outside");
        let outside_skills = outside.path().join("evil-skills");
        std::fs::create_dir(&outside_skills).expect("mk outside skills");

        // Workspace with a project settings file declaring that search path.
        let workspace = tempfile::tempdir().expect("workspace");
        let dot_norn = workspace.path().join(".norn");
        std::fs::create_dir(&dot_norn).expect("mk .norn");
        std::fs::write(
            dot_norn.join("settings.json"),
            serde_json::json!({
                "skills": { "search_paths": [outside_skills.to_string_lossy()] }
            })
            .to_string(),
        )
        .expect("write settings");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(workspace.path())
            .workspace_root(workspace.path())
            .load_runtime_base()
            .build()
            .expect("build succeeds");

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let roots = ctx.read_exempt_roots();

        let canon = |p: &std::path::Path| p.canonicalize().expect("canonicalize");
        let skills = canon(&norn_home.path().join("skills"));
        let profiles = canon(&norn_home.path().join("profiles"));
        let rules = canon(&norn_home.path().join("rules"));

        assert!(
            roots.contains(&skills),
            "home skills dir must be exempt: {roots:?}",
        );
        assert!(
            roots.contains(&profiles),
            "home profiles dir must be exempt: {roots:?}",
        );
        assert!(
            roots.contains(&rules),
            "home rules dir must be exempt: {roots:?}",
        );

        let sessions = canon(&norn_home.path().join("sessions"));
        let norn_root = canon(norn_home.path());
        let settings_path = canon(&outside_skills);
        assert!(
            !roots.contains(&sessions),
            "sessions dir must NEVER be exempt (holds all-workspace transcripts): {roots:?}",
        );
        assert!(
            !roots.contains(&norn_root),
            "the ~/.norn root itself must never be exempt: {roots:?}",
        );
        assert!(
            !roots.contains(&settings_path),
            "settings-declared search_paths must never be exempt (model-writable): {roots:?}",
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .reasoning_effort(ReasoningEffort::High)
            .service_tier(ServiceTier::Fast)
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
        assert_eq!(agent.loop_context.service_tier, Some(ServiceTier::Fast));
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            metadata: Value::Null,
        };
        let out = tool
            .execute(&envelope, ctx.as_ref())
            .await
            .expect("action_log list query runs through the built context");
        assert!(!out.is_error());
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .without_tools(&["bash", "write"])
            .build()
            .expect("build succeeds");
        assert!(agent.registry.get("bash").is_none(), "bash excluded");
        assert!(agent.registry.get("write").is_none(), "write excluded");
        assert!(agent.registry.get("read").is_some(), "read remains");
    }

    #[test]
    fn build_succeeds_when_all_tools_excluded() {
        // Excluding the entire standard set is a supported zero-tool
        // configuration (pure text transform) — build must succeed with an
        // empty gated registry rather than reject it. Owner decision
        // 2026-07-02 (docs/DECISIONS-2026-07.md) removing the former
        // ≥1-tool rejection.
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
            "wake_agent",
            "close_agent",
            "agents",
            "cron",
            "process",
        ];
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .without_tools(&names)
            .build()
            .expect("a fully-excluded tool set must still build");
        assert_eq!(
            agent.registry.names().count(),
            0,
            "every standard tool must be gated out",
        );
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .build()
            .expect("build succeeds");
        assert_eq!(agent.model, "override-model");
    }

    #[tokio::test]
    async fn run_executes_and_returns_output() {
        let outcome = AgentBuilder::new(provider_with(text_completion("Hello from the agent")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .run("say hello")
            .await
            .expect("run succeeds");
        assert!(
            outcome.is_completed(),
            "no-schema text completion is a completed run"
        );
        assert_eq!(
            outcome.output().text().as_deref(),
            Some("Hello from the agent")
        );
        assert!(
            outcome.output().event_store.is_some(),
            "event store is returned"
        );
    }

    /// An empty (or whitespace-only) prompt has no defined model-facing
    /// meaning — it must be rejected with a typed error at the run
    /// boundary, never sent to the provider as undefined behaviour.
    #[tokio::test]
    async fn run_rejects_empty_and_whitespace_prompts() {
        for prompt in ["", "   ", "\n\t "] {
            let agent = AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .build()
                .expect("build succeeds");
            match agent.run(prompt).await {
                Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                    assert!(reason.contains("empty prompt"), "{reason}");
                }
                Err(other) => panic!("expected a typed config error, got: {other}"),
                Ok(_) => panic!("prompt {prompt:?} must be rejected"),
            }
        }
    }

    /// The handle's subscription replaces the old `run_stream`: configure
    /// the channel capacity on the builder, subscribe through the handle,
    /// and drain alongside the run. Real consumers drain concurrently and
    /// stop when the run future resolves (the handle keeps the channel
    /// open, so end-of-run — not channel close — is the stop signal).
    #[tokio::test]
    async fn handle_subscription_delivers_events() {
        let agent = AgentBuilder::new(provider_with(text_completion("streamed")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .event_channel_capacity(64)
            .build()
            .expect("build succeeds");
        let handle = agent.handle();
        let mut rx = handle
            .subscribe()
            .expect("event channel configured — subscribe must succeed");
        let output = agent.run("go").await.expect("run succeeds");
        assert!(output.is_completed());
        // Every event the run broadcast is buffered for this receiver.
        let mut seen = 0usize;
        while rx.try_recv().is_ok() {
            seen += 1;
        }
        assert!(seen > 0, "the run must deliver at least one event");
    }

    #[test]
    fn subscribe_without_event_channel_is_none() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        assert!(
            agent.handle().subscribe().is_none(),
            "no configured channel means no subscription — never a silent dead channel",
        );
        assert!(agent.handle().inbound_sender().is_none());
    }

    #[test]
    fn zero_channel_capacities_fail_build() {
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .event_channel_capacity(0)
            .build();
        assert!(matches!(
            result,
            Err(NornError::Config(ConfigError::InvalidConfig { .. }))
        ));
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .inbound_capacity(0)
            .build();
        assert!(matches!(
            result,
            Err(NornError::Config(ConfigError::InvalidConfig { .. }))
        ));
    }

    fn invalid_config_reason(result: Result<Agent, NornError>) -> String {
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => reason,
            Err(other) => panic!("expected a typed config error, got: {other}"),
            Ok(_) => panic!("build must fail"),
        }
    }

    /// W3.0: wiring `.agent_registry(..)` without the coordination
    /// envelope is a build-time error naming every missing setter — Norn
    /// never assumes a default child policy or channel capacity.
    #[test]
    fn agent_registry_without_envelope_fails_build() {
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .agent_registry(AgentRegistry::shared())
                .build(),
        );
        assert!(reason.contains(".child_policy"), "{reason}");
        assert!(reason.contains(".child_result_capacity"), "{reason}");
    }

    /// Each missing half of the envelope is named individually.
    #[test]
    fn partial_coordination_envelope_names_the_missing_setter() {
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .agent_registry(AgentRegistry::shared())
                .child_policy(test_child_policy())
                .build(),
        );
        assert!(reason.contains(".child_result_capacity"), "{reason}");
        assert!(!reason.contains(".child_policy(ChildPolicy"), "{reason}");

        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .agent_registry(AgentRegistry::shared())
                .child_result_capacity(256)
                .build(),
        );
        assert!(reason.contains(".child_policy"), "{reason}");
    }

    /// An envelope without `.agent_registry(..)` would be silently
    /// ignored — that is rejected, never tolerated.
    #[test]
    fn orphaned_coordination_envelope_fails_build() {
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .child_policy(test_child_policy())
                .build(),
        );
        assert!(reason.contains("child_policy"), "{reason}");
        assert!(reason.contains("agent_registry"), "{reason}");

        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .child_result_capacity(256)
                .build(),
        );
        assert!(reason.contains("child_result_capacity"), "{reason}");
        assert!(reason.contains("agent_registry"), "{reason}");
    }

    /// Zero capacities anywhere in the envelope fail the build — a
    /// zero-capacity channel cannot exist.
    #[test]
    fn zero_coordination_capacities_fail_build() {
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .agent_registry(AgentRegistry::shared())
                .child_policy(test_child_policy())
                .child_result_capacity(0)
                .build(),
        );
        assert!(reason.contains("child_result_capacity is 0"), "{reason}");

        let mut policy = test_child_policy();
        policy.inbound_capacity = 0;
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .agent_registry(AgentRegistry::shared())
                .child_policy(policy)
                .child_result_capacity(256)
                .build(),
        );
        assert!(
            reason.contains("child_policy.inbound_capacity is 0"),
            "{reason}",
        );
    }

    /// W3.0 carriage: the validated envelope is published on the shared
    /// tool context verbatim, so the spawn/fork paths read the root's
    /// policy and capacities from one place.
    #[test]
    fn coordination_envelope_is_published_on_tool_context() {
        let policy = test_child_policy();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(policy.clone())
            .child_result_capacity(17)
            .build()
            .expect("build succeeds");

        let envelope = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context")
            .get_extension::<CoordinationEnvelope>()
            .expect("CoordinationEnvelope published on the shared context");
        assert_eq!(envelope.child_policy, policy);
        assert_eq!(envelope.child_result_capacity, 17);
        assert!(
            agent.loop_context.child_result_rx.is_some(),
            "the child-result receiver is wired alongside the envelope",
        );
    }

    /// W3.5 (review U1-M1): with NO explicit `cancel_token`, the builder
    /// must still bind the published `AgentCancellation` and the agent's
    /// own run token (the one `Agent::run` / `AgentHandle::cancel`
    /// observe) to the SAME trigger. Two independently-minted defaults
    /// compile fine and silently sever the cascade from the control
    /// surface on every default-built agent — this pins the single
    /// resolution in `build`.
    #[test]
    fn default_built_agent_publishes_its_own_run_token() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .build()
            .expect("build succeeds");

        let published = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::tools::agent::AgentCancellation>()
            .expect("AgentCancellation published on the shared context");
        assert!(!published.0.is_cancelled());

        agent.cancel.cancel();
        assert!(
            published.0.is_cancelled(),
            "the published cascade token must share the trigger with the \
             default-built agent's own run token",
        );
    }

    /// W3.2 routing: a root built with an inbound channel registers its
    /// sender in the message router under its own id, so children can
    /// address `"parent"` at the top level; without an inbound channel
    /// the root is honestly unrouted.
    #[test]
    fn root_inbound_sender_registers_in_message_router() {
        let id = Uuid::new_v4();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_id(id)
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .inbound_capacity(8)
            .build()
            .expect("build succeeds");
        let infra = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::tools::agent::AgentToolInfra>()
            .expect("AgentToolInfra installed");
        assert!(
            infra.router.is_routed(id),
            "the root's inbound sender must be registered under the root id",
        );

        let unrouted_id = Uuid::new_v4();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_id(unrouted_id)
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .build()
            .expect("build succeeds");
        let infra = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::tools::agent::AgentToolInfra>()
            .expect("AgentToolInfra installed");
        assert!(
            !infra.router.is_routed(unrouted_id),
            "a root without an inbound channel must not be routed",
        );
    }

    /// The builder owns the event channel end to end: the raw broadcast
    /// channel must be published on the tool context as
    /// `SharedAgentEventChannel` so fork/spawn children stream their
    /// events through the same channel the embedder subscribes to.
    #[test]
    fn event_channel_is_published_for_subagents() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .event_channel_capacity(16)
            .build()
            .expect("build succeeds");
        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let shared_channel = ctx
            .get_extension::<SharedAgentEventChannel>()
            .expect("SharedAgentEventChannel must be installed for child streaming");
        let mut handle_rx = agent.handle().subscribe().expect("subscribe");
        shared_channel
            .0
            .send(crate::provider::AgentEvent {
                agent_id: Uuid::nil(),
                agent_role: std::sync::Arc::from("spawn/test"),
                event: crate::provider::AgentEventKind::Provider(ProviderEvent::TextDelta {
                    text: "child delta".to_string(),
                }),
            })
            .expect("handle subscription keeps the channel open");
        let received = handle_rx.try_recv().expect("event arrives");
        assert_eq!(&*received.agent_role, "spawn/test");
    }

    /// The inbound sender is reachable both mid-chain (for infrastructure
    /// built before the agent) and on the handle, and both feed the same
    /// channel the loop drains.
    #[test]
    fn inbound_sender_available_pre_build_and_on_handle() {
        let builder = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .inbound_capacity(8);
        let pre_build = builder
            .inbound_sender()
            .expect("sender available as soon as the capacity is set");
        let agent = builder.build().expect("build succeeds");
        let handle_sender = agent
            .handle()
            .inbound_sender()
            .expect("sender available on the handle");
        // Both senders feed the channel whose receiver the agent holds.
        assert!(agent.inbound.is_some(), "loop receives the inbound half");
        drop((pre_build, handle_sender));
    }

    #[test]
    fn handle_exposes_resolved_introspection() {
        let schema = serde_json::json!({"type": "object", "required": ["answer"]});
        let temp = tempfile::tempdir().expect("tempdir");
        let id = Uuid::new_v4();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .profile(Profile {
                name: "reviewer".to_owned(),
                model: "profile-model".to_owned(),
                ..Profile::default()
            })
            .model("resolved-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .agent_id(id)
            .allowed_tools(&["read", "search"])
            .output_schema(schema.clone())
            .build()
            .expect("build succeeds");

        let info = agent.handle().info().clone();
        assert_eq!(info.agent_id, id);
        assert_eq!(info.model, "resolved-model", "model override wins");
        assert_eq!(info.profile_name.as_deref(), Some("reviewer"));
        assert_eq!(info.working_dir, temp.path());
        assert_eq!(info.output_schema.as_ref(), Some(&schema));
        assert!(!info.session_id.is_empty(), "session id always resolved");
        let mut tools = info.tool_names.clone();
        tools.sort();
        assert_eq!(tools, vec!["read".to_owned(), "search".to_owned()]);
        // The snapshot is serializable for activity records / telemetry.
        let json = serde_json::to_value(&info).expect("info serializes");
        assert_eq!(json["model"], "resolved-model");
        assert_eq!(json["output_schema"], schema);
        // Agent-side accessors agree with the handle.
        assert_eq!(agent.info().model, info.model);
        assert_eq!(agent.agent_id(), id);
    }

    #[test]
    fn default_profile_yields_no_profile_name() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        assert_eq!(agent.info().profile_name, None);
    }

    /// Cancellation through the handle: no caller-supplied token needed —
    /// the builder mints one and the handle controls it.
    #[tokio::test]
    async fn handle_cancel_stops_run_with_cancelled_reason() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        let handle = agent.handle();
        assert!(!handle.cancellation_token().is_cancelled());
        handle.cancel();
        let outcome = agent
            .run("go")
            .await
            .expect("cancelled run returns Ok(Stopped)");
        assert_eq!(outcome.stop_reason(), Some(&AgentStopReason::Cancelled));
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
                Ok(ToolOutput::success(Value::Null))
            }
        }

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
        let outcome = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .cancel_token(token)
            .run("go")
            .await
            .expect("cancelled run returns Ok(Stopped) with a Cancelled reason");
        assert!(!outcome.is_completed());
        assert_eq!(outcome.stop_reason(), Some(&AgentStopReason::Cancelled));
        // The Stopped arm's partial payload genuinely carries the run's
        // session state — the event store is handed back exactly as on
        // the Completed arm, so a stopped run remains resumable.
        assert!(
            outcome.output().event_store.is_some(),
            "stopped run must hand the event store back on the partial payload"
        );
    }

    #[tokio::test]
    async fn session_resume_accumulates_events() {
        let first = AgentBuilder::new(provider_with(text_completion("first")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .run("question one")
            .await
            .expect("first run succeeds");
        let store = first
            .into_output()
            .event_store
            .expect("event store returned");
        let after_first = store.events().len();
        assert!(after_first > 0, "first run records events");

        let second = AgentBuilder::new(provider_with(text_completion("second")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .session(store)
            .run("question two")
            .await
            .expect("resumed run succeeds");
        let store = second
            .into_output()
            .event_store
            .expect("event store returned");
        assert!(
            store.events().len() > after_first,
            "resumed run appends onto the prior session's events",
        );
    }

    #[tokio::test]
    async fn agent_registry_wires_fork_spawn_infra() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(256)
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
                !err.to_string().contains("AgentToolInfra"),
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(256)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(Arc::clone(&agent_registry))
            .child_policy(test_child_policy())
            .child_result_capacity(256)
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

        // Spawned children are wakeable actors: after result delivery the
        // registry entry and parent-held handle remain so signal_agent can
        // queue work and wake_agent can resume the child explicitly.
        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("AgentHandles installed");
        assert!(agent_registry.read().get(child_id).is_some());
        assert!(handles.contains(child_id));
    }

    /// Track B finding 1 (blocker): `workspace_root` must produce a built
    /// agent whose context denies out-of-root file access through a real
    /// tool call — previously `confine_to_workspace` had zero production
    /// callers, so the control could never be switched on.
    #[tokio::test]
    async fn workspace_root_confines_file_tools_through_built_context() {
        use crate::tool::envelope::ToolEnvelope;

        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir ws");
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "outside the workspace").expect("write secret");
        let inside = root.join("inside.txt");
        std::fs::write(&inside, "inside the workspace").expect("write inside");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            metadata: Value::Null,
        };

        let denied = tool
            .execute(&read_envelope(&secret), ctx.as_ref())
            .await
            .expect("confinement refusal is a structured tool error");
        assert!(denied.is_error(), "out-of-root read must be refused");
        assert!(
            denied.content["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("refused")),
            "refusal must be explicit: {}",
            denied.content,
        );
        assert_eq!(
            denied.content["error"]["kind"], "permission_denied",
            "confinement refusal carries the typed kind",
        );

        let allowed = tool
            .execute(&read_envelope(&inside), ctx.as_ref())
            .await
            .expect("in-root read executes");
        assert!(!allowed.is_error(), "in-root read must succeed");
    }

    /// Finding 1 companion: without `workspace_root` the built context
    /// stays unconfined — the historical embedder behaviour.
    #[tokio::test]
    async fn builder_without_workspace_root_stays_unconfined() {
        use crate::tool::envelope::ToolEnvelope;

        let outer = tempfile::tempdir().expect("tempdir");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir ws");
        let elsewhere = outer.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, "reachable").expect("write file");

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
                    metadata: Value::Null,
                },
                ctx.as_ref(),
            )
            .await
            .expect("unconfined read executes");
        assert!(!out.is_error(), "unconfined context reads anywhere");
    }

    /// Finding 1: a `workspace_root` that does not exist fails the build
    /// with a typed configuration error instead of confining nothing.
    #[test]
    fn workspace_root_must_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .load_runtime_base()
            .agent_config(AgentLoopConfig {
                context_window_limit: Some(200_000),
                auto_compact_reserve_tokens: Some(30_000),
                ..AgentLoopConfig::default()
            })
            .build()
            .expect("build succeeds");

        assert_eq!(agent.config.context_window_limit, Some(200_000));
        assert_eq!(agent.config.auto_compact_reserve_tokens, Some(30_000));
        assert!(
            agent
                .loop_context
                .base_system_instruction()
                .contains("automatically summarised or cleared"),
            "compaction guidance must be in the system prompt when the \
             effective config enables auto-compaction",
        );
    }

    /// Companion to the finding 3 regression: with compaction disabled
    /// (reserve off), the guidance must stay out of the system prompt.
    /// A windowless build is no longer the way to express "no compaction"
    /// — that now hard-errors (2026-07-05 incident guard) — so the honest
    /// no-compaction state is an armed window with the reserve disabled.
    #[test]
    fn no_auto_compact_guidance_without_compaction_config() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(std::env::temp_dir())
            .agent_config(AgentLoopConfig {
                context_window_limit: Some(TEST_CONTEXT_WINDOW),
                auto_compact_reserve_tokens: None,
                ..AgentLoopConfig::default()
            })
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

    /// Finding 5 regression: a reserve at or above the window makes the
    /// runtime trigger disable itself (every step would fire), so the build
    /// must not emit compaction guidance the loop will never honour. Both
    /// values are known at build time, so `has_auto_compact` is forced false
    /// even though both fields are `Some`.
    #[test]
    fn reserve_at_or_above_window_drops_auto_compact_guidance() {
        for (window, reserve) in [(50_000_u64, 50_000_u64), (50_000, 60_000)] {
            let temp = tempfile::tempdir().expect("tempdir");
            let agent = AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .working_dir(temp.path())
                .agent_config(AgentLoopConfig {
                    context_window_limit: Some(window),
                    auto_compact_reserve_tokens: Some(reserve),
                    ..AgentLoopConfig::default()
                })
                .build()
                .expect("build succeeds");

            // The config values themselves are preserved verbatim — the build
            // only suppresses the *prompt guidance*, matching the runtime
            // trigger which reads these same values and disables.
            assert_eq!(agent.config.context_window_limit, Some(window));
            assert_eq!(agent.config.auto_compact_reserve_tokens, Some(reserve));
            assert!(
                !agent
                    .loop_context
                    .base_system_instruction()
                    .contains("automatically summarised or cleared"),
                "reserve {reserve} >= window {window} must drop the compaction guidance \
                 (the runtime trigger disables in this shape)",
            );
        }
    }

    /// Reserve-armed default: a catalogued model with no explicit window or
    /// reserve must default its context window from the model catalog and
    /// keep the reserve knob armed, so auto-compaction is on by default.
    #[test]
    fn catalog_arms_window_and_reserve_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("gpt-5.5")
            .working_dir(temp.path())
            .build()
            .expect("build succeeds");

        let catalog_window = crate::model_catalog::smallest_context_window_for_model("gpt-5.5");
        assert!(catalog_window.is_some(), "gpt-5.5 must be in the catalog");
        assert_eq!(
            agent.config.context_window_limit, catalog_window,
            "an unset window must default to the model catalog value",
        );
        assert_eq!(
            agent.config.auto_compact_reserve_tokens,
            Some(30_000),
            "the reserve knob is armed by default",
        );
        assert!(
            agent
                .loop_context
                .base_system_instruction()
                .contains("automatically summarised or cleared"),
            "a catalog-armed window plus the default reserve enables \
             auto-compaction, so the guidance must be present",
        );
    }

    /// An explicitly configured window — even for a catalogued model — must
    /// win over the catalog fill-in (the catalog only fills an unset value).
    #[test]
    fn explicit_window_beats_catalog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("gpt-5.5")
            .working_dir(temp.path())
            .agent_config(AgentLoopConfig {
                context_window_limit: Some(50_000),
                ..AgentLoopConfig::default()
            })
            .build()
            .expect("build succeeds");

        assert_eq!(
            agent.config.context_window_limit,
            Some(50_000),
            "an explicit window must not be overwritten by the catalog",
        );
    }

    /// 2026-07-05 incident guard (owner-ruled): a model absent from the
    /// catalog with no explicit window is rejected at build — running with
    /// the protections silently disabled is the ruled-against state, and
    /// an unknown model "probably means the wrong model code".
    #[test]
    fn unknown_model_without_window_is_rejected_at_build() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("not-in-catalog")
            .working_dir(temp.path())
            .build();
        let Err(err) = result else {
            panic!("uncatalogued model with no window must be rejected");
        };

        let reason = err.to_string();
        assert!(
            reason.contains("not-in-catalog"),
            "names the model: {reason}"
        );
        assert!(
            reason.contains("typo"),
            "leads with the typo hypothesis: {reason}"
        );

        // The explicit-window escape hatch assembles fine.
        let temp = tempfile::tempdir().expect("tempdir");
        AgentBuilder::new(provider_with(vec![]))
            .model("not-in-catalog")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .build()
            .expect("explicit window arms an uncatalogued model");
    }

    /// The 2026-07-05 incident shape through the real assembly funnel: a
    /// catalogued 128k model with an explicit 272k window must fail
    /// `build()` itself — this pins the guard staying wired into build,
    /// not just its unit logic in `arming`.
    #[test]
    fn oversized_explicit_window_is_rejected_at_build() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("gpt-5.3-codex-spark")
            .context_window_limit(272_000)
            .working_dir(temp.path())
            .build();
        let Err(err) = result else {
            panic!("explicit window above the catalog maximum must be rejected");
        };

        let reason = err.to_string();
        assert!(
            reason.contains("gpt-5.3-codex-spark"),
            "names the model: {reason}"
        );
        assert!(
            reason.contains("272000"),
            "names the configured value: {reason}"
        );
        assert!(
            reason.contains("128000"),
            "names the catalog maximum: {reason}"
        );
    }

    /// The catalog fill-in resolves each agent's own model — a sub-agent
    /// built with a different catalogued model gets that model's window, not
    /// a shared or parent value.
    #[test]
    fn catalog_window_is_resolved_per_model() {
        let temp = tempfile::tempdir().expect("tempdir");
        let big = AgentBuilder::new(provider_with(vec![]))
            .model("gpt-5.5")
            .working_dir(temp.path())
            .build()
            .expect("build succeeds");
        let small = AgentBuilder::new(provider_with(vec![]))
            .model("gpt-5.3-codex-spark")
            .working_dir(temp.path())
            .build()
            .expect("build succeeds");

        assert_eq!(
            big.config.context_window_limit,
            crate::model_catalog::smallest_context_window_for_model("gpt-5.5"),
        );
        assert_eq!(
            small.config.context_window_limit,
            crate::model_catalog::smallest_context_window_for_model("gpt-5.3-codex-spark"),
        );
        assert_ne!(
            big.config.context_window_limit, small.config.context_window_limit,
            "each agent resolves its own model's catalog window",
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .load_runtime_base()
            .run("run a command")
            .await
            .expect("run completes");

        let store = output
            .into_output()
            .event_store
            .expect("event store returned");
        let blocked = store.events().iter().any(|event| {
            matches!(
                event,
                SessionEvent::ToolResult { tool_name, output, .. }
                    if tool_name == "bash"
                        // Permission denials persist as the typed
                        // `permission_denied` payload, not a collapsed
                        // string.
                        && output["error"]["kind"] == "permission_denied"
                        && output["error"]["message"]
                            .as_str()
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
                reasoning: Vec::new(),
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
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .session(Arc::new(store))
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

    /// NO ASSUMED DEFAULTS: with neither a profile model nor an explicit
    /// `.model(..)`, the build must fail with a typed error that tells the
    /// embedder exactly what to set — never fall back to a hardcoded model.
    #[test]
    fn build_without_profile_or_model_is_a_typed_error() {
        let result = AgentBuilder::new(provider_with(vec![]))
            .working_dir(std::env::temp_dir())
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("no model resolved"), "{reason}");
                assert!(reason.contains(".model("), "{reason}");
                assert!(reason.contains(".profile"), "{reason}");
            }
            Err(other) => panic!("expected a typed config error, got: {other}"),
            Ok(_) => panic!("a build with no model must fail, not assume one"),
        }
    }

    /// ITEM C: the output schema lives on the agent-loop config, so it
    /// round-trips through serde with the rest of the config — the
    /// serialized form embedders carry across activity boundaries.
    #[test]
    fn output_schema_round_trips_through_serialized_loop_config() {
        let schema = serde_json::json!({"type": "object", "required": ["verdict"]});
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .output_schema(schema.clone())
            .build()
            .expect("build succeeds");
        assert_eq!(
            agent.loop_config().output_schema.as_ref(),
            Some(&schema),
            "the effective config is introspectable through the public accessor",
        );

        let json = serde_json::to_string(agent.loop_config()).expect("config serializes");
        let back: AgentLoopConfig = serde_json::from_str(&json).expect("config deserializes");
        assert_eq!(back.output_schema.as_ref(), Some(&schema));
        assert_eq!(back.schema_tool_name, agent.config.schema_tool_name);

        // Partial JSON deserializes with defaults — the activity-input shape.
        let partial: AgentLoopConfig =
            serde_json::from_str(r#"{"output_schema": {"type": "object"}}"#)
                .expect("partial config deserializes");
        assert_eq!(
            partial.output_schema,
            Some(serde_json::json!({"type": "object"}))
        );
        assert_eq!(
            partial.schema_attempt_budget,
            AgentLoopConfig::default().schema_attempt_budget
        );
    }

    /// A runtime-base config merges with the explicit schema: the schema
    /// is part of the effective config, exactly like every other field.
    #[test]
    fn output_schema_survives_runtime_base_merge() {
        let temp = tempfile::tempdir().expect("tempdir");
        let schema = serde_json::json!({"type": "string"});
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .load_runtime_base()
            .output_schema(schema.clone())
            .build()
            .expect("build succeeds");
        assert_eq!(agent.config.output_schema.as_ref(), Some(&schema));
        assert_eq!(agent.info().output_schema.as_ref(), Some(&schema));
        assert!(
            agent
                .loop_context
                .base_system_instruction()
                .contains("structured"),
            "schema mode must reach the system prompt through the effective config",
        );
    }

    #[test]
    fn context_window_limit_setter_survives_runtime_base_merge() {
        // C8: the granular setter overrides only this field, overriding the
        // settings-derived runtime base per the explicit-config-wins rule.
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .load_runtime_base()
            .context_window_limit(4_242)
            .build()
            .expect("build succeeds");
        assert_eq!(agent.config.context_window_limit, Some(4_242));
    }

    #[test]
    fn context_window_limit_setter_beats_auto_compaction_catalog_fill() {
        // C8 interaction: the setter's window survives auto-compaction arming,
        // which fills the catalog window only when the merged value is still
        // `None`. A catalogued model would otherwise have its window filled
        // from the catalog during build(); the explicit setter value wins.
        let temp = tempfile::tempdir().expect("tempdir");
        let model = crate::model_catalog::default_selection().model;
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model(model)
            .working_dir(temp.path())
            .load_runtime_base()
            .context_window_limit(4_242)
            .build()
            .expect("build succeeds");
        assert_eq!(
            agent.config.context_window_limit,
            Some(4_242),
            "the setter value must survive catalog fill during arming",
        );
    }

    // -- open_session: the managed persisted-session path -------------------

    fn manager_in(dir: &std::path::Path) -> SessionManager {
        SessionManager::new(dir)
    }

    /// `open_session(Create)` wires the persisted session end to end:
    /// the index entry records the resolved model and working dir, the
    /// entry id becomes the cache key, the environment session id, and
    /// the introspected session id, and a run's events persist to disk.
    #[tokio::test]
    async fn open_session_create_wires_store_cache_key_and_session_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());

        let agent = AgentBuilder::new(provider_with(text_completion("persisted")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .open_session(
                &manager,
                SessionSpec::Create {
                    name: Some("track-h".to_owned()),
                },
                DurabilityPolicy::Flush,
            )
            .build()
            .expect("build succeeds");

        let entry = agent
            .session_entry()
            .expect("opened session entry surfaced")
            .clone();
        assert_eq!(entry.model, "test-model", "entry records resolved model");
        assert_eq!(
            entry.working_dir,
            temp.path().display().to_string(),
            "entry records resolved working dir",
        );
        assert_eq!(entry.name.as_deref(), Some("track-h"));
        assert_eq!(agent.config.cache_key.as_deref(), Some(entry.id.as_str()));
        assert_eq!(agent.info().session_id, entry.id);
        assert_eq!(
            agent
                .loop_context
                .environment
                .as_ref()
                .and_then(|env| env.session_id.as_deref()),
            Some(entry.id.as_str()),
            "the system prompt environment carries the persisted session id",
        );
        assert_eq!(
            agent.session_replay(),
            Some(crate::session::ReplaySummary::default()),
            "a fresh create replays nothing",
        );

        let outcome = agent.run("persist me").await.expect("run succeeds");
        assert!(outcome.is_completed());

        // The run's events landed in the managed session on disk.
        let (_, read) = manager.read_events(&entry.id).expect("session readable");
        assert!(
            !read.events.is_empty(),
            "run events must persist through the managed sink",
        );
    }

    /// Child-persistence V2: `open_session` arms the root's persistent
    /// [`crate::session::SessionBinding`] on the coordination infra —
    /// the single allocation authority every spawn/fork child mint
    /// routes through — keyed to the opened session's id. Without
    /// `open_session` (in-memory `.session(..)` or nothing) the binding
    /// is the deliberate ephemeral root.
    #[tokio::test]
    async fn open_session_installs_persistent_branching_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());

        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .open_session(
                &manager,
                SessionSpec::Create { name: None },
                DurabilityPolicy::Flush,
            )
            .build()
            .expect("build succeeds");
        let entry_id = agent.session_entry().expect("entry").id.clone();
        let infra = agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::tools::agent::AgentToolInfra>()
            .expect("coordination infra installed");
        assert!(
            infra.session.is_persistent(),
            "an opened session must arm a persistent branching binding",
        );
        assert_eq!(
            infra.session.session_id(),
            Some(entry_id.as_str()),
            "the binding is keyed to the opened session",
        );
        assert_eq!(infra.session.path_address(), "root");

        // No open_session: the binding is the deliberate ephemeral root.
        let ephemeral = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .build()
            .expect("build succeeds");
        let infra = ephemeral
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<crate::tools::agent::AgentToolInfra>()
            .expect("coordination infra installed");
        assert!(
            !infra.session.is_persistent(),
            "without a persisted session the binding is honestly ephemeral",
        );
    }

    /// Gap 14 closure: the `RunOutcome` payload carries the LIVE
    /// sink-equipped store `Arc` — appends made by the embedder AFTER
    /// the run still write through to disk, even with the fork/spawn
    /// coordination infra installed (the `Arc` cycle that used to force
    /// a silent sink-less snapshot).
    #[tokio::test]
    async fn run_outcome_store_keeps_persisting_after_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());

        let outcome = AgentBuilder::new(provider_with(text_completion("done")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            // Coordination infra installed: this is exactly the shape
            // whose Arc cycle used to trigger the snapshot fallback.
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .open_session(
                &manager,
                SessionSpec::Create { name: None },
                DurabilityPolicy::Flush,
            )
            .run("first turn")
            .await
            .expect("run succeeds");
        let store = outcome
            .into_output()
            .event_store
            .expect("event store returned");

        // Post-run embedder append: must reach the SAME on-disk session.
        use crate::session::events::{EventBase, SessionEvent};
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: "appended after the run".to_owned(),
            })
            .expect("post-run append accepted");

        let index = crate::session::read_index(sessions.path()).expect("index");
        let entry = &index[0];
        let read = crate::session::read_session_events(sessions.path(), &entry.id)
            .expect("session readable");
        assert!(
            read.events.iter().any(|e| matches!(
                e,
                SessionEvent::UserMessage { content, .. }
                    if content == "appended after the run"
            )),
            "a post-run append must still write through to disk — the \
             returned store carries the live sink, never a snapshot",
        );
    }

    /// `open_session(OpenOrResume)` with the same deterministic id
    /// resumes the prior run's history — the retry-safe activity path.
    #[tokio::test]
    async fn open_session_open_or_resume_continues_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());
        let spec = || SessionSpec::OpenOrResume {
            id: "wf-7.step-2".to_owned(),
        };

        let first = AgentBuilder::new(provider_with(text_completion("first")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .open_session(&manager, spec(), DurabilityPolicy::Flush)
            .build()
            .expect("first build succeeds");
        assert_eq!(first.info().session_id, "wf-7.step-2");
        let outcome = first.run("attempt one").await.expect("first run succeeds");
        assert!(outcome.is_completed());

        let retry = AgentBuilder::new(provider_with(text_completion("second")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .open_session(&manager, spec(), DurabilityPolicy::Flush)
            .build()
            .expect("retry build succeeds");
        let replay = retry.session_replay().expect("resume surfaced replay");
        assert!(
            replay.replayed_events > 0,
            "the retry must replay the first attempt's history",
        );
        assert_eq!(
            manager.list().expect("index readable").len(),
            1,
            "one deterministic id, one session",
        );
    }

    #[test]
    fn open_session_conflicts_with_explicit_session_store() {
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .session(Arc::new(EventStore::new()))
            .open_session(
                &manager,
                SessionSpec::Create { name: None },
                DurabilityPolicy::Flush,
            )
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("open_session"), "{reason}");
            }
            Err(other) => panic!("expected a typed config error, got: {other}"),
            Ok(_) => panic!("session + open_session must fail the build"),
        }
    }

    #[test]
    fn open_session_conflicts_with_explicit_cache_key() {
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_config(AgentLoopConfig {
                cache_key: Some("explicit-key".to_owned()),
                ..AgentLoopConfig::default()
            })
            .open_session(
                &manager,
                SessionSpec::Create { name: None },
                DurabilityPolicy::Flush,
            )
            .build();
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("cache_key"), "{reason}");
            }
            Err(other) => panic!("expected a typed config error, got: {other}"),
            Ok(_) => panic!("open_session + explicit cache_key must fail the build"),
        }
    }

    /// A failed open (e.g. resuming a session that does not exist) is a
    /// typed build error — never a silent fresh session.
    #[test]
    fn open_session_resume_of_missing_session_fails_build() {
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());
        let result = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .open_session(
                &manager,
                SessionSpec::Resume {
                    id_or_name: "does-not-exist".to_owned(),
                },
                DurabilityPolicy::Flush,
            )
            .build();
        match result {
            Err(NornError::Session(_)) => {}
            Err(other) => panic!("expected a session error, got: {other}"),
            Ok(_) => panic!("resuming a missing session must fail the build"),
        }
    }

    /// `open_session(Fork)` copies the source history into a new session
    /// and the agent runs against the fork, leaving the source untouched.
    #[tokio::test]
    async fn open_session_fork_runs_against_forked_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = tempfile::tempdir().expect("session dir");
        let manager = manager_in(sessions.path());

        let source = AgentBuilder::new(provider_with(text_completion("origin")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .open_session(
                &manager,
                SessionSpec::Create {
                    name: Some("source".to_owned()),
                },
                DurabilityPolicy::Flush,
            )
            .build()
            .expect("source build succeeds");
        let source_id = source.session_entry().expect("source entry").id.clone();
        let outcome = source.run("seed history").await.expect("source run");
        assert!(outcome.is_completed());
        let (_, source_read) = manager.read_events(&source_id).expect("source readable");
        let source_len = source_read.events.len();

        let fork = AgentBuilder::new(provider_with(text_completion("forked")))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(temp.path())
            .open_session(
                &manager,
                SessionSpec::Fork {
                    source: source_id.clone(),
                    name: Some("fork".to_owned()),
                },
                DurabilityPolicy::Flush,
            )
            .build()
            .expect("fork build succeeds");
        let fork_entry = fork.session_entry().expect("fork entry").clone();
        assert_ne!(fork_entry.id, source_id);
        let replay = fork.session_replay().expect("fork replay");
        assert_eq!(
            replay.replayed_events,
            source_len + 1,
            "fork replays the copied events plus the fork marker",
        );
        let outcome = fork.run("continue on fork").await.expect("fork run");
        assert!(outcome.is_completed());

        let (_, source_after) = manager.read_events(&source_id).expect("source readable");
        assert_eq!(
            source_after.events.len(),
            source_len,
            "the fork's run must not touch the source session",
        );
    }

    /// Read the catalog description `tool_search` reports for `web_search`
    /// through the built agent's live tool context.
    async fn catalog_web_search_description(agent: &Agent) -> String {
        use crate::tool::envelope::ToolEnvelope;

        let ctx = agent
            .registry
            .shared_context()
            .expect("registry exposes its shared tool context");
        let tool = agent
            .registry
            .get("tool_search")
            .expect("tool_search registered");
        let envelope = ToolEnvelope {
            tool_call_id: "surface-test".to_string(),
            tool_name: "tool_search".to_string(),
            model_args: serde_json::json!({"query": "", "max_results": 500}),
            metadata: Value::Null,
        };
        let out = tool
            .execute(&envelope, ctx.as_ref())
            .await
            .expect("tool_search runs through the built context");
        out.content["results"]
            .as_array()
            .expect("results array")
            .iter()
            .find(|result| result["name"] == "web_search")
            .expect("web_search entry present in catalog dump")["description"]
            .as_str()
            .expect("description is a string")
            .to_owned()
    }

    /// The same registry resolved against two capability sets — hosted web
    /// search on and off — must flip all three projections of the resolved
    /// tool surface: the system-prompt tools section, the `tool_search`
    /// catalog, and the provider request definitions. The second build is a
    /// resume-style rebuild (`.session(store)` from the first run), proving
    /// a provider change between resumes re-resolves the whole surface and
    /// nothing stale is carried over.
    #[tokio::test]
    async fn provider_capability_switch_flips_all_three_projections_across_resume() {
        use crate::provider::mock::MockProvider;
        use crate::provider::tools::{
            HostedToolDefinition, ProviderCapabilities, ProviderToolDefinition,
        };

        // --- Hosted-capable provider: every projection shows the hosted truth.
        let hosted_provider = Arc::new(MockProvider::with_capabilities(
            text_completion("first"),
            ProviderCapabilities {
                hosted_web_search: true,
                ..ProviderCapabilities::default()
            },
        ));
        let agent = AgentBuilder::new(Arc::clone(&hosted_provider) as Arc<dyn Provider>)
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("hosted build succeeds");

        let prompt = agent.loop_context.base_system_instruction();
        assert!(
            prompt.contains("not a callable function"),
            "hosted prompt must carry the provider truth for web_search",
        );
        assert!(
            !prompt.contains("Search the public web"),
            "the function-mode description must not survive hosted reframing",
        );

        let description = catalog_web_search_description(&agent).await;
        assert!(
            description.contains("not a callable function"),
            "hosted catalog entry must carry the provider truth: {description}",
        );

        let outcome = agent.run("go").await.expect("hosted run succeeds");
        let requests = hosted_provider.requests().expect("requests recorded");
        let wire = &requests[0].tools;
        assert!(
            wire.iter().any(|tool| matches!(
                tool,
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_))
            )),
            "hosted provider must receive the hosted web-search tool",
        );
        assert!(
            !wire.iter().any(|tool| matches!(
                tool,
                ProviderToolDefinition::Function(function) if function.name == "web_search"
            )),
            "the web_search function definition must not also be sent",
        );

        // --- Resume-style rebuild against a provider WITHOUT hosted search:
        // every projection flips back to the callable-function truth.
        let store = outcome
            .into_output()
            .event_store
            .expect("event store returned");
        let plain_provider = Arc::new(MockProvider::new(text_completion("second")));
        let agent = AgentBuilder::new(Arc::clone(&plain_provider) as Arc<dyn Provider>)
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .session(store)
            .build()
            .expect("resumed build succeeds");

        let prompt = agent.loop_context.base_system_instruction();
        assert!(
            prompt.contains("Search the public web"),
            "function-mode prompt must list web_search as a callable function",
        );
        assert!(
            !prompt.contains("not a callable function"),
            "no hosted framing may leak across the provider switch",
        );

        let description = catalog_web_search_description(&agent).await;
        assert!(
            description.contains("Search the public web"),
            "function-mode catalog entry must keep the function description: {description}",
        );
        assert!(!description.contains("not a callable function"));

        let outcome = agent.run("again").await.expect("resumed run succeeds");
        assert!(outcome.is_completed());
        let requests = plain_provider.requests().expect("requests recorded");
        let wire = &requests[0].tools;
        assert!(
            wire.iter().any(|tool| matches!(
                tool,
                ProviderToolDefinition::Function(function) if function.name == "web_search"
            )),
            "without the capability web_search is sent as a function tool",
        );
        assert!(
            !wire.iter().any(|tool| matches!(
                tool,
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_))
            )),
            "no hosted definition may be sent without the capability",
        );
    }

    /// `.event_schemas(..)` installs the set on the built loop context.
    #[test]
    fn event_schemas_setter_installs_on_loop_context() {
        use crate::agent_loop::event_schemas::{EventSchemaSet, EventType};

        let mut schemas = EventSchemaSet::new();
        schemas.set(EventType::Text, serde_json::json!({"type": "string"}));
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .event_schemas(schemas)
            .build()
            .expect("build succeeds");
        let installed = agent
            .loop_context
            .event_schemas
            .as_ref()
            .expect("event schemas installed on the loop context");
        assert_eq!(
            installed.get(EventType::Text),
            Some(&serde_json::json!({"type": "string"})),
        );
    }

    /// `.variables(store)` overrides the minted store, and with no
    /// `open_session` the supplied store's id becomes the resolved session
    /// id so `{{session_id}}` and the environment agree.
    #[test]
    fn variables_setter_overrides_store_and_pins_session_id() {
        use crate::integration::variables::VariableStore;

        let store = Arc::new(VariableStore::with_builtins().with_session_id("custom-session"));
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .variables(Arc::clone(&store))
            .build()
            .expect("build succeeds");
        let installed = agent
            .loop_context
            .variables
            .as_ref()
            .expect("variable store installed on the loop context");
        assert!(
            Arc::ptr_eq(installed, &store),
            "the supplied store overrides the minted one",
        );
        assert_eq!(
            agent.info().session_id,
            "custom-session",
            "the supplied store's id becomes the resolved session id",
        );
    }

    /// A supplied variable store whose id contradicts an `open_session`
    /// persisted id fails the build rather than silently diverging.
    #[test]
    fn variables_setter_conflicting_session_id_fails_build() {
        use crate::integration::variables::VariableStore;

        let temp = tempfile::tempdir().expect("tempdir");
        let manager = SessionManager::new(temp.path());
        let store = Arc::new(VariableStore::with_builtins().with_session_id("not-the-session"));
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .working_dir(temp.path())
                .open_session(
                    &manager,
                    SessionSpec::Create { name: None },
                    DurabilityPolicy::Flush,
                )
                .variables(store)
                .build(),
        );
        assert!(
            reason.contains("disagrees with the resolved session id"),
            "{reason}"
        );
    }

    /// `.disallowed_tools(..)` gates the registry with deny-wins semantics.
    #[test]
    fn disallowed_tools_setter_denies_named_tools() {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .allowed_tools(&["read", "bash"])
            .disallowed_tools(&["bash"])
            .build()
            .expect("build succeeds");
        assert!(
            agent.registry.get("bash").is_none(),
            "deny wins even when the allow-list names the tool",
        );
        assert!(agent.registry.get("read").is_some());
    }

    /// `.terminal_reclamation(false)` suppresses the reclamation marker the
    /// coordination runtime installs by default.
    #[test]
    fn terminal_reclamation_setter_gates_reclaim_marker() {
        use crate::tools::agent::ReclaimOnResultDelivery;

        let default_agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .build()
            .expect("build succeeds");
        assert!(
            default_agent
                .registry
                .shared_context()
                .expect("shared tool context")
                .get_extension::<ReclaimOnResultDelivery>()
                .is_some(),
            "terminal reclamation is installed by default (headless)",
        );

        let tui_agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .terminal_reclamation(false)
            .build()
            .expect("build succeeds");
        assert!(
            tui_agent
                .registry
                .shared_context()
                .expect("shared tool context")
                .get_extension::<ReclaimOnResultDelivery>()
                .is_none(),
            "terminal_reclamation(false) suppresses the marker for status-panel drivers",
        );
    }

    /// `.register_root(..)` reserves the registry entry and adopts the
    /// reserved id as the agent's id.
    #[test]
    fn register_root_setter_reserves_and_adopts_id() {
        let registry = AgentRegistry::shared();
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(Arc::clone(&registry))
            .child_policy(test_child_policy())
            .child_result_capacity(16)
            .register_root("/root".to_string(), "lead".to_string())
            .build()
            .expect("build succeeds");
        let entry = registry
            .read()
            .get_by_path("/root")
            .expect("root entry reserved and confirmed");
        assert_eq!(
            entry.id,
            agent.agent_id(),
            "the agent adopts the reserved root id",
        );
    }

    /// `.register_root(..)` without `.agent_registry(..)` fails the build —
    /// the entry would be silently unregistered.
    #[test]
    fn register_root_without_registry_fails_build() {
        let reason = invalid_config_reason(
            AgentBuilder::new(provider_with(vec![]))
                .model("test-model")
                .context_window_limit(TEST_CONTEXT_WINDOW)
                .working_dir(std::env::temp_dir())
                .register_root("/root".to_string(), "lead".to_string())
                .build(),
        );
        assert!(
            reason.contains("register_root is set but agent coordination is not wired"),
            "{reason}"
        );
    }
}
