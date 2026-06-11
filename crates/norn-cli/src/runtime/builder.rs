//! Runtime assembly orchestrator for the Norn CLI (NC-004 R8).
//!
//! [`build_runtime`] is the single entry point that every consumer (print
//! mode, REPL, the future `session resume` path) calls to turn a parsed
//! [`Cli`] into a fully-populated [`RuntimeBundle`] ready for
//! `run_agent_step`. The function chains every other NC-004 helper:
//!
//! 1. Resolve the profile from `--profile` (R1).
//! 2. Apply CLI flag overrides on the profile (R2).
//! 3. Parse `-c key=value` overrides into the typed [`ConfigOverrides`]
//!    (R3).
//! 4. Load rules from `--rules` (R4).
//! 5. Build the variable store from `--variables` (R5).
//! 6. Collect MCP extension URIs from `--extension` (R6).
//! 7. Merge per-event schemas from profile and `--event-schema` (R7).
//! 8. Call [`norn::profile::from_profile`] to build the [`LoopContext`]
//!    and gated [`ToolRegistry`], then apply the runtime-only wiring:
//!    [`SimpleTokenEstimator`], [`ContextEdits`], [`RetryPolicy`],
//!    variables, and event schemas.
//!
//! The provider construction (NC-003) and tool-registry population (NC-
//! 003 / later briefs) happen outside this function — [`RuntimeBundle`]
//! carries the [`ProviderConfigOverrides`] and `extension_uris` so the
//! downstream callers can apply them.

use std::path::PathBuf;
use std::sync::Arc;

use norn::agent_loop::loop_context::LoopContext;
use norn::agent_loop::retry::RetryPolicy;
use norn::agent_loop::runner::ToolExecutor;
use norn::agent_loop::tokens::SimpleTokenEstimator;
use norn::config::{NornSettings, load_settings, merge_settings, validate_settings};
use norn::context::{ContextLoader, scan_rule_dirs};
use norn::integration::DiagnosticCollector;
use norn::integration::hooks::{HookRegistry, load_hooks_from_settings};
use norn::profile::{Profile, from_profile};
use norn::rules::engine::RuleEngine;
use norn::session::context_edit::ContextEdits;
use norn::system_prompt::{
    ExecutionMode, SystemPromptInputs, ToolPromptEntry, build_system_prompt,
};
use norn::tool::registry::ToolRegistry;

use norn::tools::task::{SharedTaskStore, TaskStore};

use norn::tools::DiskTaskStore;

use super::bundle::{RuntimeBundle, RuntimeInputs};
use super::wiring::{
    build_diagnostic_collector, build_skill_catalog, build_skill_search_paths, build_write_tool,
    iteration_monitor_from_profile,
};
use crate::cli::BuildError;
use crate::cli::Cli;
use crate::config::ConfigOverrides;
use crate::config::build_variable_store;
use crate::config::collect_extension_uris;
use crate::config::load_rule_engine;
use crate::config::merge_event_schemas;
use crate::config::overrides::{
    AppliedOverrides, apply_cli_profile_overrides, apply_config_overrides_to_loop,
    apply_loop_config_overrides, apply_settings_reasoning_to_profile,
    apply_settings_to_agent_config, apply_working_dir, default_agent_loop_config,
    overlay_cli_provider_overrides, provider_overrides_from_settings,
    retry_policy_from_settings_and_overrides,
};
use crate::config::resolve_profile;

/// Assemble the [`RuntimeBundle`] from the parsed CLI and the caller-
/// supplied registry / hooks.
///
/// Side effects: changes the process working directory when
/// `--working-dir` is set (per DESIGN.md NC3). Every other operation is
/// pure with respect to the surrounding environment.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] for any invalid flag, unknown
/// `--event-schema` type, unreadable rules / profile / event-schema
/// file, unparseable duration, or empty `--extension` URI.
pub fn build_runtime(cli: &Cli, mut inputs: RuntimeInputs) -> Result<RuntimeBundle, BuildError> {
    apply_working_dir(cli)?;

    let cwd = std::env::current_dir()?;
    // The agent's working directory is shared across `ToolContext`,
    // `LoopContext`, `VariableStore`, `RuleEngine`, and any future
    // working-dir-aware components. All share the SAME `Arc<Mutex<PathBuf>>`
    // so bash's `cd` parsing updates every consumer atomically.
    let shared_wd = norn::tool::context::SharedWorkingDir::new(cwd.clone());
    let merged_settings = load_and_merge_settings()?;

    let mut profile = resolve_profile(cli.profile.as_deref())?;
    apply_settings_reasoning_to_profile(&merged_settings, &mut profile)?;
    let applied = apply_cli_profile_overrides(cli, &mut profile)?;

    let mut config_overrides = ConfigOverrides::parse(&cli.config)?;
    if let Some(debug_api) = &cli.debug_api {
        config_overrides.debug_dump_dir = Some(resolve_debug_api_dir(debug_api));
    }

    let mut rules = match cli.rules.as_deref() {
        Some(path) => Some(load_rule_engine(path)?),
        None => None,
    };
    rules = merge_discovered_rules(rules, &cwd);
    rules = rules.map(|r| r.with_working_dir(shared_wd.clone()));

    let variables = build_variable_store(&cli.variables, shared_wd.clone())?;
    let extension_uris = collect_extension_uris(&cli.extension)?;
    let event_schemas = merge_event_schemas(&profile, &cli.event_schema)?;

    let mut agent_config = default_agent_loop_config();
    apply_settings_to_agent_config(&merged_settings, &mut agent_config)?;
    apply_config_overrides_to_loop(&config_overrides, &mut agent_config);
    apply_loop_config_overrides(cli, &mut agent_config)?;

    let mut provider_overrides = provider_overrides_from_settings(&merged_settings)?;
    overlay_cli_provider_overrides(&mut provider_overrides, &config_overrides);

    let retry_policy =
        retry_policy_from_settings_and_overrides(&merged_settings, &config_overrides)?;

    let model = profile.model.clone();
    let iteration_monitor = iteration_monitor_from_profile(&profile)?;
    let diagnostics = build_diagnostic_collector();

    let write_tool = build_write_tool(&profile, &config_overrides)?;
    inputs.registry.register(Box::new(write_tool));

    let skill_paths = build_skill_search_paths(&merged_settings, &cwd);
    let skill_catalog = build_skill_catalog(&skill_paths);
    if !skill_catalog.is_empty() {
        inputs
            .registry
            .register(Box::new(norn::tools::skill::SkillTool::new()));
    }

    // Snapshot the full (pre-gating) tool name set: `from_profile` and
    // `set_disallowed` below remove gated names from `names()`, which
    // would make the unknown-name check report false positives for
    // legitimately gated tools.
    warn_unmatched_tool_flag_names(&inputs.registry, &applied);

    let shared_task_store = build_shared_task_store(cli);

    // NH-006 R1/R2: load shell-hook config from the three settings tiers
    // and merge it with any caller-supplied programmatic [`HookRegistry`]
    // via the single library-owned assembly
    // ([`norn::runtime_init::assemble_hook_registry`]): programmatic hooks
    // register first, an outstanding programmatic `Arc` clone is folded in
    // via `HookRegistry::merge_shared` instead of being dropped (H13), and
    // [`norn::config::types::HookEntry::timeout`] is interpreted as
    // MILLISECONDS. The resulting [`Arc<HookRegistry>`] is threaded through
    // both the [`LoopContext`] and the shared [`ToolContext`] extension
    // table. When no settings files exist and no programmatic hooks were
    // supplied, `hooks` stays `None`.
    let hook_settings = load_hooks_from_settings(&cwd)?;
    let hooks =
        norn::runtime_init::assemble_hook_registry(inputs.hooks, &hook_settings, &profile, &cwd)
            .map_err(|err| BuildError::Argument(err.to_string()))?;

    let loop_context = build_loop_context(BuildLoopContextArgs {
        profile: &profile,
        registry: inputs.registry,
        rules,
        hooks: hooks.clone(),
        event_schemas,
        variables,
        retry_policy,
        iteration_monitor,
        diagnostics: Arc::clone(&diagnostics),
    });
    let (mut loop_context, mut registry) = loop_context;
    // Promote profile-derived `system_sections[0]` content into
    // `base_prefix` so iteration-top `clear_dynamic_sections` no longer
    // wipes it. NX-005 layering: `base_prefix` (Norn base + profile
    // instructions) -> always-on NORN.md -> `base_suffix` (skill
    // catalog) -> dynamic sections.
    let profile_prefix = std::mem::take(&mut loop_context.system_sections);
    loop_context.base_prefix = profile_prefix.into_iter().next().unwrap_or_default();
    loop_context.base_suffix = skill_listing_for_catalog(&skill_catalog);
    loop_context.context_loader = Some(ContextLoader::load(&cwd));
    loop_context.environment = Some(norn::system_prompt::EnvironmentConfig {
        session_id: None,
        model: model.clone(),
    });
    loop_context.rebuild_base_section();
    // Loop context shares the same `SharedWorkingDir` handle as the tool
    // context — bash's `cd` updates flow to prompt commands, hooks, rules,
    // and shell-variable execution.
    loop_context.working_dir = shared_wd.clone();
    let mut diag_ctx = super::wiring::build_tool_context_with_diagnostics(
        &cwd,
        shared_wd,
        inputs.lsp_backend.clone(),
        inputs.lsp_workspace.as_deref(),
    );
    // Workspace confinement shares libnorn's single validation
    // (`norn::agent::validate_workspace_root`): canonicalize, and reject
    // nonexistent / non-directory roots loudly instead of confining
    // nothing. The flag name is prefixed so the user can locate the
    // offending value.
    if let Some(root) = norn::agent::validate_workspace_root(cli.workspace_root.clone())
        .map_err(|err| BuildError::Argument(format!("--workspace-root: {err}")))?
    {
        diag_ctx.confine_to_workspace(root);
    }
    registry.set_context(Arc::new(diag_ctx));
    // H17: `--disallowed-tools` gates the registry with the same
    // exact-name semantics as `--allowed-tools` / `set_available`; a
    // disallowed name stays unavailable even when the allow-list names
    // it (deny wins, enforced by `ToolRegistry::set_disallowed`).
    registry.set_disallowed(applied.disallowed_tools.clone());
    publish_hooks_on_registry(&registry, hooks.as_ref());
    {
        let Some(shared) = registry.shared_context() else {
            // Structurally unreachable: `set_context` installed the shared
            // ToolContext two statements above. Guarded loudly anyway —
            // silently skipping these installs would strand the task
            // store, diagnostics, skills, and tool catalog.
            return Err(BuildError::Argument(
                "tool registry lost its shared ToolContext during assembly; \
                 runtime extensions cannot be installed"
                    .to_owned(),
            ));
        };
        norn::runtime_init::install_permission_policy(&shared, &merged_settings);
        norn::runtime_init::install_runtime_extensions(&shared, &shared_task_store, &diagnostics);
        norn::runtime_init::install_skill_infra(&shared, skill_paths, Arc::clone(&skill_catalog));
        norn::runtime_init::install_context_search_paths(&shared, &merged_settings, &cwd);
        // NA-006 populates the handles at spawn time; the collection must
        // exist before the first dispatch. Shared installer with
        // AgentBuilder assembly so the two launch paths cannot drift.
        norn::runtime_init::install_agent_handles(&shared);
    }
    norn::runtime_init::install_tool_catalog(&registry);
    let registry = Arc::new(registry);

    Ok(RuntimeBundle {
        loop_context,
        registry,
        agent_config,
        provider_overrides,
        model,
        extension_uris,
        disallowed_tools: applied.disallowed_tools,
        diagnostics,
        shared_task_store,
    })
}

