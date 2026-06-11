//! Assembly-phase helpers for [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! These functions are the cohesive build phases extracted from
//! `agent/builder.rs`: workspace-root validation, runtime-base overlay
//! resolution (diagnostics, rules, hooks, agent-loop config), system-prompt
//! construction, hook-registry finishing (diagnostic stop hook), tool-catalog
//! / tool-definition projection, fork-spawn infrastructure wiring, and the
//! event-store snapshot used when an `Arc` cycle prevents reclaiming the
//! store. They carry no builder state — every input is explicit — so each
//! phase is unit-testable on its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agent::mailbox::Mailbox;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::{
    CHILD_RESULT_CHANNEL_CAPACITY, ChildAgentResult, ChildResultSender,
};
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{Hook, HookRegistry};
use crate::integration::variables::VariableStore;
use crate::internal::extraction::SharedProvider;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::RetryPolicy;
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::profile::{Profile, default_scan_dirs, resolve_profile};
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::rules::engine::RuleEngine;
use crate::runtime_init::LoadedRuntimeBase;
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
use crate::tools::bash::BashTool;
use crate::tools::diagnostics::{
    DiagnosticInfra, DiagnosticStopHook, DiagnosticsPostCheck, build_diagnostic_infra,
};
use crate::tools::lsp::{LspBackend, LspWorkspace};
use crate::tools::registry_builder::register_standard_tools;
use crate::tools::tool_search::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};

/// A deferred installer that publishes a typed extension on the agent's
/// shared [`ToolContext`] at build time. Stored by
/// [`AgentBuilder::extension`](crate::agent::builder::AgentBuilder::extension)
/// and run during [`assemble_tool_context`].
pub(crate) type ExtensionInstaller = Box<dyn FnOnce(&ToolContext) + Send>;

/// Resolve the agent's working directory: the explicit builder value, or the
/// process CWD when unset.
pub(crate) fn resolve_working_dir(explicit: Option<PathBuf>) -> Result<PathBuf, NornError> {
    match explicit {
        Some(dir) => Ok(dir),
        None => std::env::current_dir().map_err(|e| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: format!("cannot determine working directory: {e}"),
            })
        }),
    }
}

/// Validate and canonicalize a workspace-confinement root.
///
/// The root must exist and be a directory; canonicalizing it (resolving
/// symlinks and relative segments against the process working directory)
/// means the confinement checks enforced by
/// [`ToolContext::confine_to_workspace`](crate::tool::context::ToolContext::confine_to_workspace)
/// always compare against fully resolved real paths, and a misconfigured
/// root fails assembly loudly instead of silently confining nothing.
/// `None` passes through unchanged — no confinement was requested.
///
/// This is the single workspace-root validation shared by every launch
/// path: [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build)
/// applies it to the builder's `workspace_root`, and `norn-cli`'s
/// `build_runtime` applies it to the `--workspace-root` flag value.
///
/// # Errors
///
/// Returns [`NornError::Config`] when the root cannot be canonicalized
/// (does not exist / unresolvable) or resolves to something that is not
/// a directory.
pub fn validate_workspace_root(root: Option<PathBuf>) -> Result<Option<PathBuf>, NornError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let canonical = root.canonicalize().map_err(|e| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "workspace_root {} cannot be resolved: {e}; it must be an existing directory",
                root.display()
            ),
        })
    })?;
    if !canonical.is_dir() {
        return Err(NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "workspace_root {} is not a directory (resolved to {})",
                root.display(),
                canonical.display()
            ),
        }));
    }
    Ok(Some(canonical))
}

/// Resolve the base profile: the explicit profile wins, then a named profile
/// resolved through the standard scan dirs, then the default profile.
pub(crate) fn resolve_base_profile(
    profile: Option<Profile>,
    profile_name: Option<&str>,
    working_dir: &Path,
) -> Result<Profile, NornError> {
    match profile {
        Some(profile) => Ok(profile),
        None => match profile_name {
            Some(name) => Ok(resolve_profile(name, &default_scan_dirs(working_dir))?),
            None => Ok(Profile::default()),
        },
    }
}

