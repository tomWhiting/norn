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

mod init;

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
    populate_loop_context, resolve_base_profile, resolve_runtime_overlay, resolve_working_dir,
    restore_session_state, validate_workspace_root,
};
use crate::agent::build_support::{
    compute_read_exempt_roots, resolve_coordination, resolve_root_agent_id, validate_build_inputs,
};
use crate::agent::child_policy::ChildPolicy;
use crate::agent::handle::ResolvedAgentInfo;
use crate::agent::instance::Agent;
use crate::agent::mcp::McpAttachment;
use crate::agent::output::RunOutcome;
use crate::agent::prompt_install::{SystemPromptInstall, install_system_prompt};
use crate::agent::registry::AgentRegistry;
use crate::agent::registry_assembly::build_base_tool_registry;
use crate::agent::session_spec::SessionRequest;
use crate::agent_loop::config::{AgentLoopConfig, ToolExecutor};
use crate::agent_loop::event_schemas::EventSchemaSet;
use crate::agent_loop::inbound::{InboundChannel, InboundSender};
use crate::agent_loop::retry::RetryPolicy;
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::integration::variables::{SessionVariable, VariableSource, VariableStore};
use crate::profile::{Capability, Profile, ProfileOrigin, from_profile};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::provider::traits::Provider;
use crate::provider::{AgentEvent, AgentEventSender, SharedAgentEventChannel};
use crate::rules::engine::RuleEngine;
use crate::session::{manager::ReplaySummary, store::EventStore};
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
    /// - [`NornError::Provider`] when a threaded provider has no stable
    ///   state identity, or a managed session is bound to another identity.
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

        let resolved_profile = resolve_base_profile(
            self.profile.take(),
            self.profile_origin,
            self.profile_name.as_deref(),
            &working_dir,
        )?;
        let profile_prompt_source = resolved_profile.prompt_source;
        let mut profile = resolved_profile.profile;
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
            let base = crate::runtime_init::base::load_runtime_base_at_launch_root(
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
        let (provider_capabilities, provider_state_identity) =
            crate::agent::session_open::provider_authority(self.provider.as_ref())?;

        // Open the managed persisted session now that the model and
        // working directory are resolved (see `session_open` for the full
        // contract). The root's session-branching identity
        // (child-persistence V2): a disk-persisted session yields a
        // persistent binding — the single allocation authority every
        // spawn/fork/rhai child mint under this agent routes through —
        // while the in-memory `.session(store)` path and the no-session
        // default stay deliberately ephemeral (the `--no-session` honesty
        // axis: children then run memory-only, with the typed
        // `session: None` branch event on the parent timeline).
        let mut opened_session: Option<(crate::session::SessionIndexEntry, ReplaySummary)> = None;
        let mut session_binding = Arc::new(crate::session::SessionBinding::ephemeral_root());
        let mut artifact_store = None;
        if let Some(request) = self.session_request.take() {
            let opened = crate::agent::session_open::open_root_session(
                request,
                &model,
                &working_dir.display().to_string(),
                provider_state_identity,
            )?;
            session_binding = opened.binding;
            artifact_store = Some(opened.artifacts);
            self.session = Some(opened.store);
            opened_session = Some((opened.entry, opened.replay));
        }

        let lsp_backend = self.lsp_backend.clone();
        let mut registry = build_base_tool_registry(
            lsp_backend.clone(),
            self.extra_tools,
            &self.without_tools,
            self.bash_drain_grace,
        );
        self.mcp.register_tools(&mut registry)?;
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
        self.mcp.restrict_tools(&mut registry)?;

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
                profile_source: profile_prompt_source,
                has_auto_compact,
                capabilities: provider_capabilities,
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
            artifact_store,
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
            )?;
        }
        // Every agent's context carries its OWN base system instruction
        // (R5): `fork` reads this extension to hand a fork child the
        // forker's context, so the root publishes its installed base here
        // — the previously designed-never-wired publish end. The root's
        // launch model rides alongside as the parent-model ground truth
        // for spawns that omit `model` (an unregistered root has no
        // agent-registry entry to read it from).
        crate::agent::arming::publish_parent_execution_context(
            &registry,
            shared.as_ref(),
            &loop_context,
            &model,
        );
        let (registry, tool_runtime) = self.mcp.assemble(&working_dir, registry, &shared)?;
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
            tool_runtime,
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
mod tests;