/// Load the three settings layers from disk, merge them with an empty
/// CLI layer, and validate the result. The CLI layer is empty here
/// because CLI `--flag` / `-c key=value` precedence is enforced by
/// folding directly onto the typed runtime structs after this point
/// (settings supply defaults; CLI overlays them).
fn load_and_merge_settings() -> Result<NornSettings, BuildError> {
    let cwd = std::env::current_dir()?;
    let mut layers = load_settings(&cwd)?;
    let mut cli_layer = NornSettings::default();
    let merged = merge_settings(
        &mut layers.user,
        &mut layers.project,
        &mut layers.local,
        &mut cli_layer,
    );
    validate_settings(&merged)?;
    Ok(merged)
}

/// Construct the production [`SharedTaskStore`] backed by a
/// [`DiskTaskStore`] at `{norn_dir}/tasks/` with the session-derived
/// group slug.
///
/// Group slug derivation:
///
/// - `--session-name` value sanitised through the slug rules (any
///   character outside `[A-Za-z0-9_-]` becomes `-`), or
/// - the literal `"default"` when no session name is set.
///
/// The disk directory is NOT created here; [`DiskTaskStore`] defers
/// creation to the first write so a `build_runtime` call with no
/// task-tool invocation never touches `~/.norn/tasks/`.
fn build_shared_task_store(cli: &Cli) -> Arc<SharedTaskStore> {
    let root = crate::config::paths::norn_dir()
        .unwrap_or_else(|| PathBuf::from(".norn"))
        .join("tasks");
    let slug = cli
        .session_name
        .as_deref()
        .map_or_else(|| "default".to_string(), sanitise_slug);
    let disk = DiskTaskStore::new(root, slug);
    let store: Arc<dyn TaskStore> = Arc::new(disk);
    Arc::new(SharedTaskStore(store))
}

/// Coerce any input string into a value accepted by
/// [`norn::tools::task::disk::validate_slug`] by replacing
/// disallowed characters with `-` and folding empties to `default`.
fn sanitise_slug(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("default");
    }
    out
}

/// Prepend the Norn base system prompt to the loop context's
/// [`LoopContext::base_prefix`] and rebuild `system_sections[0]`.
///
/// Collects tool metadata from the registry, builds the system prompt
/// with the specified execution mode, and prepends it onto
/// [`LoopContext::base_prefix`] (which already carries the resolved
/// profile system instructions). Calling
/// [`LoopContext::rebuild_base_section`] then reassembles
/// `system_sections[0]` in the canonical NX-005 layering: Norn base
/// prompt + profile instructions, always-on NORN.md, skill catalog
/// listing. This replaces the pre-NX-005 path that called
/// `system_sections.insert(0, …)` and was silently wiped by
/// [`LoopContext::clear_dynamic_sections`] on every iteration.
pub fn apply_system_prompt(bundle: &mut RuntimeBundle, mode: ExecutionMode) {
    let tools: Vec<ToolPromptEntry> = bundle
        .registry
        .names()
        .filter_map(|name| {
            let tool = bundle.registry.get(name)?;
            Some(ToolPromptEntry {
                name: tool.name().to_owned(),
                category: tool.category(),
                description: tool.description().to_owned(),
                usage_guidance: tool.usage_guidance().map(str::to_owned),
            })
        })
        .collect();

    let has_event_schemas = bundle.loop_context.event_schemas.is_some();
    let event_schema_descriptions = bundle
        .loop_context
        .event_schemas
        .as_ref()
        .map(|set| {
            set.event_types()
                .map(|et| {
                    let schema = set
                        .get(*et)
                        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                        .unwrap_or_default();
                    let label = match et {
                        norn::agent_loop::event_schemas::EventType::Text => "text message",
                    };
                    (label.to_owned(), schema)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let inputs = SystemPromptInputs {
        mode,
        tools,
        has_output_schema: false,
        event_schema_descriptions,
        has_rules_engine: bundle.loop_context.rules.is_some(),
        has_auto_compact: bundle.loop_context.context_edits.is_some() && has_event_schemas,
    };

    let base_prompt = build_system_prompt(&inputs);
    let prefix = std::mem::take(&mut bundle.loop_context.base_prefix);
    bundle.loop_context.base_prefix = if prefix.is_empty() {
        base_prompt
    } else {
        format!("{base_prompt}\n\n{prefix}")
    };
    bundle.loop_context.rebuild_base_section();
}

/// Produce the `# Available Skills` catalog listing string, or an empty
/// string when the catalog is empty / every skill hides via
/// `disable-model-invocation`.
///
/// The returned value is stored on [`LoopContext::base_suffix`] by
/// `build_runtime` so [`LoopContext::rebuild_base_section`] places the
/// listing as the final section in `system_sections[0]`, matching
/// DESIGN.md §D7 layer order (Norn base + profile + NORN.md + skill
/// catalog).
fn skill_listing_for_catalog(catalog: &norn::skill::SkillCatalog) -> String {
    if catalog.is_empty() {
        return String::new();
    }
    catalog.system_prompt_listing()
}

/// Merge rules discovered from the rules-directory search order
/// (project `{cwd}/.norn/rules/` first, user `~/.norn/rules/` second)
/// into an existing engine, or construct a fresh engine from them
/// when no `--rules` engine was loaded.
///
/// Directories that do not exist are silently skipped by
/// [`scan_rule_dirs`]; individual parse failures are logged and dropped
/// so a single broken rule file never blocks startup (DESIGN.md §D5
/// and NX-005 R2 acceptance).
fn merge_discovered_rules(
    existing: Option<RuleEngine>,
    cwd: &std::path::Path,
) -> Option<RuleEngine> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.push(crate::config::paths::project_rules_dir(cwd));
    if let Some(user) = norn::config::paths::rules_dir() {
        dirs.push(user);
    }
    let discovered = scan_rule_dirs(&dirs);
    if discovered.is_empty() {
        return existing;
    }
    match existing {
        Some(mut engine) => {
            for rule in discovered {
                engine.add_rule(rule);
            }
            Some(engine)
        }
        None => Some(RuleEngine::new(discovered)),
    }
}

/// Names from `names` that match no entry of `registered` exactly.
/// Pure core of [`warn_unmatched_tool_flag_names`], split out for tests.
fn unmatched_tool_names<'a>(
    names: &'a [String],
    registered: &std::collections::HashSet<&str>,
) -> Vec<&'a str> {
    names
        .iter()
        .map(String::as_str)
        .filter(|name| !registered.contains(name))
        .collect()
}

/// Emit a visible stderr warning for every `--allowed-tools` /
/// `--disallowed-tools` name that matches no tool registered at
/// `build_runtime` time. Gating is exact-name, so a typo or wrong-case
/// name would otherwise enforce nothing with zero feedback. This is a
/// warning rather than a hard error because `--extension` MCP servers
/// may legitimately register additional tools after assembly.
///
/// Must run BEFORE `from_profile` / `set_disallowed` apply gating: gated
/// names disappear from `ToolRegistry::names()`, which would turn
/// legitimately gated tools into false positives.
fn warn_unmatched_tool_flag_names(registry: &ToolRegistry, applied: &AppliedOverrides) {
    let registered: std::collections::HashSet<&str> = registry.names().collect();
    for (flag, names) in [
        ("--allowed-tools", &applied.allowed_tools),
        ("--disallowed-tools", &applied.disallowed_tools),
    ] {
        for name in unmatched_tool_names(names, &registered) {
            eprintln!(
                "norn: warning: {flag} name '{name}' matches no registered tool \
                 (names are case-sensitive and matched exactly); it takes effect \
                 only if a tool with that exact name is registered later (e.g. by \
                 an --extension MCP server)",
            );
        }
    }
}

/// Publish the shared [`Arc<HookRegistry>`] onto the registry's
/// orchestrator [`norn::tool::context::ToolContext`] so dispatch sites
/// without a [`LoopContext`] reference (notably
/// `norn::tools::agent::spawn`) can retrieve it via
/// `ctx.get_extension::<HookRegistry>()`. A `None` `hooks` value is a
/// no-op so callers can call this unconditionally.
fn publish_hooks_on_registry(registry: &ToolRegistry, hooks: Option<&Arc<HookRegistry>>) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    if let Some(hooks_arc) = hooks {
        shared.insert_extension(Arc::clone(hooks_arc));
    }
}

