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

use crate::agent::child_policy::CoordinationEnvelope;
use crate::agent::message_router::MessageRouter;
use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{Hook, HookRegistry};
use crate::integration::variables::VariableStore;
use crate::internal::extraction::SharedProvider;
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::RetryPolicy;
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::profile::{Profile, default_scan_dirs, resolve_profile};
use crate::provider::request::ToolDefinition;
use crate::provider::surface::collect_function_definitions;
use crate::provider::traits::Provider;
use crate::rules::engine::RuleEngine;
use crate::runtime_init::LoadedRuntimeBase;
use crate::session::action_log::ActionLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;
use crate::skill::SkillCatalog;
use crate::system_prompt::environment::EnvironmentConfig;
use crate::tool::catalog::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};
use crate::tool::context::{SessionId, SharedWorkingDir, ToolContext};
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;
use crate::tools::agent::AgentToolInfra;
use crate::tools::bash::BashTool;
use crate::tools::diagnostics::{
    DiagnosticInfra, DiagnosticStopHook, DiagnosticsPostCheck, build_diagnostic_infra,
};
use crate::tools::lsp::{LspBackend, LspWorkspace};
use crate::tools::registry_builder::register_standard_tools;

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

/// Resolve the [`SkillToolConfig`](crate::tools::skill::SkillToolConfig)
/// from the merged settings' `tools.skill` section (D5).
///
/// An absent section — or an absent `shell_execution` key — defers to the
/// tool's own documented default (shell execution **enabled**); `false`
/// disables skill-authored shell expansion. This is the library-side
/// mirror of the CLI's `skill_tool_config_from_settings`, so the embedded
/// `load_runtime_base` path and the CLI resolve the skill tool identically.
pub(crate) fn skill_tool_config_from_settings(
    settings: &crate::config::NornSettings,
) -> crate::tools::skill::SkillToolConfig {
    let shell_execution = settings
        .tools
        .as_ref()
        .and_then(|tools| tools.skill.as_ref())
        .and_then(|skill| skill.shell_execution);
    match shell_execution {
        Some(shell_execution) => crate::tools::skill::SkillToolConfig { shell_execution },
        None => crate::tools::skill::SkillToolConfig::default(),
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
    // Report the skill catalog's load-time diagnostics (skill-shadowed,
    // skill-missing-description, skill-yaml-parse-failed, skill-io-error,
    // skill-allowed-tools-not-enforced, …) into the shared diagnostic
    // collector so a user with a malformed skill sees a surfaced diagnostic
    // instead of the skill silently vanishing. The catalog retains its own
    // list (this copies, it does not empty), so this single assembly seam —
    // the one place both the catalog and the resolved collector are in
    // scope (the CLI reaches it through `AgentBuilder`) — must remain the
    // only caller per build. Severity is carried on each `NornDiagnostic`,
    // so warnings stay warnings and info stays info.
    report_skill_diagnostics(&base.skill_catalog, &effective_diagnostics);
    crate::runtime_init::install_context_search_paths(shared, &base.settings, working_dir);
    // Consent boundary: without this, `permissions.deny` / `permissions.ask`
    // rules from the merged settings are silently unenforced on the
    // embedded path (the CLI installs the policy itself).
    crate::runtime_init::install_permission_policy(shared, &base.settings);
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
    // An explicit rules engine (e.g. norn-cli's `--rules` file) is merged
    // ONTO the runtime base's auto-discovered rules rather than replacing
    // them: both sets are enforced, and the explicit rules inherit the
    // base engine's working-dir / diagnostics / shell-timeout wiring. On a
    // rule-ID collision (both IDs derive from the file stem) the explicit
    // rule wins — `merge_rules_from` drops the shadowed discovered rule and
    // logs it, so the operator's `--rules` override is never silently
    // discarded. With no base rules, the explicit engine passes through with
    // its own wiring; with no explicit engine, the base's discovered rules
    // stand.
    let rules = match (overrides.rules, runtime_rules) {
        (Some(explicit), Some(mut base_rules)) => {
            base_rules.merge_rules_from(explicit);
            Some(base_rules)
        }
        (Some(explicit), None) => Some(explicit),
        (None, base_rules) => base_rules,
    };
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
/// NORN.md context loader and iteration monitor.
///
/// The skill-catalog prompt listing is applied separately by
/// [`apply_skill_listing`] — the single shared mechanism every launch path
/// (root here; spawned/forked children at their own construction sites)
/// uses — so the "# Available Skills" section cannot drift between root and
/// children, and is gated on whether the `skill` tool is actually callable.
pub(crate) fn apply_base_to_loop_context(loop_context: &mut LoopContext, base: &LoadedRuntimeBase) {
    loop_context.context_loader = Some(base.context_loader.clone());
    loop_context
        .iteration_monitor
        .clone_from(&base.iteration_monitor);
}

/// Copy the skill catalog's accumulated load-time diagnostics into the
/// shared diagnostic collector, preserving each diagnostic's own severity.
/// The catalog retains its own list — this reports, it does not empty the
/// source, so it must be called exactly once per assembled agent (the
/// single `build()` call site).
///
/// Without this report a malformed skill (missing description, unparseable
/// frontmatter, IO error, shadowed name, unenforced allowed-tools) is
/// silently dropped from the catalog and the user sees nothing.
fn report_skill_diagnostics(catalog: &SkillCatalog, collector: &DiagnosticCollector) {
    for diagnostic in catalog.diagnostics() {
        collector.report(diagnostic.clone());
    }
}

/// Whether the `skill` tool is on a child's resolved tool surface: it must
/// be present (and un-gated) in the shared parent registry *and* admitted
/// by the child's allow-list — the same two filters
/// [`collect_function_definitions`](crate::provider::surface::collect_function_definitions)
/// applies to the child's tool definitions. A child's system prompt
/// advertises the skill listing only when this holds, so it never lists a
/// skill the child has no tool to load.
pub(crate) fn child_skill_tool_available(
    parent_registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> bool {
    parent_registry.get("skill").is_some()
        && allow_list.is_none_or(|list| list.iter().any(|name| name == "skill"))
}

/// Apply the skill-catalog "# Available Skills" listing to a loop context's
/// `base_suffix` — the single shared mechanism the root builder and the
/// child launch paths use for the listing's content and gating, so the
/// section cannot drift between them.
///
/// Sets nothing when `skill_tool_available` is `false`: advertising a skill
/// the agent has no tool to load would be a lie. The content is the
/// catalog's filtered
/// [`SkillCatalog::system_prompt_listing`], identical for root and
/// children (an all-hidden or empty catalog yields an empty string, which
/// the system-prompt build omits).
pub(crate) fn apply_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    if skill_tool_available {
        loop_context.base_suffix = catalog.system_prompt_listing();
    }
}

/// Give a spawned/forked child the same "# Available Skills" listing the
/// root gets.
///
/// Children build a bare [`LoopContext`] and never run the root's
/// `install_system_prompt` — the step that materializes `base_suffix` into
/// the system instruction — so this applies the shared listing via
/// [`apply_skill_listing`], then folds the child's base instruction into
/// `base_prefix` and rebuilds the base section, producing the same
/// base-instruction-then-listing layering the root emits. A no-op when the
/// resulting listing is empty (the skill tool is gated off for the child,
/// or the catalog is empty / all-hidden), leaving the child's system
/// instruction untouched.
pub(crate) fn install_child_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    // An embedder-supplied parent base (`ParentSystemInstruction`) may
    // legitimately already contain the listing — the root's
    // `base_system_instruction()` includes its materialized `base_suffix`.
    // Appending again would duplicate the section, so the exact generated
    // listing text already present anywhere in the child's base is treated
    // as installed.
    let listing = catalog.system_prompt_listing();
    if !listing.is_empty()
        && loop_context
            .system_sections
            .first()
            .is_some_and(|base| base.contains(&listing))
    {
        return;
    }
    apply_skill_listing(loop_context, catalog, skill_tool_available);
    if loop_context.base_suffix.is_empty() {
        return;
    }
    if loop_context.base_prefix.is_empty() {
        loop_context.base_prefix = loop_context
            .system_sections
            .first()
            .cloned()
            .unwrap_or_default();
    }
    loop_context.rebuild_base_section();
}

/// Which non-`Option` [`AgentLoopConfig`] fields the caller explicitly set.
///
/// `Option` fields carry their own presence (`Some` = explicitly set), but
/// the four non-`Option` fields below are indistinguishable from an unset
/// default by value alone. A caller that explicitly restores the library
/// default (e.g. `-c schema_budget=3` when 3 is the default) must still win
/// over a settings-derived runtime base, so presence is tracked
/// structurally here rather than inferred from value inequality — the
/// latter silently reverted an explicit-to-default override back to the
/// base value.
#[derive(Clone, Copy, Default)]
pub(crate) struct AgentConfigPresence {
    /// `schema_attempt_budget` was explicitly set.
    pub(crate) schema_attempt_budget: bool,
    /// `auto_compact_keep_recent_turns` was explicitly set.
    pub(crate) auto_compact_keep_recent_turns: bool,
    /// `auto_compact_reserve_tokens` was explicitly set. Tracked
    /// structurally (like the other non-sentinel fields) because its
    /// default is a meaningful `Some(30_000)`, not `None` — a value-vs-`None`
    /// heuristic would clobber a base's reserve with the builder default.
    pub(crate) auto_compact_reserve_tokens: bool,
    /// `schema_tool_name` was explicitly set.
    pub(crate) schema_tool_name: bool,
    /// `conversation_state` was explicitly set.
    pub(crate) conversation_state: bool,
}

impl AgentConfigPresence {
    /// Mark every non-`Option` field present — the caller supplied a
    /// complete [`AgentLoopConfig`] via
    /// [`AgentBuilder::agent_config`](crate::agent::builder::AgentBuilder::agent_config),
    /// so all of its fields are explicit.
    pub(crate) fn all() -> Self {
        Self {
            schema_attempt_budget: true,
            auto_compact_keep_recent_turns: true,
            auto_compact_reserve_tokens: true,
            schema_tool_name: true,
            conversation_state: true,
        }
    }
}

/// The effective agent-loop config: the runtime base's config with
/// explicitly-set builder fields overlaid, or the explicit config alone
/// when no base was loaded. This single value drives both the loop config
/// and the system prompt's compaction guidance — they must never consult
/// different sources.
pub(crate) fn effective_agent_config(
    runtime_base: Option<&LoadedRuntimeBase>,
    explicit: AgentLoopConfig,
    present: AgentConfigPresence,
) -> AgentLoopConfig {
    match runtime_base {
        Some(base) => merge_agent_config(base.agent_config.clone(), explicit, present),
        None => explicit,
    }
}

/// Arm auto-compaction on a loop context and its effective agent-loop
/// config — the single shared mechanism every agent launch path (root,
/// spawned child, rhai-spawned child, fork) uses, so the trigger cannot
/// drift between them.
///
/// Installs the token estimator and the [`ContextEdits`] tracker on the
/// loop context (the preflight needs both: the estimator to size each
/// request, the tracker for the usage floor and the compaction commit),
/// and fills an unset `context_window_limit` from the model catalog for
/// *this agent's* resolved model. An explicit window — from settings, a
/// `-c` override, or any future child-policy field — always wins because
/// the fill runs only when the merged value is still `None`. A model
/// absent from the catalog keeps `None`, which leaves the trigger
/// disabled (`maybe_auto_compact` returns early on a `None` window),
/// matching the root behavior exactly. The reserve default
/// (`AgentLoopConfig::default().auto_compact_reserve_tokens`) already
/// flows through the config and is not touched here.
pub(crate) fn arm_auto_compaction(
    loop_context: &mut LoopContext,
    config: &mut AgentLoopConfig,
    model: &str,
) {
    loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
    loop_context.context_edits = Some(ContextEdits::new());
    if config.context_window_limit.is_none() {
        config.context_window_limit =
            crate::model_catalog::smallest_context_window_for_model(model);
    }
}

/// Populate the loop context's execution infrastructure: retry policy
/// (explicit, else the runtime base's, else default), diagnostics,
/// working dir, variable store, and environment config. Returns the
/// session id: `session_id_override` when given (the persisted session's
/// index-entry id from `open_session`), otherwise the id minted by the
/// variable store. The returned id, the `{{session_id}}` variable, and
/// the system prompt environment always agree.
///
/// Auto-compaction (token estimator, context-edit tracker, and the
/// catalog-derived context window) is armed separately by
/// [`arm_auto_compaction`], which the caller invokes once the effective
/// agent-loop config is resolved — the same shared mechanism the child
/// launch paths use, so the trigger cannot drift between root and
/// children.
pub(crate) fn populate_loop_context(
    loop_context: &mut LoopContext,
    retry_policy: Option<RetryPolicy>,
    runtime_base: Option<&LoadedRuntimeBase>,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
    shared_wd: &SharedWorkingDir,
    model: &str,
    session_id_override: Option<&str>,
) -> String {
    loop_context.retry_policy = retry_policy.unwrap_or_else(|| {
        runtime_base.map_or_else(RetryPolicy::default, |base| base.retry_policy.clone())
    });
    loop_context.diagnostics = diagnostics.map(Arc::clone);
    loop_context.working_dir = shared_wd.clone();
    let mut variables = VariableStore::with_builtins().with_working_dir(shared_wd.clone());
    if let Some(id) = session_id_override {
        variables = variables.with_session_id(id);
    }
    let variables = Arc::new(variables);
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
/// [`SharedWorkingDir`] handle as the tool context. When resuming, the
/// history is snapshotted once into
/// [`ReplayArtifacts`](crate::session::ReplayArtifacts) — a single
/// traversal — and that one value restores both the persisted compaction
/// marks and the action ledger, so the session-lifetime queryability
/// contract holds without per-consumer re-walks of the event history.
pub(crate) fn restore_session_state(
    session: Option<EventStore>,
    loop_context: &mut LoopContext,
    shared_wd: SharedWorkingDir,
) -> (Arc<EventStore>, Arc<ActionLog>) {
    let resuming = session.is_some();
    let event_store = Arc::new(session.unwrap_or_default());
    let action_log = Arc::new(ActionLog::with_working_dir(
        Arc::clone(&event_store),
        shared_wd,
    ));
    if resuming {
        let artifacts = crate::session::ReplayArtifacts::from_events(event_store.events());
        if let Some(edits) = loop_context.context_edits.as_mut() {
            edits.mark_superseded(artifacts.superseded_event_ids.iter().cloned());
        }
        crate::agent::resume::rebuild_action_log(&action_log, &artifacts.events);
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
    /// Effective context window used to derive model-facing tool-output caps.
    pub(crate) context_window_limit: Option<u64>,
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
    crate::runtime_init::install_tool_output_budget(&ctx, parts.context_window_limit);
    ctx.insert_extension(Arc::new(SharedProvider(parts.provider)));
    ctx.insert_extension(parts.action_log);
    for install in parts.extensions {
        install(&ctx);
    }
    ctx
}

/// Overlay explicitly-set builder fields onto the runtime-base agent config.
///
/// Fields whose default is a meaningful value rather than `None` — the
/// non-`Option` fields and `auto_compact_reserve_tokens` — overlay when
/// `present` marks them explicit (so an explicit-to-default value still
/// wins, and the builder default never clobbers a base value). The
/// remaining `Option` fields, whose `None` default already means "unset",
/// overlay when they are `Some`. Every field of [`AgentLoopConfig`] is
/// covered — a completeness test in this module asserts a fully-explicit
/// config survives the merge intact, so a future field cannot silently miss
/// the overlay.
fn merge_agent_config(
    mut base: AgentLoopConfig,
    explicit: AgentLoopConfig,
    present: AgentConfigPresence,
) -> AgentLoopConfig {
    if present.schema_attempt_budget {
        base.schema_attempt_budget = explicit.schema_attempt_budget;
    }
    if present.auto_compact_keep_recent_turns {
        base.auto_compact_keep_recent_turns = explicit.auto_compact_keep_recent_turns;
    }
    if present.auto_compact_reserve_tokens {
        base.auto_compact_reserve_tokens = explicit.auto_compact_reserve_tokens;
    }
    if present.schema_tool_name {
        base.schema_tool_name = explicit.schema_tool_name;
    }
    if present.conversation_state {
        base.conversation_state = explicit.conversation_state;
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
    if explicit.cache_key.is_some() {
        base.cache_key = explicit.cache_key;
    }
    if explicit.server_compaction_threshold_tokens.is_some() {
        base.server_compaction_threshold_tokens = explicit.server_compaction_threshold_tokens;
    }
    if explicit.output_schema.is_some() {
        base.output_schema = explicit.output_schema;
    }
    if explicit.prompt_command_timeout.is_some() {
        base.prompt_command_timeout = explicit.prompt_command_timeout;
    }
    if explicit.linger.is_some() {
        base.linger = explicit.linger;
    }
    base
}

/// Publish the tool catalog (registry tools plus consumer extras) on `ctx`.
///
/// Entries come from each tool's
/// [`Tool::catalog_entries`](crate::tool::traits::Tool::catalog_entries),
/// so field hints and composite subcommand entries are derived from the
/// tools' own schemas.
///
/// The published snapshot is deliberately provider-blind: the `tool_search`
/// tool projects it through
/// [`reframe_catalog_entries`](crate::provider::surface::reframe_catalog_entries)
/// at query time against the capabilities of the provider published on the
/// live tool context, so the catalog the model sees always tracks the
/// currently-bound provider — including across rebinds and on launch paths
/// that install the catalog before a provider exists.
pub(crate) fn install_tool_catalog(registry: &ToolRegistry, ctx: &ToolContext) {
    let mut entries: Vec<ToolCatalogEntry> = registry
        .names()
        .filter_map(|name| registry.get(name))
        .flat_map(Tool::catalog_entries)
        .collect();

    if let Some(extras) = ctx.get_extension::<ToolCatalogExtras>() {
        entries.extend(extras.0.iter().cloned());
    }

    ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries))));
}