/// Build the ungated tool registry: the standard set (with the bash drain
/// grace applied when overridden), plus the caller's extra tools, minus the
/// excluded names.
pub(crate) fn build_base_tool_registry(
    lsp_backend: Option<Arc<dyn LspBackend>>,
    extra_tools: Vec<Box<dyn Tool + Send + Sync>>,
    without_tools: &[String],
    bash_drain_grace: Option<Duration>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_standard_tools(&mut registry, lsp_backend);
    // Replace the standard bash tool with one carrying the overridden drain
    // grace. Caller-registered replacements (extra tools named `bash`) are
    // registered afterwards and win, matching registry semantics.
    if let Some(grace) = bash_drain_grace
        && registry.remove("bash").is_some()
    {
        registry.register(Box::new(BashTool::new().with_drain_grace(grace)));
    }
    for tool in extra_tools {
        registry.register(tool);
    }
    for name in without_tools {
        registry.remove(name);
    }
    registry
}

/// Publish the runtime base's shared infrastructure (task store, diagnostic
/// collector, skill infra, context search paths, consent-boundary permission
/// policy) on the shared tool context.
///
/// `diagnostics` is the *resolved* collector (caller-supplied when present,
/// otherwise the base's own) so an embedder's collector is never displaced.
pub(crate) fn install_runtime_base_extensions(
    shared: &ToolContext,
    base: &LoadedRuntimeBase,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
    working_dir: &Path,
) {
    let effective_diagnostics =
        diagnostics.map_or_else(|| Arc::clone(&base.diagnostics), Arc::clone);
    crate::runtime_init::install_runtime_extensions(
        shared,
        &base.shared_task_store,
        &effective_diagnostics,
    );
    crate::runtime_init::install_skill_infra(
        shared,
        base.skill_paths.clone(),
        Arc::clone(&base.skill_catalog),
    );
    crate::runtime_init::install_context_search_paths(shared, &base.settings, working_dir);
    // Consent boundary: without this, `permissions.deny` / `permissions.ask`
    // rules from the merged settings are silently unenforced on the
    // embedded path (the CLI installs the policy itself).
    crate::runtime_init::install_permission_policy(shared, &base.settings);
}