/// Internal argument bundle for [`build_loop_context`] to keep the
/// function signature within the `clippy::too_many_arguments` budget.
struct BuildLoopContextArgs<'a> {
    profile: &'a Profile,
    registry: ToolRegistry,
    rules: Option<RuleEngine>,
    hooks: Option<Arc<HookRegistry>>,
    event_schemas: Option<norn::agent_loop::event_schemas::EventSchemaSet>,
    variables: Option<Arc<norn::integration::variables::VariableStore>>,
    retry_policy: RetryPolicy,
    iteration_monitor: Option<norn::agent_loop::iteration::IterationMonitorConfig>,
    diagnostics: Arc<DiagnosticCollector>,
}

/// Call [`from_profile`] and then layer every CLI-derived field that
/// libnorn's builder does not populate (event schemas, variables, retry
/// policy, token estimator, context edits, iteration monitor,
/// diagnostic collector).
fn build_loop_context(args: BuildLoopContextArgs<'_>) -> (LoopContext, ToolRegistry) {
    let (mut loop_context, registry) =
        from_profile(args.profile, args.registry, args.rules, args.hooks);

    loop_context.event_schemas = args.event_schemas;
    loop_context.variables = args.variables;
    loop_context.retry_policy = args.retry_policy;
    loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
    loop_context.context_edits = Some(ContextEdits::new());
    loop_context.iteration_monitor = args.iteration_monitor;
    loop_context.diagnostics = Some(args.diagnostics);

    (loop_context, registry)
}