/// Tool definitions (envelope-wrapped schemas) for the provider call.
///
/// Delegates to the shared registry → function-definition projection in
/// [`crate::provider::surface`] — the same projection the spawn/fork child
/// launch paths use — so the inputs to the resolved tool surface cannot
/// drift between assembly and child launches.
pub(crate) fn collect_tool_definitions(registry: &ToolRegistry) -> Vec<ToolDefinition> {
    collect_function_definitions(registry, None)
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
    /// The builder-required coordination envelope: the root's child
    /// policy plus the child-result channel capacity. Validated by
    /// [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build)
    /// before this runs (both values present, capacities non-zero).
    pub(crate) envelope: CoordinationEnvelope,
    /// The root agent's own inbound sender, when the builder configured
    /// an inbound channel (`AgentBuilder::inbound_capacity`). Registered
    /// in the [`MessageRouter`] under the root's id so children can
    /// address `"parent"` at the top level. `None` when the root has no
    /// inbound channel — messaging the root then fails honestly as
    /// `NotRouted`.
    pub(crate) root_inbound: Option<crate::r#loop::inbound::InboundSender>,
    /// The root agent's own run-cancellation token — the builder's
    /// `cancel_token` when one was supplied, otherwise the fresh token
    /// the builder resolved; the *same* token `Agent::run` threads into
    /// the root's [`AgentStepRequest`](crate::agent_loop::runner::AgentStepRequest)
    /// and `AgentHandle::cancel` triggers. Published as the
    /// [`AgentCancellation`](crate::tools::agent::AgentCancellation)
    /// extension so spawn/fork create child run tokens as children of it
    /// — cancelling the root cascades to the whole tree (W3.5).
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    /// Whether to install the
    /// [`ReclaimOnResultDelivery`](crate::tools::agent::ReclaimOnResultDelivery)
    /// marker. `true` for embedded / headless runtimes (no external status
    /// observer polls the registry, so a finished child's terminal entry
    /// is reclaimed once its result is delivered); `false` for a driver
    /// that owns reclamation itself through a status panel (the TUI), which
    /// would otherwise race the hold window into nonexistence.
    pub(crate) terminal_reclamation: bool,
}