/// Build the Norn base system prompt from the gated registry and layer it
/// over the profile instructions (or the caller's `system_prompt` override)
/// into `loop_context.system_sections[0]`.
pub(crate) fn install_system_prompt(
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
pub(crate) fn snapshot_store(store: &EventStore) -> EventStore {
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

/// Finish the merged hook registry by appending the diagnostic stop hook.
///
/// When `hooks` is a shared `Arc` (outstanding caller clones), the existing
/// hooks are folded in via [`HookRegistry::merge_shared`] so nothing is
/// dropped and the caller's hooks keep first-`Block`-wins precedence over
/// the diagnostic stop hook, which always registers last.
pub(crate) fn append_diagnostic_stop_hook(
    hooks: Option<Arc<HookRegistry>>,
    diagnostic_infra: Option<Arc<DiagnosticInfra>>,
) -> Option<Arc<HookRegistry>> {
    let Some(infra) = diagnostic_infra else {
        return hooks;
    };

    let mut registry = match hooks {
        Some(arc) => match Arc::try_unwrap(arc) {
            Ok(owned) => owned,
            Err(shared) => {
                let mut fresh = HookRegistry::new();
                fresh.merge_shared(shared);
                fresh
            }
        },
        None => HookRegistry::new(),
    };
    registry.register(Hook::Stop(Box::new(DiagnosticStopHook::new(infra))));
    Some(Arc::new(registry))
}

/// Builder-level overrides feeding [`resolve_runtime_overlay`]. Every field
/// is the caller-supplied value taken off the builder; `None` defers to the
/// runtime base (when loaded).
pub(crate) struct OverlayOverrides {
    /// Caller-supplied diagnostic infrastructure (wins over the default
    /// infra built for the runtime base).
    pub(crate) diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    /// Caller-supplied diagnostic collector (never displaced by the base's).
    pub(crate) diagnostics: Option<Arc<DiagnosticCollector>>,
    /// Caller-supplied rules engine (wins over base-discovered rules).
    pub(crate) rules: Option<RuleEngine>,
    /// Programmatic hook registry. When the runtime base was loaded this is
    /// `None` — the registry was already merged into the base's hooks (H13).
    pub(crate) hooks: Option<Arc<HookRegistry>>,
    /// LSP backend for default diagnostic-infra construction.
    pub(crate) lsp_backend: Option<Arc<dyn LspBackend>>,
    /// LSP workspace for default diagnostic-infra construction.
    pub(crate) lsp_workspace: Option<Arc<LspWorkspace>>,
}

/// Cross-cutting infrastructure resolved from the runtime base plus the
/// builder overrides; consumed by `AgentBuilder::build`.
pub(crate) struct RuntimeOverlay {
    /// The runtime base, passed back with its rules/hooks taken.
    pub(crate) runtime_base: Option<LoadedRuntimeBase>,
    /// Resolved diagnostic collector (caller's, else the base's).
    pub(crate) diagnostics: Option<Arc<DiagnosticCollector>>,
    /// Resolved diagnostic infrastructure, when any is configured.
    pub(crate) diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    /// Resolved rules engine.
    pub(crate) rules: Option<RuleEngine>,
    /// Final hook registry: programmatic/base hooks plus the diagnostic
    /// stop hook (always registered last so user hooks win first-`Block`).
    pub(crate) hooks: Option<Arc<HookRegistry>>,
}

/// Resolve the cross-cutting build infrastructure: caller overrides win,
/// the runtime base backs them up, and the diagnostic stop hook is folded
/// onto the final hook registry.
///
/// H13: exactly one of `overrides.hooks` / the base's merged hooks is
/// `Some` when hooks exist (the builder moves its programmatic registry
/// into `load_runtime_base` when a base is loaded), so nothing is merged
/// twice and nothing is silently dropped.
pub(crate) fn resolve_runtime_overlay(
    mut runtime_base: Option<LoadedRuntimeBase>,
    overrides: OverlayOverrides,
    working_dir: &Path,
) -> RuntimeOverlay {
    let runtime_rules = runtime_base.as_mut().and_then(|base| base.rules.take());
    let runtime_hooks = runtime_base.as_mut().and_then(|base| base.hooks.take());
    let diagnostic_infra = if let Some(infra) = overrides.diagnostic_infra {
        Some(infra)
    } else if runtime_base.is_some() {
        Some(Arc::new(build_diagnostic_infra(
            working_dir,
            overrides.lsp_backend,
            overrides.lsp_workspace.as_deref(),
        )))
    } else {
        None
    };
    // A caller-supplied diagnostic collector always wins; the runtime
    // base's collector backs it up only when the caller supplied none.
    let diagnostics = overrides.diagnostics.or_else(|| {
        runtime_base
            .as_ref()
            .map(|base| Arc::clone(&base.diagnostics))
    });
    let rules = overrides.rules.or(runtime_rules);
    let hook_source = overrides.hooks.or(runtime_hooks);
    let hooks = append_diagnostic_stop_hook(hook_source, diagnostic_infra.as_ref().map(Arc::clone));
    RuntimeOverlay {
        runtime_base,
        diagnostics,
        diagnostic_infra,
        rules,
        hooks,
    }
}

/// Overlay the runtime base's loaders and monitors onto the loop context:
/// NORN.md context loader, skill-catalog prompt listing, iteration monitor.
pub(crate) fn apply_base_to_loop_context(loop_context: &mut LoopContext, base: &LoadedRuntimeBase) {
    loop_context.context_loader = Some(base.context_loader.clone());
    loop_context.base_suffix = base.skill_catalog.system_prompt_listing();
    loop_context
        .iteration_monitor
        .clone_from(&base.iteration_monitor);
}

/// The effective agent-loop config: the runtime base's config with
/// explicitly-set builder fields overlaid, or the explicit config alone
/// when no base was loaded. This single value drives both the loop config
/// and the system prompt's compaction guidance — they must never consult
/// different sources.
pub(crate) fn effective_agent_config(
    runtime_base: Option<&LoadedRuntimeBase>,
    explicit: AgentLoopConfig,
) -> AgentLoopConfig {
    match runtime_base {
        Some(base) => merge_agent_config(base.agent_config.clone(), explicit),
        None => explicit,
    }
}

/// Populate the loop context's execution infrastructure: retry policy
/// (explicit, else the runtime base's, else default), token estimator,
/// context-edit tracker, diagnostics, working dir, variable store, and
/// environment config. Returns the session id minted by the variable store.
pub(crate) fn populate_loop_context(
    loop_context: &mut LoopContext,
    retry_policy: Option<RetryPolicy>,
    runtime_base: Option<&LoadedRuntimeBase>,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
    shared_wd: &SharedWorkingDir,
    model: &str,
) -> String {
    loop_context.retry_policy = retry_policy.unwrap_or_else(|| {
        runtime_base.map_or_else(RetryPolicy::default, |base| base.retry_policy.clone())
    });
    loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
    loop_context.context_edits = Some(ContextEdits::new());
    loop_context.diagnostics = diagnostics.map(Arc::clone);
    loop_context.working_dir = shared_wd.clone();
    let variables = Arc::new(VariableStore::with_builtins().with_working_dir(shared_wd.clone()));
    let session_id = variables.session_id().to_owned();
    loop_context.variables = Some(variables);
    loop_context.environment = Some(EnvironmentConfig {
        session_id: Some(session_id.clone()),
        model: model.to_owned(),
    });
    session_id
}

/// Create (or resume) the session event store and the action log that
/// shares it.
///
/// The event store backs both the loop's `ToolResult` persistence and the
/// action log's Level 2/3 look-ups, so one `Arc` is shared between them.
/// The action log resolves model-supplied relative paths against the live
/// agent working dir (not process CWD), so it shares the same
/// [`SharedWorkingDir`] handle as the tool context. When resuming,
/// persisted compactions are re-applied and the action ledger is rebuilt
/// so the session-lifetime queryability contract holds.
pub(crate) fn restore_session_state(
    session: Option<EventStore>,
    loop_context: &mut LoopContext,
    shared_wd: SharedWorkingDir,
) -> (Arc<EventStore>, Arc<ActionLog>) {
    let resuming = session.is_some();
    let event_store = Arc::new(session.unwrap_or_default());
    if let Some(edits) = loop_context.context_edits.as_mut() {
        edits.apply_persisted_compactions(&event_store);
    }
    let action_log = Arc::new(ActionLog::with_working_dir(
        Arc::clone(&event_store),
        shared_wd,
    ));
    if resuming {
        crate::agent::resume::rebuild_action_log(&action_log, &event_store.events());
    }
    (event_store, action_log)
}

/// Parts for [`assemble_tool_context`]; every field is consumed into the
/// assembled context.
pub(crate) struct ToolContextParts {
    /// Shared working-dir handle (same handle as the loop context's).
    pub(crate) shared_wd: SharedWorkingDir,
    /// Validated workspace-confinement root, when configured.
    pub(crate) workspace_root: Option<PathBuf>,
    /// Session id minted by the variable store.
    pub(crate) session_id: String,
    /// Resolved diagnostic collector.
    pub(crate) diagnostics: Option<Arc<DiagnosticCollector>>,
    /// Resolved diagnostic infrastructure; installs the diagnostics
    /// post-check alongside it.
    pub(crate) diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    /// H14: the *final merged* hook registry — the same `Arc` the loop
    /// dispatches — so sub-agent tools observe identical hooks.
    pub(crate) hooks: Option<Arc<HookRegistry>>,
    /// Caller-supplied post-validation checks, appended after the
    /// diagnostics post-check.
    pub(crate) post_checks: Vec<Box<dyn RuntimePostValidateCheck>>,
    /// Provider published for internal extraction agents.
    pub(crate) provider: Arc<dyn Provider>,
    /// The shared action log (same `Arc` as the loop context's).
    pub(crate) action_log: Arc<ActionLog>,
    /// Consumer-supplied extension installers, run last so embedding
    /// runtimes can contribute tool-catalog extras before publication.
    pub(crate) extensions: Vec<ExtensionInstaller>,
}

/// Assemble the agent's shared [`ToolContext`]: workspace confinement is
/// applied first (before any extension or registry publication, so every
/// tool call observes it from the first dispatch), then the standard
/// extensions, post-checks, and consumer-supplied installers.
pub(crate) fn assemble_tool_context(parts: ToolContextParts) -> ToolContext {
    let mut ctx = ToolContext::with_working_dir(parts.shared_wd);
    if let Some(root) = parts.workspace_root {
        ctx.confine_to_workspace(root);
    }
    ctx.insert_extension(Arc::new(SessionId(parts.session_id)));
    if let Some(diagnostics) = parts.diagnostics {
        ctx.insert_extension(diagnostics);
    }
    if let Some(infra) = parts.diagnostic_infra {
        ctx.insert_extension(infra);
        ctx.post_checks.push(Box::new(DiagnosticsPostCheck));
    }
    if let Some(hooks) = parts.hooks {
        ctx.insert_extension(hooks);
    }
    ctx.post_checks.extend(parts.post_checks);
    ctx.insert_extension(Arc::new(SharedProvider(parts.provider)));
    ctx.insert_extension(parts.action_log);
    for install in parts.extensions {
        install(&ctx);
    }
    ctx
}

/// Overlay explicitly-set builder fields onto the runtime-base agent config.
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

/// Publish the tool catalog (registry tools plus consumer extras) on `ctx`.
pub(crate) fn install_tool_catalog(registry: &ToolRegistry, ctx: &ToolContext) {
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
pub(crate) fn collect_tool_definitions(registry: &ToolRegistry) -> Vec<ToolDefinition> {
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

/// Parts needed to install the fork/spawn runtime infrastructure.
pub(crate) struct AgentInfraParts {
    /// Shared agent registry the coordination tools resolve against.
    pub(crate) registry: Arc<RwLock<AgentRegistry>>,
    /// Provider shared with spawned and forked children.
    pub(crate) provider: Arc<dyn Provider>,
    /// The parent agent's session event store.
    pub(crate) event_store: Arc<EventStore>,
    /// The parent agent's id.
    pub(crate) id: Uuid,
}

/// Install the complete fork/spawn runtime on the agent's shared
/// [`ToolContext`]: [`AgentToolInfra`], an empty
/// [`AgentHandles`](crate::tools::agent::AgentHandles) collection
/// (required by `spawn_agent` / `fork`), a [`ChildResultSender`] whose
/// receiver must be wired onto
/// [`LoopContext::child_result_rx`](crate::r#loop::loop_context::LoopContext)
/// so the loop drains child results at iteration boundaries, and the
/// [`ReclaimOnResultDelivery`](crate::tools::agent::ReclaimOnResultDelivery)
/// marker.
///
/// The reclamation marker is installed unconditionally here because every
/// builder-assembled agent is an embedded / headless runtime: no external
/// status observer (such as the TUI agent status panel) ever polls its
/// registry, so once a naturally-finished child's result has been
/// delivered through the channel, the spawn/fork wrapper reclaims the
/// terminal registry entry and parent-held handle — long-running embedded
/// processes must not pin one event store per finished child forever. See
/// [`crate::tools::agent::reclaim`] for the full ownership rule; the TUI
/// runtime is assembled by `norn-cli`'s `build_runtime`, never through
/// this path.
///
/// Returns the receiver half of the child-result channel; the caller wires
/// it into the loop context. Everything `spawn_agent` / `fork` /
/// `signal_agent` / `close_agent` resolve at call time is published here —
/// no partial wiring.
pub(crate) fn install_agent_infra(
    tool_registry: &Arc<ToolRegistry>,
    shared: &ToolContext,
    parts: AgentInfraParts,
) -> mpsc::Receiver<ChildAgentResult> {
    let infra = AgentToolInfra {
        registry: parts.registry,
        mailbox: Arc::new(Mailbox::new()),
        provider: parts.provider,
        event_store: parts.event_store,
        agent_id: parts.id,
        parent_id: None,
        tool_registry: Some(Arc::clone(tool_registry)),
    };
    shared.insert_extension(Arc::new(infra));
    crate::runtime_init::install_agent_handles(shared);
    crate::runtime_init::install_terminal_reclamation(shared);

    let (child_tx, child_rx) = mpsc::channel::<ChildAgentResult>(CHILD_RESULT_CHANNEL_CAPACITY);
    shared.insert_extension(Arc::new(ChildResultSender(Arc::new(child_tx))));
    child_rx
}