/// Resolve the debug API dump directory from the `--debug-api` flag value.
///
/// An empty string (the `default_missing_value` sentinel) resolves to
/// `~/.norn/debug/`. Any non-empty value is used as-is.
fn resolve_debug_api_dir(value: &str) -> PathBuf {
    if value.is_empty() {
        crate::config::paths::norn_dir()
            .unwrap_or_else(|| PathBuf::from(".norn"))
            .join("debug")
    } else {
        PathBuf::from(value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use clap::Parser;
    use norn::tool::catalog::SharedToolCatalog;
    use std::time::Duration;

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    #[serial_test::serial]
    fn defaults_yield_default_profile_and_bundle() {
        // Isolate NORN_HOME so concurrent #[serial] tests writing
        // settings.json cannot poison the retry_policy assertion.
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = TempNornHome::new(tempdir);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.model, "gpt-5.5");
        assert!(bundle.extension_uris.is_empty());
        assert!(bundle.disallowed_tools.is_empty());
        assert!(bundle.loop_context.event_schemas.is_none());
        assert!(bundle.loop_context.variables.is_none());
        assert_eq!(bundle.loop_context.retry_policy.max_retries, 2);
        assert!(bundle.loop_context.rules.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn token_estimator_and_context_edits_always_set() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert!(
            bundle.loop_context.token_estimator.is_some(),
            "SimpleTokenEstimator must be wired unconditionally per NC21",
        );
        assert!(
            bundle.loop_context.context_edits.is_some(),
            "ContextEdits::new() must be wired unconditionally per NC21",
        );
    }

    #[test]
    #[serial_test::serial]
    fn model_override_flows_into_bundle_model_not_loop_context() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-m", "gpt-5.5"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.model, "gpt-5.5");
        // model isn't a field on LoopContext; reflected via bundle.model only.
        assert_eq!(bundle.loop_context.system_sections.len(), 1);
    }

    #[test]
    #[serial_test::serial]
    fn system_prompt_lands_in_loop_context_base_section() {
        // Isolate HOME and NORN_HOME so the seven-tier skill scan does
        // not observe any user-level skills, and switch the cwd to a
        // tempdir so project-level `.claude/skills/` etc. checked into
        // the repo cannot leak into `system_sections[0]`.
        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cli = cli_from(&[
            "norn",
            "-C",
            dir.path().to_str().unwrap(),
            "-S",
            "be concise",
        ]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();
        assert_eq!(bundle.loop_context.system_sections[0], "be concise");
    }

    #[test]
    #[serial_test::serial]
    fn allowed_tools_gates_registry_to_named_subset() {
        use norn::error::ToolError;
        use norn::tool::context::ToolContext;
        use norn::tool::envelope::ToolEnvelope;
        use norn::tool::scheduling::ToolEffect;
        use norn::tool::traits::{Tool, ToolOutput};

        struct StubTool {
            tool_name: String,
        }
        #[async_trait::async_trait]
        impl Tool for StubTool {
            fn name(&self) -> &str {
                &self.tool_name
            }
            fn description(&self) -> &'static str {
                "stub"
            }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success(serde_json::json!(null)))
            }
        }

        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--allowed-tools", "read"]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubTool {
            tool_name: "read".to_owned(),
        }));
        registry.register(Box::new(StubTool {
            tool_name: "write".to_owned(),
        }));

        let bundle = build_runtime(
            &cli,
            RuntimeInputs {
                registry,
                hooks: None,
                lsp_workspace: None,
                lsp_backend: None,
            },
        )
        .unwrap();
        assert!(bundle.registry.get("read").is_some());
        assert!(
            bundle.registry.get("write").is_none(),
            "write must be gated out by --allowed-tools=read",
        );
    }

    /// Minimal named tool for registry-gating tests.
    struct NamedStub {
        tool_name: String,
    }

    impl NamedStub {
        fn boxed(name: &str) -> Box<dyn Tool + Send + Sync> {
            Box::new(Self {
                tool_name: name.to_owned(),
            })
        }
    }

    #[async_trait::async_trait]
    impl Tool for NamedStub {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &'static str {
            "stub"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> norn::tool::scheduling::ToolEffect {
            norn::tool::scheduling::ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &norn::tool::envelope::ToolEnvelope,
            _ctx: &norn::tool::context::ToolContext,
        ) -> Result<norn::tool::traits::ToolOutput, norn::error::ToolError> {
            Ok(norn::tool::traits::ToolOutput::success(serde_json::json!(
                null
            )))
        }
    }

    use norn::tool::traits::Tool;

    /// H17: `--disallowed-tools` gates the registry even without an
    /// allow-list — the named tools are unavailable for lookup and
    /// dispatch.
    #[test]
    #[serial_test::serial]
    fn disallowed_tools_gates_registry() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--disallowed-tools", "write,edit"]);
        let mut registry = ToolRegistry::new();
        registry.register(NamedStub::boxed("read"));
        registry.register(NamedStub::boxed("write"));
        registry.register(NamedStub::boxed("edit"));

        let bundle = build_runtime(
            &cli,
            RuntimeInputs {
                registry,
                hooks: None,
                lsp_workspace: None,
                lsp_backend: None,
            },
        )
        .unwrap();
        assert!(bundle.registry.get("read").is_some());
        assert!(
            bundle.registry.get("write").is_none(),
            "write must be gated out by --disallowed-tools",
        );
        assert!(
            bundle.registry.get("edit").is_none(),
            "edit must be gated out by --disallowed-tools",
        );
        assert_eq!(
            bundle.disallowed_tools,
            vec!["write".to_owned(), "edit".to_owned()],
            "bundle still carries the raw list for audit surfaces",
        );
    }

    /// H17: a name present in both `--allowed-tools` and
    /// `--disallowed-tools` is unavailable — deny wins.
    #[test]
    #[serial_test::serial]
    fn disallowed_tools_wins_over_allowed_tools() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&[
            "norn",
            "--allowed-tools",
            "read,write",
            "--disallowed-tools",
            "write",
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(NamedStub::boxed("read"));
        registry.register(NamedStub::boxed("write"));

        let bundle = build_runtime(
            &cli,
            RuntimeInputs {
                registry,
                hooks: None,
                lsp_workspace: None,
                lsp_backend: None,
            },
        )
        .unwrap();
        assert!(bundle.registry.get("read").is_some());
        assert!(
            bundle.registry.get("write").is_none(),
            "--disallowed-tools must win over --allowed-tools",
        );
    }

    /// H16: a `permissions` section in settings compiles into a
    /// [`norn::config::PermissionPolicy`] published on the registry's
    /// shared context, where tool dispatch enforces it.
    #[test]
    #[serial_test::serial]
    fn permission_settings_install_policy_on_shared_context() {
        use norn::config::{PermissionDecision, PermissionPolicy};

        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "permissions": {
                    "deny": ["bash(rm *)"],
                    "ask": ["write"],
                    "allow": ["read"]
                }
            }"#,
        );

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        let policy = shared
            .get_extension::<PermissionPolicy>()
            .expect("PermissionPolicy must be installed from settings.permissions");
        assert_eq!(policy.rule_counts()["deny"], 1);
        assert_eq!(policy.rule_counts()["ask"], 1);
        assert_eq!(policy.rule_counts()["allow"], 1);
        assert!(matches!(
            policy.evaluate("bash", &serde_json::json!({"command": "rm -rf /tmp/x"})),
            PermissionDecision::Deny { .. },
        ));
    }

    /// H16: no `permissions` section (or an empty one) installs no
    /// policy — dispatch treats the missing extension as "no consent
    /// boundary configured".
    #[test]
    #[serial_test::serial]
    fn absent_or_empty_permission_settings_install_no_policy() {
        use norn::config::PermissionPolicy;

        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);

        // No settings file at all.
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        assert!(
            shared.get_extension::<PermissionPolicy>().is_none(),
            "no settings: no policy installed",
        );

        // A permissions section with zero rules.
        write_user_settings(&guard, r#"{"permissions": {}}"#);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        assert!(
            shared.get_extension::<PermissionPolicy>().is_none(),
            "empty permissions section: no policy installed",
        );
    }

    #[test]
    #[serial_test::serial]
    fn variables_flag_populates_loop_context_variables() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--variables", "project=yggdrasil"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert!(bundle.loop_context.variables.is_some());
    }

    #[test]
    #[serial_test::serial]
    fn extension_flag_populates_bundle_extension_uris() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-e", "stdio://a", "--extension", "http://b"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.extension_uris, vec!["stdio://a", "http://b"]);
    }

    #[test]
    #[serial_test::serial]
    fn empty_extension_uri_returns_argument_error() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-e", ""]);
        let result = build_runtime(&cli, RuntimeInputs::default());
        match result {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(err) => assert!(matches!(err, BuildError::Argument(_))),
        }
    }

    #[test]
    #[serial_test::serial]
    fn config_max_turns_only_fills_when_cli_did_not() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli_a = cli_from(&["norn", "--max-turns", "3", "-c", "max_turns=99"]);
        let bundle_a = build_runtime(&cli_a, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle_a.agent_config.max_iterations,
            Some(3),
            "CLI --max-turns must win over -c max_turns",
        );

        let cli_b = cli_from(&["norn", "-c", "max_turns=99"]);
        let bundle_b = build_runtime(&cli_b, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle_b.agent_config.max_iterations, Some(99));
    }

    #[test]
    #[serial_test::serial]
    fn config_schema_budget_lands_on_agent_config() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-c", "schema_budget=10"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.agent_config.schema_attempt_budget, 10);
    }

    #[test]
    #[serial_test::serial]
    fn config_base_url_flows_into_provider_overrides() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-c", "base_url=http://local"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.base_url.as_deref(),
            Some("http://local"),
        );
    }

    #[test]
    #[serial_test::serial]
    fn config_retry_max_overrides_default() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-c", "retry_max=4"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.loop_context.retry_policy.max_retries, 4);
    }

    #[test]
    #[serial_test::serial]
    fn config_retry_base_delay_overrides_default_backoff() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "-c", "retry_base_delay=2s"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.retry_policy.initial_backoff,
            Duration::from_secs(2),
        );
    }

    #[test]
    #[serial_test::serial]
    fn event_schema_cli_flag_lands_in_loop_context() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--event-schema", r#"text={"type":"object"}"#]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let set = bundle
            .loop_context
            .event_schemas
            .as_ref()
            .expect("event_schemas wired when --event-schema flag present");
        assert!(set.has(norn::agent_loop::event_schemas::EventType::Text));
    }

    #[test]
    #[serial_test::serial]
    fn invalid_event_type_propagates_as_argument_error() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--event-schema", r#"made_up={"type":"object"}"#]);
        let result = build_runtime(&cli, RuntimeInputs::default());
        match result {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(err) => assert!(matches!(err, BuildError::Argument(_))),
        }
    }

    #[test]
    #[serial_test::serial]
    fn working_dir_flag_changes_process_cwd() {
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let _bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let new_cwd = std::env::current_dir().unwrap();
        // Canonicalise both for symlink-resolution differences on macOS
        // (`/var` vs `/private/var`).
        assert_eq!(
            std::fs::canonicalize(&new_cwd).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap(),
        );
        std::env::set_current_dir(&original).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn prompt_commands_from_profile_flow_into_loop_context() {
        use norn::profile::PromptCommand;
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        std::fs::write(
            &path,
            r#"name = "p"
model = "gpt-5"
system_instructions = []

[[prompt_commands]]
name = "cwd"
command = "echo cwd"
"#,
        )
        .unwrap();
        let cli = cli_from(&["norn", "--profile", path.to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.loop_context.prompt_commands.len(), 1);
        let first: &PromptCommand = &bundle.loop_context.prompt_commands[0];
        assert_eq!(first.name, "cwd");
    }

    #[test]
    #[serial_test::serial]
    fn rules_flag_loads_rule_engine_onto_loop_context() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rust.yaml");
        std::fs::write(
            &path,
            "---\nname: Rust\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\n---\nbody",
        )
        .unwrap();
        let cli = cli_from(&["norn", "--rules", path.to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert!(bundle.loop_context.rules.is_some());
    }

    #[test]
    #[serial_test::serial]
    fn missing_rules_file_returns_argument_error() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--rules", "/no/such/rules.yaml"]);
        let result = build_runtime(&cli, RuntimeInputs::default());
        match result {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(err) => assert!(matches!(err, BuildError::Argument(_))),
        }
    }

    #[test]
    #[serial_test::serial]
    fn cli_reasoning_effort_flows_into_loop_context() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--reasoning-effort", "high"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.reasoning_effort,
            Some(norn::provider::request::ReasoningEffort::High),
        );
    }

    #[test]
    fn resolve_debug_api_dir_empty_defaults_to_norn_debug() {
        let resolved = resolve_debug_api_dir("");
        assert!(
            resolved.ends_with("debug"),
            "empty sentinel should resolve to a 'debug' subdirectory, got: {}",
            resolved.display(),
        );
    }

    #[test]
    fn resolve_debug_api_dir_custom_path_used_verbatim() {
        let resolved = resolve_debug_api_dir("/tmp/custom-debug");
        assert_eq!(resolved, PathBuf::from("/tmp/custom-debug"));
    }

    #[test]
    #[serial_test::serial]
    fn debug_api_flag_flows_into_provider_overrides() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--debug-api", "/tmp/api-dump"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.debug_dump_dir,
            Some(PathBuf::from("/tmp/api-dump")),
        );
    }

    #[test]
    #[serial_test::serial]
    fn debug_api_flag_without_value_resolves_default() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--debug-api"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let dir = bundle
            .provider_overrides
            .debug_dump_dir
            .expect("--debug-api without value must still set debug_dump_dir");
        assert!(
            dir.ends_with("debug"),
            "default debug dir should end with 'debug', got: {}",
            dir.display(),
        );
    }

    #[test]
    #[serial_test::serial]
    fn no_debug_api_flag_leaves_provider_overrides_none() {
        // Isolate NORN_HOME so a concurrent test's settings.provider.debug_dump_dir
        // cannot pollute the assertion.
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = TempNornHome::new(tempdir);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert!(bundle.provider_overrides.debug_dump_dir.is_none());
    }

    /// Set `NORN_HOME` to a temp directory for the duration of a test.
    struct TempNornHome {
        prior: Option<std::ffi::OsString>,
        tempdir: tempfile::TempDir,
    }

    impl TempNornHome {
        fn new(tempdir: tempfile::TempDir) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with the `#[serial]` markers on every consumer;
            // no concurrent reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", tempdir.path()) };
            Self { prior, tempdir }
        }

        fn path(&self) -> &std::path::Path {
            self.tempdir.path()
        }
    }

    impl Drop for TempNornHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn shared_task_store_wired_to_disk_under_norn_home() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        let session = format!("brief-na004-{}", uuid::Uuid::new_v4());

        let cli = cli_from(&["norn", "--session-name", &session]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();

        // The store is empty and lazy — the tasks directory should not
        // exist yet because no tasks have been written.
        let tasks_root = guard.path().join("tasks");
        assert!(
            !tasks_root.join(&session).exists(),
            "build_runtime must not eagerly create the group directory",
        );

        // Writing a task through the bundle's shared store creates the
        // group directory under the tempdir-rooted NORN_HOME.
        let now = chrono::Utc::now();
        let entry = norn::tools::TaskEntry {
            id: "t-brief".to_string(),
            description: "wiring smoke test".to_string(),
            status: norn::tools::TaskStatus::Pending,
            depends_on: vec![],
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            assigned_agent: None,
        };
        bundle.shared_task_store.0.create(entry).unwrap();
        let written = tasks_root.join(&session).join("t-brief.json");
        assert!(
            written.exists(),
            "task should land under {} but did not exist",
            written.display(),
        );
    }

    #[test]
    #[serial_test::serial]
    fn shared_task_store_defaults_to_default_group_without_session_name() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();

        let now = chrono::Utc::now();
        let entry = norn::tools::TaskEntry {
            id: "t-default".to_string(),
            description: "default-group smoke test".to_string(),
            status: norn::tools::TaskStatus::Pending,
            depends_on: vec![],
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            assigned_agent: None,
        };
        bundle.shared_task_store.0.create(entry).unwrap();
        assert!(
            guard
                .path()
                .join("tasks")
                .join("default")
                .join("t-default.json")
                .exists(),
            "missing-session-name path must use the literal 'default' slug",
        );
    }

    #[test]
    fn sanitise_slug_replaces_invalid_chars_and_handles_empty() {
        assert_eq!(sanitise_slug("ok-slug_1"), "ok-slug_1");
        assert_eq!(sanitise_slug("has/slash"), "has-slash");
        assert_eq!(sanitise_slug("space here"), "space-here");
        assert_eq!(sanitise_slug(""), "default");
    }

    fn bundle_with_standard_tools(cli: &Cli) -> RuntimeBundle {
        let mut inputs = RuntimeInputs::default();
        crate::runtime::register_standard_tools(&mut inputs.registry, None);
        build_runtime(cli, inputs).unwrap()
    }

    #[test]
    #[serial_test::serial]
    fn build_runtime_installs_task_store_catalog_and_handles() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        assert!(
            shared.get_extension::<SharedTaskStore>().is_some(),
            "SharedTaskStore must be installed during build_runtime",
        );
        assert!(
            shared.get_extension::<SharedToolCatalog>().is_some(),
            "SharedToolCatalog must be installed during build_runtime",
        );
        let handles = shared
            .get_extension::<norn::tools::agent::AgentHandles>()
            .expect("AgentHandles must be installed during build_runtime");
        assert!(handles.is_empty(), "AgentHandles starts empty");
    }

    /// Reclamation ownership at the runtime boundary: `build_runtime` is
    /// shared between the TUI driver and the headless print path, and the
    /// TUI's agent status panel owns terminal-entry reclamation through
    /// its hold window — so the shared builder must NOT install the
    /// [`norn::tools::agent::ReclaimOnResultDelivery`] marker. Only the
    /// headless print driver layers it on afterwards (see
    /// `crate::runtime::install_headless_reclamation`).
    #[test]
    #[serial_test::serial]
    fn build_runtime_does_not_install_reclamation_marker() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        assert!(
            shared
                .get_extension::<norn::tools::agent::ReclaimOnResultDelivery>()
                .is_none(),
            "the shared builder must leave reclamation to the per-driver wiring",
        );
    }

    /// The headless print driver's reclamation wiring publishes the
    /// marker on the same shared context `build_runtime` assembled.
    #[test]
    #[serial_test::serial]
    fn install_headless_reclamation_publishes_marker_on_bundle_context() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        crate::runtime::install_headless_reclamation(&bundle.registry);
        let shared = bundle
            .registry
            .shared_context()
            .expect("registry exposes a shared context");
        assert!(
            shared
                .get_extension::<norn::tools::agent::ReclaimOnResultDelivery>()
                .is_some(),
            "headless wiring must install the reclamation marker",
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn task_tool_resolves_after_build_runtime() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = bundle_with_standard_tools(&cli);
        let executor: &dyn ToolExecutor = &*bundle.registry;
        let out = executor
            .execute(
                "task",
                "test-call",
                serde_json::json!({"action": "create", "description": "wired"}),
            )
            .await
            .expect("task tool dispatch succeeds once SharedTaskStore is installed");
        assert_eq!(out["task"]["status"], "pending");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn tool_search_resolves_after_build_runtime() {
        // Isolate NORN_HOME (serialised with every other NORN_HOME
        // consumer) so build_runtime reads hermetic settings, not the
        // developer's ~/.norn or a concurrent test's half-written file.
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = bundle_with_standard_tools(&cli);
        let executor: &dyn ToolExecutor = &*bundle.registry;
        let out = executor
            .execute("tool_search", "test-call", serde_json::json!({"query": ""}))
            .await
            .expect("tool_search dispatch succeeds once SharedToolCatalog is installed");
        let results = out["results"]
            .as_array()
            .expect("tool_search returns a results array");
        assert!(
            !results.is_empty(),
            "empty query returns the full catalogue"
        );
        let names: Vec<&str> = results
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "empty query returns alphabetical results");
    }

    #[tokio::test]
    async fn spawn_agent_gets_past_infra_after_install() {
        use norn::provider::mock::MockProvider;
        use norn::provider::traits::Provider;

        let cli = cli_from(&["norn"]);
        let bundle = bundle_with_standard_tools(&cli);
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        crate::runtime::install_agent_tool_infra(
            &bundle.registry,
            provider,
            Arc::new(norn::session::store::EventStore::new()),
            uuid::Uuid::new_v4(),
            Arc::clone(&bundle.registry),
            norn::agent::registry::AgentRegistry::shared(),
        );

        let executor: &dyn ToolExecutor = &*bundle.registry;
        let result = executor
            .execute(
                "spawn_agent",
                "test-call",
                serde_json::json!({"task": "do x", "model": "gpt-5.5", "role": "worker"}),
            )
            .await;
        if let Err(err) = result {
            let reason = err.to_string();
            assert!(
                !reason.contains("AgentToolInfra"),
                "spawn_agent must get past infra_from once AgentToolInfra is installed: {reason}",
            );
        }
    }

    /// Convenience: drop a user-level `settings.json` under
    /// `$NORN_HOME` (the [`TempNornHome`] tempdir). Returns a handle so
    /// the test can keep the env-var override live for the call to
    /// [`build_runtime`].
    fn write_user_settings(guard: &TempNornHome, body: &str) {
        let path = guard.path().join("settings.json");
        std::fs::write(&path, body).unwrap();
    }

    /// H13 regression at the CLI boundary: a programmatic [`HookRegistry`]
    /// whose `Arc` has an outstanding clone (exactly what an embedder or
    /// `AgentBuilder` produces) must survive the merge with settings shell
    /// hooks. The deleted CLI-local assembly silently replaced it with an
    /// empty registry; the converged library assembly folds it in via
    /// `HookRegistry::merge_shared`.
    #[test]
    #[serial_test::serial]
    fn programmatic_hooks_survive_outstanding_arc_clone_with_shell_hooks() {
        use norn::integration::hooks::{Hook, HookOutcome, HookRegistry, StopHook};

        struct BlockingStop;

        #[async_trait::async_trait]
        impl StopHook for BlockingStop {
            async fn on_stop(&self, _final_text: &str) -> HookOutcome {
                HookOutcome::Block {
                    reason: "programmatic".to_owned(),
                }
            }
        }

        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "command": "true", "timeout": 1000 }
                    ]
                }
            }"#,
        );

        let mut programmatic = HookRegistry::new();
        programmatic.register(Hook::Stop(Box::new(BlockingStop)));
        let programmatic = Arc::new(programmatic);
        // The clone an embedder legitimately retains across build_runtime.
        let outstanding = Arc::clone(&programmatic);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(
            &cli,
            RuntimeInputs {
                registry: ToolRegistry::new(),
                hooks: Some(programmatic),
                lsp_workspace: None,
                lsp_backend: None,
            },
        )
        .unwrap();

        let hooks = bundle
            .loop_context
            .hooks
            .as_ref()
            .expect("merged registry installed on the loop context");
        assert_eq!(hooks.pre_tool_len(), 1, "settings shell hook registered");
        assert_eq!(
            hooks.stop_len(),
            1,
            "programmatic hooks must survive the merge even when the Arc \
             has outstanding clones (H13)",
        );
        drop(outstanding);
    }

    /// `--workspace-root` confines the registry's shared `ToolContext`.
    #[test]
    #[serial_test::serial]
    fn workspace_root_flag_confines_shared_tool_context() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let root = tempfile::tempdir().unwrap();
        let cli = cli_from(&["norn", "--workspace-root", root.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        assert_eq!(
            shared.workspace_root(),
            Some(std::fs::canonicalize(root.path()).unwrap().as_path()),
            "--workspace-root must land on ToolContext::workspace_root",
        );
    }

    /// Without the flag, path resolution stays unconfined.
    #[test]
    #[serial_test::serial]
    fn no_workspace_root_flag_leaves_tools_unconfined() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        assert!(shared.workspace_root().is_none());
    }

    /// A nonexistent `--workspace-root` is a hard argument error, not a
    /// silently-ignored confinement.
    #[test]
    #[serial_test::serial]
    fn workspace_root_missing_directory_is_argument_error() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = cli_from(&["norn", "--workspace-root", "/no/such/workspace-root"]);
        match build_runtime(&cli, RuntimeInputs::default()) {
            Ok(_) => panic!("expected Argument error for missing workspace root"),
            Err(BuildError::Argument(reason)) => {
                assert!(reason.contains("--workspace-root"), "reason: {reason}");
                assert!(
                    reason.contains("/no/such/workspace-root"),
                    "reason: {reason}",
                );
            }
            Err(other @ BuildError::Auth(_)) => panic!("expected Argument, got {other:?}"),
        }
    }

    /// `--workspace-root` pointing at a file (not a directory) is rejected.
    #[test]
    #[serial_test::serial]
    fn workspace_root_file_is_argument_error() {
        let _norn_home = TempNornHome::new(tempfile::tempdir().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain-file");
        std::fs::write(&file, "x").unwrap();
        let cli = cli_from(&["norn", "--workspace-root", file.to_str().unwrap()]);
        match build_runtime(&cli, RuntimeInputs::default()) {
            Ok(_) => panic!("expected Argument error for non-directory workspace root"),
            Err(BuildError::Argument(reason)) => {
                assert!(reason.contains("not a directory"), "reason: {reason}");
            }
            Err(other @ BuildError::Auth(_)) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_tool_names_reports_only_unregistered() {
        let registered: std::collections::HashSet<&str> =
            ["read", "write", "bash"].into_iter().collect();
        let names = vec![
            "read".to_owned(),
            "Bash".to_owned(),
            "wrte".to_owned(),
            "write".to_owned(),
        ];
        assert_eq!(
            unmatched_tool_names(&names, &registered),
            vec!["Bash", "wrte"],
            "wrong-case and misspelled names must be reported; exact \
             matches must not",
        );
    }

    #[test]
    fn unmatched_tool_names_empty_input_reports_nothing() {
        let registered: std::collections::HashSet<&str> = ["read"].into_iter().collect();
        assert!(unmatched_tool_names(&[], &registered).is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn settings_agent_fields_flow_into_agent_loop_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "agent": {
                    "max_turns": 11,
                    "step_timeout": "45s",
                    "schema_budget": 7,
                    "context_window": 250000,
                    "compact_threshold": 0.6,
                    "compact_keep_turns": 8,
                    "conversation_state": "provider_threaded",
                    "server_compaction_threshold_tokens": 190000
                }
            }"#,
        );

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.agent_config.max_iterations, Some(11));
        assert_eq!(
            bundle.agent_config.step_timeout,
            Some(Duration::from_secs(45))
        );
        assert_eq!(bundle.agent_config.schema_attempt_budget, 7);
        assert_eq!(bundle.agent_config.context_window_limit, Some(250_000));
        assert!(
            (bundle.agent_config.auto_compact_threshold_pct.unwrap() - 0.6).abs() < f64::EPSILON,
        );
        assert_eq!(bundle.agent_config.auto_compact_keep_recent_turns, 8);
        assert_eq!(
            bundle.agent_config.conversation_state,
            norn::agent_loop::config::ConversationStateMode::ProviderThreaded,
        );
        assert_eq!(
            bundle.agent_config.server_compaction_threshold_tokens,
            Some(190_000),
        );
    }

    #[test]
    #[serial_test::serial]
    fn settings_provider_fields_flow_into_provider_overrides() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "provider": {
                    "base_url": "https://from.settings/v1",
                    "timeout": "12s",
                    "max_retries": 4,
                    "options": {"k":"v"},
                    "debug_dump_dir": "/tmp/from-settings"
                }
            }"#,
        );

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.base_url.as_deref(),
            Some("https://from.settings/v1"),
        );
        assert_eq!(
            bundle.provider_overrides.request_timeout,
            Some(Duration::from_secs(12)),
        );
        assert_eq!(bundle.provider_overrides.max_retries, Some(4));
        assert_eq!(
            bundle
                .provider_overrides
                .provider_options
                .as_ref()
                .and_then(|v| v.get("k"))
                .and_then(serde_json::Value::as_str),
            Some("v"),
        );
        assert_eq!(
            bundle.provider_overrides.debug_dump_dir,
            Some(PathBuf::from("/tmp/from-settings")),
        );
    }

    #[test]
    #[serial_test::serial]
    fn settings_retry_fields_produce_retry_policy() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "retry": {
                    "max_retries": 5,
                    "base_delay": "3s",
                    "backoff_multiplier": 1.5
                }
            }"#,
        );

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.loop_context.retry_policy.max_retries, 5);
        assert_eq!(
            bundle.loop_context.retry_policy.initial_backoff,
            Duration::from_secs(3),
        );
        assert!((bundle.loop_context.retry_policy.backoff_multiplier - 1.5).abs() < f64::EPSILON,);
    }

    #[test]
    #[serial_test::serial]
    fn cli_dash_c_max_turns_wins_over_settings() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"agent":{"max_turns":10}}"#);

        let cli = cli_from(&["norn", "-c", "max_turns=5"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.agent_config.max_iterations,
            Some(5),
            "-c overrides settings"
        );
    }

    #[test]
    #[serial_test::serial]
    fn cli_dash_c_base_url_wins_over_settings() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{"provider":{"base_url":"https://from.settings"}}"#,
        );

        let cli = cli_from(&["norn", "-c", "base_url=https://from.cli"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.base_url.as_deref(),
            Some("https://from.cli"),
        );
    }

    #[test]
    #[serial_test::serial]
    fn settings_reasoning_effort_used_when_profile_has_none() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"agent":{"reasoning_effort":"low"}}"#);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.reasoning_effort,
            Some(norn::provider::request::ReasoningEffort::Low),
        );
    }

    #[test]
    #[serial_test::serial]
    fn profile_reasoning_effort_wins_over_settings() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"agent":{"reasoning_effort":"low"}}"#);

        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = profile_dir.path().join("p.toml");
        std::fs::write(
            &profile_path,
            r#"name = "p"
model = "gpt-5.5"
reasoning_effort = "high"
system_instructions = []
"#,
        )
        .unwrap();
        let cli = cli_from(&["norn", "--profile", profile_path.to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.reasoning_effort,
            Some(norn::provider::request::ReasoningEffort::High),
            "profile.reasoning_effort wins over settings.agent.reasoning_effort",
        );
    }

    #[test]
    #[serial_test::serial]
    fn no_settings_no_profile_reasoning_falls_through() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = TempNornHome::new(tempdir);
        // No settings file written — user layer is empty.

        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = profile_dir.path().join("p.toml");
        std::fs::write(
            &profile_path,
            r#"name = "p"
model = "gpt-5.5"
reasoning_effort = "medium"
system_instructions = []
"#,
        )
        .unwrap();
        let cli = cli_from(&["norn", "--profile", profile_path.to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.reasoning_effort,
            Some(norn::provider::request::ReasoningEffort::Medium),
        );
    }

    #[test]
    #[serial_test::serial]
    fn cli_reasoning_effort_wins_over_settings_and_profile() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"agent":{"reasoning_effort":"low"}}"#);

        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = profile_dir.path().join("p.toml");
        std::fs::write(
            &profile_path,
            r#"name = "p"
model = "gpt-5.5"
reasoning_effort = "medium"
system_instructions = []
"#,
        )
        .unwrap();
        let cli = cli_from(&[
            "norn",
            "--profile",
            profile_path.to_str().unwrap(),
            "--reasoning-effort",
            "high",
        ]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.loop_context.reasoning_effort,
            Some(norn::provider::request::ReasoningEffort::High),
        );
    }

    #[test]
    #[serial_test::serial]
    fn malformed_settings_propagates_as_argument_error() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, "{ this is not json }");

        let cli = cli_from(&["norn"]);
        match build_runtime(&cli, RuntimeInputs::default()) {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(err) => assert!(matches!(err, BuildError::Argument(_))),
        }
    }

    #[test]
    #[serial_test::serial]
    fn settings_skill_search_paths_install_with_defaults() {
        use norn::tools::SkillSearchPaths;
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"skills":{"search_paths":["./custom-skills"]}}"#);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        let paths = shared
            .get_extension::<SkillSearchPaths>()
            .expect("SkillSearchPaths installed when settings provide entries");

        let cwd = std::env::current_dir().unwrap();
        let expected_custom = cwd.join("custom-skills");
        let expected_project = cwd.join(".norn").join("skills");
        let expected_user = guard.path().join("skills");

        assert_eq!(
            paths.0.first(),
            Some(&expected_custom),
            "settings paths must be prepended; got {:?}",
            paths.0,
        );
        assert!(
            paths.0.contains(&expected_project),
            "project-default `.norn/skills/` missing; got {:?}",
            paths.0,
        );
        assert!(
            paths.0.contains(&expected_user),
            "user-default `$NORN_HOME/skills/` missing; got {:?}",
            paths.0,
        );
    }

    #[test]
    #[serial_test::serial]
    fn no_settings_skill_paths_still_installs_defaults() {
        use norn::tools::SkillSearchPaths;
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        // No settings file written.

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        let paths = shared
            .get_extension::<SkillSearchPaths>()
            .expect("SkillSearchPaths installed even without settings");
        let cwd = std::env::current_dir().unwrap();
        assert!(paths.0.contains(&cwd.join(".norn").join("skills")));
        assert!(paths.0.contains(&guard.path().join("skills")));
    }

    #[test]
    #[serial_test::serial]
    fn settings_context_search_paths_install() {
        use norn::tools::ContextSearchPaths;
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"context":{"search_paths":["./docs"]}}"#);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        let paths = shared
            .get_extension::<ContextSearchPaths>()
            .expect("ContextSearchPaths installed when settings provide entries");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(paths.0, vec![cwd.join("docs")]);
    }

    #[test]
    #[serial_test::serial]
    fn no_settings_context_paths_leaves_extension_unset() {
        use norn::tools::ContextSearchPaths;
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = TempNornHome::new(tempdir);
        // No settings written — context.search_paths absent.

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        assert!(
            shared.get_extension::<ContextSearchPaths>().is_none(),
            "absent context.search_paths must leave the extension uninstalled",
        );
    }

    #[test]
    #[serial_test::serial]
    fn settings_rate_limit_flows_into_provider_overrides() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"provider":{"rate_limit":120}}"#);

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.provider_overrides.rate_limit, Some(120));
    }

    #[test]
    #[serial_test::serial]
    fn no_settings_rate_limit_leaves_override_none() {
        let tempdir = tempfile::tempdir().unwrap();
        let _guard = TempNornHome::new(tempdir);
        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(bundle.provider_overrides.rate_limit, None);
    }

    #[test]
    #[serial_test::serial]
    fn settings_rate_retry_knobs_flow_into_provider_overrides() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "provider": {
                    "rate_limit_interval": "90s",
                    "retry_backoff": "500ms",
                    "retry_after_ceiling": "2m",
                    "runner_path": "/opt/tools/claude-custom"
                }
            }"#,
        );

        let cli = cli_from(&["norn"]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.rate_limit_interval,
            Some(Duration::from_secs(90)),
        );
        assert_eq!(
            bundle.provider_overrides.retry_backoff,
            Some(Duration::from_millis(500)),
        );
        assert_eq!(
            bundle.provider_overrides.retry_after_ceiling,
            Some(Duration::from_mins(2)),
        );
        assert_eq!(
            bundle.provider_overrides.runner_path,
            Some(PathBuf::from("/opt/tools/claude-custom")),
        );
    }

    #[test]
    #[serial_test::serial]
    fn cli_dash_c_rate_retry_knobs_win_over_settings() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(
            &guard,
            r#"{
                "provider": {
                    "rate_limit_interval": "90s",
                    "retry_backoff": "500ms",
                    "retry_after_ceiling": "2m"
                }
            }"#,
        );

        let cli = cli_from(&[
            "norn",
            "-c",
            "rate_limit_interval=30s",
            "-c",
            "retry_backoff=3s",
            "-c",
            "retry_after_ceiling=10m",
        ]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        assert_eq!(
            bundle.provider_overrides.rate_limit_interval,
            Some(Duration::from_secs(30)),
            "-c overrides settings",
        );
        assert_eq!(
            bundle.provider_overrides.retry_backoff,
            Some(Duration::from_secs(3)),
            "-c overrides settings",
        );
        assert_eq!(
            bundle.provider_overrides.retry_after_ceiling,
            Some(Duration::from_mins(10)),
            "-c overrides settings",
        );
    }

    #[test]
    #[serial_test::serial]
    fn zero_rate_limit_interval_in_settings_rejected_at_build() {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = TempNornHome::new(tempdir);
        write_user_settings(&guard, r#"{"provider":{"rate_limit_interval":"0s"}}"#);

        let cli = cli_from(&["norn"]);
        match build_runtime(&cli, RuntimeInputs::default()) {
            Ok(_) => panic!("expected Argument error for zero rate_limit_interval"),
            Err(BuildError::Argument(reason)) => {
                assert!(
                    reason.contains("provider.rate_limit_interval"),
                    "reason: {reason}",
                );
            }
            Err(other @ BuildError::Auth(_)) => panic!("expected Argument, got {other:?}"),
        }
    }

    /// Set both `HOME` and `NORN_HOME` to (different) temp directories
    /// for the duration of a test so that the seven-tier skill scan
    /// observes empty user-level trees (`~/.norn/skills/`,
    /// `~/.agents/skills/`, `~/.claude/skills/`) regardless of what
    /// exists on the host machine. Restores prior values on drop.
    struct IsolatedHome {
        prior_norn: Option<std::ffi::OsString>,
        prior_home: Option<std::ffi::OsString>,
        norn_home: tempfile::TempDir,
        _home: tempfile::TempDir,
    }

    impl IsolatedHome {
        fn new() -> Self {
            let norn_home = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();
            let prior_norn = std::env::var_os("NORN_HOME");
            let prior_home = std::env::var_os("HOME");
            // SAFETY: every consumer is `#[serial]` so no concurrent
            // reader observes the env mutation.
            unsafe {
                std::env::set_var("NORN_HOME", norn_home.path());
                std::env::set_var("HOME", home.path());
            }
            Self {
                prior_norn,
                prior_home,
                norn_home,
                _home: home,
            }
        }

        fn norn_home_path(&self) -> &std::path::Path {
            self.norn_home.path()
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            unsafe {
                match &self.prior_norn {
                    Some(val) => std::env::set_var("NORN_HOME", val),
                    None => std::env::remove_var("NORN_HOME"),
                }
                match &self.prior_home {
                    Some(val) => std::env::set_var("HOME", val),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    /// Run `build_runtime` from a temp cwd while keeping the prior cwd
    /// restored on return. Mirrors the explicit cwd-restore pattern in
    /// `working_dir_flag_changes_process_cwd`.
    ///
    /// Returns the temp dir handle (alive for the test's lifetime) and
    /// the canonical cwd that `build_runtime` resolved internally — the
    /// canonical form is needed because `apply_working_dir` on macOS
    /// resolves `/var/...` to `/private/var/...` so a direct
    /// `dir.path()` comparison would not match the installed
    /// `SkillSearchPaths` entries.
    fn build_runtime_in_temp_cwd(cli_args: &[&str]) -> (tempfile::TempDir, PathBuf, RuntimeBundle) {
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut args: Vec<&str> = vec!["norn", "-C"];
        let dir_str = dir.path().to_str().unwrap();
        args.push(dir_str);
        args.extend_from_slice(cli_args);
        let cli = cli_from(&args);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let resolved_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&original).unwrap();
        (dir, resolved_cwd, bundle)
    }

    // ------------------------------------------------------------------
    // R2: seven-tier search paths + SkillCatalog installation
    // ------------------------------------------------------------------

    #[test]
    #[serial_test::serial]
    fn skill_search_paths_follow_d1_ordering() {
        use norn::tools::SkillSearchPaths;
        let _isolate = IsolatedHome::new();
        let (_cwd, resolved_cwd, bundle) = build_runtime_in_temp_cwd(&[]);
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        let paths = shared
            .get_extension::<SkillSearchPaths>()
            .expect("SkillSearchPaths installed");

        let project_norn = resolved_cwd.join(".norn").join("skills");
        let project_agents = resolved_cwd.join(".agents").join("skills");
        let project_claude = resolved_cwd.join(".claude").join("skills");
        let project_meridian = resolved_cwd.join(".meridian").join("skills");

        let idx = |needle: &std::path::Path| {
            paths.0.iter().position(|p| p == needle).unwrap_or_else(|| {
                panic!(
                    "expected {} in skill paths, got {:?}",
                    needle.display(),
                    paths.0
                )
            })
        };

        let i_norn = idx(&project_norn);
        let i_agents = idx(&project_agents);
        let i_claude = idx(&project_claude);
        let i_meridian = idx(&project_meridian);

        assert!(
            i_norn < i_agents,
            "project .norn must precede .agents: {:?}",
            paths.0
        );
        assert!(
            i_agents < i_claude,
            "project .agents must precede .claude: {:?}",
            paths.0
        );
        assert!(
            i_claude < i_meridian,
            "project .claude must precede .meridian: {:?}",
            paths.0
        );
    }

    #[test]
    #[serial_test::serial]
    fn build_runtime_installs_skill_catalog_extension() {
        use norn::skill::SkillCatalog;
        let _isolate = IsolatedHome::new();
        let (_cwd, _resolved_cwd, bundle) = build_runtime_in_temp_cwd(&[]);
        let shared = bundle
            .registry
            .shared_context()
            .expect("shared context present");
        assert!(
            shared.get_extension::<SkillCatalog>().is_some(),
            "Arc<SkillCatalog> must be installed during build_runtime",
        );
    }

    // ------------------------------------------------------------------
    // R3 + R4: conditional registration and catalog listing
    // ------------------------------------------------------------------

    fn write_skill_in_project(cwd: &std::path::Path, name: &str, description: &str) {
        let dir = cwd.join(".norn").join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\ndescription: {description}\n---\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn no_skills_omits_skill_tool_and_listing() {
        let _isolate = IsolatedHome::new();
        let (_cwd, _resolved_cwd, bundle) = build_runtime_in_temp_cwd(&[]);
        assert!(
            bundle.registry.get("skill").is_none(),
            "SkillTool must not be registered when no skills exist",
        );
        for section in &bundle.loop_context.system_sections {
            assert!(
                !section.contains("# Available Skills"),
                "no skills section expected when catalog is empty, got: {section}",
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn skills_present_register_skill_tool_and_inject_listing() {
        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_skill_in_project(dir.path(), "deploy", "Deploy the service.");
        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        assert!(
            bundle.registry.get("skill").is_some(),
            "SkillTool must be registered when a skill exists",
        );
        let base = &bundle.loop_context.system_sections[0];
        assert!(
            base.contains("# Available Skills"),
            "base section must include the skills listing, got: {base}",
        );
        assert!(
            base.contains("- deploy: Deploy the service."),
            "listing must include the discovered skill, got: {base}",
        );
    }

    #[test]
    #[serial_test::serial]
    fn skill_listing_appended_after_profile_instructions() {
        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_skill_in_project(dir.path(), "deploy", "Deploy the service.");

        let cli = cli_from(&[
            "norn",
            "-C",
            dir.path().to_str().unwrap(),
            "-S",
            "profile instruction",
        ]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        let base = &bundle.loop_context.system_sections[0];
        let profile_pos = base
            .find("profile instruction")
            .expect("profile instruction present");
        let listing_pos = base.find("# Available Skills").expect("listing present");
        assert!(
            profile_pos < listing_pos,
            "skills listing must follow profile instructions; got: {base}",
        );
        // Separator: a blank line between profile prose and the heading.
        assert!(
            base.contains("profile instruction\n\n# Available Skills"),
            "listing must be separated from profile instruction by a blank line; got: {base}",
        );
    }

    #[test]
    #[serial_test::serial]
    fn skill_user_tier_picked_up_from_norn_home() {
        let isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Write a user-tier skill under ~/.norn/skills/ via NORN_HOME.
        let user_skills = isolate.norn_home_path().join("skills").join("notes");
        std::fs::create_dir_all(&user_skills).unwrap();
        std::fs::write(
            user_skills.join("SKILL.md"),
            "---\ndescription: User-level skill.\n---\nbody\n",
        )
        .unwrap();

        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        assert!(
            bundle.registry.get("skill").is_some(),
            "user-tier skill must trigger SkillTool registration",
        );
        let base = &bundle.loop_context.system_sections[0];
        assert!(
            base.contains("- notes: User-level skill."),
            "user-tier skill must appear in listing, got: {base}",
        );
    }

    // ------------------------------------------------------------------
    // NX-005 R1, R3, R5: NORN.md context surface and compaction survival
    // ------------------------------------------------------------------

    #[test]
    #[serial_test::serial]
    fn project_norn_md_appears_in_system_prompt() {
        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NORN.md"), "project-conventions").unwrap();
        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        let base = &bundle.loop_context.system_sections[0];
        assert!(
            base.contains("project-conventions"),
            "project NORN.md content must surface in system_sections[0], got: {base}",
        );
    }

    #[test]
    #[serial_test::serial]
    fn user_norn_md_appears_before_project_norn_md_in_system_prompt() {
        let isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(isolate.norn_home_path().join("NORN.md"), "USER-LEVEL").unwrap();
        std::fs::write(dir.path().join("NORN.md"), "PROJECT-LEVEL").unwrap();

        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        let base = &bundle.loop_context.system_sections[0];
        let user_pos = base
            .find("USER-LEVEL")
            .expect("user NORN.md content present");
        let project_pos = base
            .find("PROJECT-LEVEL")
            .expect("project NORN.md content present");
        assert!(
            user_pos < project_pos,
            "user-level NORN.md must appear before project-level NORN.md; got: {base}",
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn project_rule_activates_on_matching_path_change() {
        use norn::rules::types::{PathOperation, RuntimeEvent};

        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = dir.path().join(".norn").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("rust-conventions.md"),
            "---\nname: Rust conventions\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\n---\nNo unwrap in library code.",
        )
        .unwrap();

        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        let engine = bundle
            .loop_context
            .rules
            .as_ref()
            .expect("scan_rule_dirs must populate the engine when a project rule exists");
        let injections = engine
            .process_event(&RuntimeEvent::PathChanged {
                path: "src/lib.rs".to_owned(),
                operation: PathOperation::Read,
            })
            .await;
        assert_eq!(
            injections.len(),
            1,
            "matching PathChanged must produce one injection",
        );
        assert_eq!(injections[0].rule_id.as_str(), "rust-conventions");
        assert!(injections[0].content.contains("No unwrap"));
    }

    #[test]
    #[serial_test::serial]
    fn always_on_context_survives_simulated_compaction() {
        let _isolate = IsolatedHome::new();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NORN.md"), "survive-me").unwrap();

        let cli = cli_from(&["norn", "-C", dir.path().to_str().unwrap()]);
        let mut bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        std::env::set_current_dir(&original).unwrap();

        // Pre-condition: NORN.md content surfaces.
        assert!(
            bundle.loop_context.system_sections[0].contains("survive-me"),
            "precondition: NORN.md content must be in base section before compaction",
        );

        // Simulate a stale dynamic section accumulating across iterations.
        bundle
            .loop_context
            .append_system_section("dynamic-rule-injection");
        assert_eq!(bundle.loop_context.system_sections.len(), 2);

        // Simulate compaction: the runner truncates dynamic sections at
        // the top of each iteration.
        bundle.loop_context.clear_dynamic_sections();
        assert_eq!(
            bundle.loop_context.system_sections.len(),
            1,
            "clear_dynamic_sections must truncate to base only",
        );
        assert!(
            bundle.loop_context.system_sections[0].contains("survive-me"),
            "always-on NORN.md content must survive clear_dynamic_sections",
        );
    }
}