/// Install the complete fork/spawn runtime on the agent's shared
/// [`ToolContext`]: [`AgentToolInfra`], an empty
/// [`AgentHandles`](crate::tools::agent::AgentHandles) collection
/// (required by `spawn_agent` / `fork`), a [`ChildResultSender`] whose
/// receiver must be wired onto
/// [`LoopContext::child_result_rx`](crate::agent_loop::loop_context::LoopContext)
/// so the loop drains child results at iteration boundaries, and the
/// [`ReclaimOnResultDelivery`](crate::tools::agent::ReclaimOnResultDelivery)
/// marker.
///
/// The reclamation marker is installed when
/// [`AgentInfraParts::terminal_reclamation`] is `true` (the builder
/// default, matching every embedded / headless runtime): no external
/// status observer polls the registry, so once a naturally-finished
/// child's result has been delivered through the channel, the spawn/fork
/// wrapper reclaims the terminal registry entry and parent-held handle —
/// long-running embedded processes must not pin one event store per
/// finished child forever. A driver that owns reclamation itself through a
/// status panel (the TUI, via
/// [`AgentBuilder::terminal_reclamation(false)`](crate::agent::builder::AgentBuilder::terminal_reclamation))
/// suppresses it. See [`crate::tools::agent::reclaim`] for the full
/// ownership rule.
///
/// Also publishes the session-wide [`ActionLogTree`] rooted at this agent:
/// every spawn/fork child registers its own per-agent [`ActionLog`] into
/// this tree (and forwards it to grandchildren), so the `action_log`
/// tool's `scope` argument can federate queries over the agent's subtree.
/// The root's own log — already inserted on `shared` by
/// [`assemble_tool_context`] — is registered as the tree root. On session
/// resume the root's log is rebuilt by [`restore_session_state`]; child
/// branches are not persisted, so a resumed tree starts with the root
/// alone (see [`crate::session::action_log_tree`]).
///
/// Also publishes the builder-required [`CoordinationEnvelope`] (the
/// root's child policy plus the child-result channel capacity) on the
/// shared context, and sizes the child-result channel from it — the
/// capacity is always the caller's explicit choice, never a library
/// default.
///
/// Also publishes the root's run-cancellation token as the
/// [`AgentCancellation`](crate::tools::agent::AgentCancellation)
/// extension (W3.5): spawn/fork create each child's run token as a child
/// of it, so cancelling the root cascades cooperatively through the
/// whole agent tree — every descendant ends with its real `Cancelled`
/// outcome through its own completion wrapper.
///
/// Also registers the root's own inbound sender (when one exists) in the
/// fresh [`MessageRouter`] under the root's id, so children can address
/// `"parent"` at the top level. The root's route is process-lifetime — it
/// has no completion wrapper — so it is never explicitly deregistered;
/// when the root's loop ends, the closed channel is detected on the next
/// delivery attempt and the stale route removed (`RouteError::ChannelClosed`
/// lazy cleanup).
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
    let router = Arc::new(MessageRouter::new());
    if let Some(root_inbound) = parts.root_inbound {
        router.register(parts.id, root_inbound);
    }
    let infra = AgentToolInfra {
        registry: parts.registry,
        router,
        pending_messages: Arc::new(PendingAgentMessages::from_events(
            &parts.event_store.events(),
        )),
        provider: parts.provider,
        event_store: parts.event_store,
        agent_id: parts.id,
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::clone(tool_registry)),
    };
    shared.insert_extension(Arc::new(infra));
    // W3.5 cancellation cascade: publish the root's run token so
    // spawn/fork create child run tokens as children of it — cancelling
    // the root's handle cancels every descendant's run cooperatively.
    shared.insert_extension(Arc::new(crate::tools::agent::AgentCancellation(
        parts.cancel,
    )));
    crate::runtime_init::install_agent_handles(shared);
    // Terminal reclamation is gated (not unconditional): a headless /
    // embedded runtime reclaims a finished child's terminal registry entry
    // once its result is delivered, but a driver that owns reclamation
    // through a status panel (the TUI) must not — installing the marker
    // there would race its hold window into nonexistence.
    if parts.terminal_reclamation {
        crate::runtime_init::install_terminal_reclamation(shared);
    }

    let log_tree = Arc::new(ActionLogTree::new(parts.id));
    if let Some(root_log) = shared.get_extension::<ActionLog>() {
        log_tree.register(parts.id, None, root_log);
    } else {
        // Unreachable on the builder path (assemble_tool_context always
        // inserts the action log before this runs), but a context wired
        // differently must not lose the tree anchor silently.
        tracing::warn!(
            agent_id = %parts.id,
            "install_agent_infra: no ActionLog extension on the shared context; \
             the action-log tree is anchored at the root with no root log",
        );
    }
    shared.insert_extension(log_tree);

    // The coordination envelope is carried on the shared context so the
    // spawn/fork launch paths read the root's child policy and per-agent
    // channel capacities from one place; without it every spawn/fork
    // fails with a typed MissingExtension (W3.2).
    let child_result_capacity = parts.envelope.child_result_capacity;
    shared.insert_extension(Arc::new(parts.envelope));

    let (child_tx, child_rx) = mpsc::channel::<ChildAgentResult>(child_result_capacity);
    shared.insert_extension(Arc::new(ChildResultSender(Arc::new(child_tx))));
    child_rx
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::r#loop::config::ConversationStateMode;
    use crate::r#loop::linger::LingerPolicy;
    use crate::provider::mock::MockProvider;

    /// Finding-4 regression: an explicit non-`Option` value that equals the
    /// library default must still win over a settings-derived base when its
    /// presence flag is set. The old value-vs-default sentinel reverted it
    /// back to the base value, silently discarding the explicit override.
    #[test]
    fn explicit_non_option_default_value_wins_when_present() {
        let base = AgentLoopConfig {
            schema_attempt_budget: 5,
            auto_compact_keep_recent_turns: 20,
            auto_compact_reserve_tokens: Some(90_000),
            ..AgentLoopConfig::default()
        };
        let explicit = AgentLoopConfig {
            // Each equals the library default (3 / 10 / Some(30_000)) — the
            // exact case the old sentinel misclassified as "unset".
            schema_attempt_budget: AgentLoopConfig::default().schema_attempt_budget,
            auto_compact_keep_recent_turns: AgentLoopConfig::default()
                .auto_compact_keep_recent_turns,
            auto_compact_reserve_tokens: AgentLoopConfig::default().auto_compact_reserve_tokens,
            ..AgentLoopConfig::default()
        };
        let merged = merge_agent_config(base, explicit, AgentConfigPresence::all());
        assert_eq!(
            merged.schema_attempt_budget, 3,
            "explicit schema_attempt_budget=3 (the default) must win over base=5",
        );
        assert_eq!(
            merged.auto_compact_keep_recent_turns, 10,
            "explicit auto_compact_keep_recent_turns=10 (the default) must win over base=20",
        );
        assert_eq!(
            merged.auto_compact_reserve_tokens,
            Some(30_000),
            "explicit reserve=Some(30_000) (the default) must win over base=Some(90_000)",
        );
    }

    /// Without a presence flag a non-`Option` field defers to the base —
    /// the caller never set it, so the settings-derived value stands.
    #[test]
    fn unset_non_option_field_defers_to_base() {
        let base = AgentLoopConfig {
            schema_attempt_budget: 5,
            ..AgentLoopConfig::default()
        };
        let explicit = AgentLoopConfig::default();
        let merged = merge_agent_config(base, explicit, AgentConfigPresence::default());
        assert_eq!(
            merged.schema_attempt_budget, 5,
            "no presence flag means the base (settings) value stands",
        );
    }

    /// Finding-5 regression + completeness guard: a fully-explicit config
    /// (every field non-default, all presence flags set) must overlay the
    /// base in its entirety — including `prompt_command_timeout` and
    /// `linger`, which the previous overlay omitted. This fails loudly if a
    /// future `AgentLoopConfig` field is added without a matching overlay.
    #[test]
    fn fully_explicit_config_overlays_every_field() {
        let base = AgentLoopConfig::default();
        let explicit = AgentLoopConfig {
            schema_attempt_budget: 9,
            max_iterations: Some(42),
            step_timeout: Some(Duration::from_secs(99)),
            context_window_limit: Some(123_456),
            auto_compact_reserve_tokens: Some(45_000),
            auto_compact_keep_recent_turns: 33,
            schema_tool_name: "custom_output".to_owned(),
            cache_key: Some("ck".to_owned()),
            conversation_state: ConversationStateMode::ManualReplay,
            server_compaction_threshold_tokens: Some(7_000),
            output_schema: Some(serde_json::json!({"type": "object"})),
            prompt_command_timeout: Some(Duration::from_secs(12)),
            linger: Some(LingerPolicy {
                deadline: Duration::from_secs(3),
            }),
        };
        let merged = merge_agent_config(base, explicit.clone(), AgentConfigPresence::all());
        assert_eq!(merged.schema_attempt_budget, explicit.schema_attempt_budget);
        assert_eq!(merged.max_iterations, explicit.max_iterations);
        assert_eq!(merged.step_timeout, explicit.step_timeout);
        assert_eq!(merged.context_window_limit, explicit.context_window_limit);
        assert_eq!(
            merged.auto_compact_reserve_tokens,
            explicit.auto_compact_reserve_tokens
        );
        assert_eq!(
            merged.auto_compact_keep_recent_turns,
            explicit.auto_compact_keep_recent_turns
        );
        assert_eq!(merged.schema_tool_name, explicit.schema_tool_name);
        assert_eq!(merged.cache_key, explicit.cache_key);
        assert_eq!(merged.conversation_state, explicit.conversation_state);
        assert_eq!(
            merged.server_compaction_threshold_tokens,
            explicit.server_compaction_threshold_tokens
        );
        assert_eq!(merged.output_schema, explicit.output_schema);
        assert_eq!(
            merged.prompt_command_timeout, explicit.prompt_command_timeout,
            "prompt_command_timeout must overlay onto the base (finding 5)",
        );
        assert_eq!(
            merged.linger.is_some(),
            explicit.linger.is_some(),
            "linger must overlay onto the base (finding 5)",
        );
    }

    /// The shared arming installs the estimator and the context-edit
    /// tracker on the loop context and fills an unset window from the
    /// catalog for the resolved model, leaving the reserve default
    /// untouched. This is the exact end state every launch path (root,
    /// spawn, fork, rhai) must produce — the single mechanism they all
    /// call, so the auto-compaction trigger cannot drift between them.
    #[test]
    fn arm_auto_compaction_installs_estimator_edits_and_catalog_window() {
        let model = crate::model_catalog::default_selection().model;
        let catalog_window = crate::model_catalog::smallest_context_window_for_model(model);
        assert!(
            catalog_window.is_some(),
            "test precondition: the default model must be catalogued",
        );

        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig::default();
        assert!(loop_context.token_estimator.is_none());
        assert!(loop_context.context_edits.is_none());
        assert!(config.context_window_limit.is_none());

        arm_auto_compaction(&mut loop_context, &mut config, model);

        assert!(
            loop_context.token_estimator.is_some(),
            "arming installs the token estimator the preflight needs",
        );
        assert!(
            loop_context.context_edits.is_some(),
            "arming installs the context-edit tracker (floor + compaction commit)",
        );
        assert_eq!(
            config.context_window_limit, catalog_window,
            "an unset window is filled from the catalog for the resolved model",
        );
        assert_eq!(
            config.auto_compact_reserve_tokens,
            Some(30_000),
            "the reserve default flows through untouched by arming",
        );
    }

    /// An explicit window (settings / `-c` override / any future
    /// child-policy field) is authoritative: arming only fills a `None`
    /// window, so an explicit value survives even for a catalogued model.
    #[test]
    fn arm_auto_compaction_explicit_window_beats_catalog() {
        let model = crate::model_catalog::default_selection().model;
        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig {
            context_window_limit: Some(12_345),
            ..AgentLoopConfig::default()
        };

        arm_auto_compaction(&mut loop_context, &mut config, model);

        assert_eq!(
            config.context_window_limit,
            Some(12_345),
            "an explicit window must never be overwritten by the catalog value",
        );
        assert!(loop_context.token_estimator.is_some());
        assert!(loop_context.context_edits.is_some());
    }

    /// A model absent from the catalog keeps a `None` window — the trigger
    /// stays disabled (`maybe_auto_compact` returns early on `None`),
    /// matching the root behavior, with no error. The estimator and the
    /// tracker are still installed (harmless with the trigger off).
    #[test]
    fn arm_auto_compaction_non_catalog_model_leaves_window_none() {
        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig::default();

        arm_auto_compaction(&mut loop_context, &mut config, "not-in-catalog-model-xyz");

        assert_eq!(
            config.context_window_limit, None,
            "a non-catalog model leaves the window None, disabling the trigger",
        );
        assert!(loop_context.token_estimator.is_some());
        assert!(loop_context.context_edits.is_some());
    }

    /// `install_agent_infra` anchors the session-wide [`ActionLogTree`]
    /// at the agent and registers the shared context's root [`ActionLog`]
    /// under it, so spawn/fork children can link in and federated
    /// `action_log` scope queries resolve.
    #[test]
    fn install_agent_infra_publishes_action_log_tree_with_root_log() {
        let tool_registry = Arc::new(ToolRegistry::new());
        let ctx = ToolContext::empty();
        let action_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        ctx.insert_extension(Arc::clone(&action_log));

        let agent_id = Uuid::new_v4();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let envelope = CoordinationEnvelope {
            child_policy: crate::agent::child_policy::ChildPolicy {
                messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: crate::agent::child_policy::DelegationBudget {
                    remaining_depth: 1,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            child_result_capacity: 256,
        };
        let root_cancel = tokio_util::sync::CancellationToken::new();
        let _child_rx = install_agent_infra(
            &tool_registry,
            &ctx,
            AgentInfraParts {
                registry: AgentRegistry::shared(),
                provider,
                event_store: Arc::new(EventStore::new()),
                id: agent_id,
                envelope: envelope.clone(),
                root_inbound: None,
                cancel: root_cancel.clone(),
                terminal_reclamation: true,
            },
        );

        let published = ctx
            .get_extension::<CoordinationEnvelope>()
            .expect("CoordinationEnvelope published on the shared context");
        assert_eq!(
            *published, envelope,
            "the published envelope carries the caller's values verbatim",
        );

        // W3.5: the root's run token is published for the cancellation
        // cascade — the extension must share the trigger with the token
        // the caller passed (the same one Agent::run / AgentHandle use).
        let published_cancel = ctx
            .get_extension::<crate::tools::agent::AgentCancellation>()
            .expect("AgentCancellation published on the shared context");
        assert!(!published_cancel.0.is_cancelled());
        root_cancel.cancel();
        assert!(
            published_cancel.0.is_cancelled(),
            "the published token must be the root's own run token",
        );

        let tree = ctx
            .get_extension::<ActionLogTree>()
            .expect("ActionLogTree published on the shared context");
        assert_eq!(tree.root(), agent_id, "the tree is rooted at this agent");
        let root_log = tree.log_of(agent_id).expect("root log registered");
        assert!(
            Arc::ptr_eq(&root_log, &action_log),
            "the tree's root log is the same Arc the context publishes",
        );
        assert!(tree.children_of(agent_id).is_empty(), "no children yet");
    }

    /// Defect 2 regression: a malformed skill (missing description) in a
    /// scanned directory must surface a diagnostic on the shared collector
    /// after runtime-base assembly — previously the catalog accumulated the
    /// diagnostic but nothing drained it, so the skill silently vanished.
    #[test]
    fn skill_catalog_diagnostics_reach_the_collector_after_assembly() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let broken = cwd.path().join(".norn").join("skills").join("broken");
        std::fs::create_dir_all(&broken).expect("mkdir");
        // Missing `description` → the loader emits skill-missing-description.
        std::fs::write(broken.join("SKILL.md"), "---\nname: broken\n---\nbody").expect("write");

        let mut profile = crate::profile::Profile::default();
        let base = crate::runtime_init::load_runtime_base(cwd.path(), &mut profile, None, None)
            .expect("runtime base loads");
        let ctx = ToolContext::empty();
        install_runtime_base_extensions(&ctx, &base, None, cwd.path());

        let snapshot = base.diagnostics.snapshot();
        assert!(
            snapshot
                .iter()
                .any(|d| d.code == "skill-missing-description"),
            "the malformed skill must surface a diagnostic on the shared collector: {snapshot:?}",
        );
    }

    /// A child's skill listing is gated on the `skill` tool being on the
    /// child's resolved surface: present + admitted → available; present +
    /// excluded by allow-list → unavailable; absent registry → unavailable.
    #[test]
    fn child_skill_tool_available_respects_registry_and_allow_list() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::skill::SkillTool::new()));

        assert!(child_skill_tool_available(&registry, None));
        assert!(child_skill_tool_available(
            &registry,
            Some(&["skill".to_owned(), "read".to_owned()]),
        ));
        assert!(!child_skill_tool_available(
            &registry,
            Some(&["read".to_owned()])
        ));

        let empty = ToolRegistry::new();
        assert!(!child_skill_tool_available(&empty, None));
    }

    /// The shared child-listing installer folds the "# Available Skills"
    /// section into the child's base instruction (after the base) when the
    /// skill tool is available, and leaves the instruction untouched when it
    /// is not — the same filtered listing the root gets.
    #[test]
    fn install_child_skill_listing_appends_when_available_and_skips_when_not() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("greet");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: greet the user\n---\nbody",
        )
        .unwrap();
        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);

        let mut available = LoopContext::new("You are a sub-agent.");
        install_child_skill_listing(&mut available, &catalog, true);
        let base = available.base_system_instruction();
        assert!(
            base.contains("You are a sub-agent."),
            "base retained: {base}"
        );
        assert!(
            base.contains("# Available Skills"),
            "listing present when available: {base}",
        );
        assert!(
            base.find("You are a sub-agent.") < base.find("# Available Skills"),
            "the base instruction must precede the listing: {base}",
        );

        let mut gated = LoopContext::new("You are a sub-agent.");
        install_child_skill_listing(&mut gated, &catalog, false);
        assert_eq!(
            gated.base_system_instruction(),
            "You are a sub-agent.",
            "an unavailable skill tool leaves the child's instruction untouched",
        );
    }

    /// Regression: an embedder-supplied parent base
    /// (`ParentSystemInstruction`) may already contain the listing — the
    /// root's `base_system_instruction()` includes its materialized
    /// `base_suffix`. Installing on such a base must not duplicate the
    /// "# Available Skills" section.
    #[test]
    fn install_child_skill_listing_does_not_duplicate_listing_bearing_base() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("greet");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: greet the user\n---\nbody",
        )
        .unwrap();
        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);

        // A parent base that already carries the exact generated listing,
        // as a root's materialized instruction would.
        let listing_bearing_base =
            format!("You are the parent.\n\n{}", catalog.system_prompt_listing());
        let mut child = LoopContext::new(&listing_bearing_base);
        install_child_skill_listing(&mut child, &catalog, true);

        let base = child.base_system_instruction();
        assert_eq!(
            base.matches("# Available Skills").count(),
            1,
            "the listing must appear exactly once: {base}",
        );
        assert_eq!(
            base, listing_bearing_base,
            "a listing-bearing base is left untouched",
        );

        // Idempotency of the guard itself: a second install is also a no-op.
        install_child_skill_listing(&mut child, &catalog, true);
        assert_eq!(
            child
                .base_system_instruction()
                .matches("# Available Skills")
                .count(),
            1,
            "repeat installs must not duplicate the section",
        );
    }
}
