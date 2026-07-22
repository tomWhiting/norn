//! Runtime-base extension publication and builder-overlay resolution.

use std::path::Path;
use std::sync::Arc;

use crate::error::NornError;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{Hook, HookRegistry};
use crate::r#loop::loop_context::LoopContext;
use crate::rules::engine::RuleEngine;
use crate::runtime_init::LoadedRuntimeBase;
use crate::skill::SkillCatalog;
use crate::tool::context::ToolContext;
use crate::tools::diagnostics::{DiagnosticInfra, DiagnosticStopHook};
use crate::tools::diagnostics_infra::build_diagnostic_infra_at_launch_root;
use crate::tools::lsp::{LspBackend, LspWorkspace};

/// Publish the runtime base's shared infrastructure (task store, diagnostic
/// collector, skill infra, context search paths, consent-boundary permission
/// policy) on the shared tool context.
///
/// `diagnostics` is the *resolved* collector (caller-supplied when present,
/// otherwise the base's own) so an embedder's collector is never displaced.
///
/// # Errors
///
/// Returns [`NornError::Config`] when the variant catalog cannot be built
/// from the merged settings (`prompt`/`prompt_file` conflict, unreadable
/// prompt file, unrecognised reasoning effort) -- a startup error, never
/// swallowed.
pub(crate) fn install_runtime_base_extensions(
    shared: &ToolContext,
    base: &LoadedRuntimeBase,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
    working_dir: &Path,
) -> Result<(), NornError> {
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
        working_dir.to_path_buf(),
    );
    // Report the skill catalog's load-time diagnostics (skill-shadowed,
    // skill-missing-description, skill-yaml-parse-failed, skill-io-error,
    // skill-allowed-tools-not-enforced, ...) into the shared diagnostic
    // collector so a user with a malformed skill sees a surfaced diagnostic
    // instead of the skill silently vanishing. The catalog retains its own
    // list (this copies, it does not empty), so this single assembly seam --
    // the one place both the catalog and the resolved collector are in
    // scope (the CLI reaches it through `AgentBuilder`) -- must remain the
    // only caller per build. Severity is carried on each `NornDiagnostic`,
    // so warnings stay warnings and info stays info.
    report_skill_diagnostics(&base.skill_catalog, &effective_diagnostics);
    crate::runtime_init::install_context_search_paths(shared, &base.settings, working_dir);
    // Consent boundary: without this, `permissions.deny` / `permissions.ask`
    // rules from the merged settings are silently unenforced on the
    // embedded path (the CLI installs the policy itself).
    crate::runtime_init::install_permission_policy(shared, &base.settings);
    // Agent variants: compile the merged settings over the built-ins and
    // publish the catalog used by spawn/fork/Rhai variant resolution. Prompt
    // files are read eagerly, so a broken variant fails the build here.
    crate::runtime_init::install_variant_catalog(shared, &base.settings, working_dir)?;
    Ok(())
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
    /// `None` -- the registry was already merged into the base's hooks (H13).
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
        Some(Arc::new(build_diagnostic_infra_at_launch_root(
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
    // An explicit rules engine is merged onto the runtime base's discovered
    // rules rather than replacing them. Explicit rules win ID collisions, so
    // an operator override is never silently discarded.
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
/// [`crate::agent::arming::apply_skill_listing`], the single shared mechanism
/// used by root and child launch paths.
pub(crate) fn apply_base_to_loop_context(loop_context: &mut LoopContext, base: &LoadedRuntimeBase) {
    loop_context.context_loader = Some(base.context_loader.clone());
    loop_context
        .iteration_monitor
        .clone_from(&base.iteration_monitor);
}

/// Copy accumulated skill load diagnostics into the shared collector.
///
/// This reports without emptying the catalog, so the assembly seam must call
/// it exactly once per agent.
fn report_skill_diagnostics(catalog: &SkillCatalog, collector: &DiagnosticCollector) {
    for diagnostic in catalog.diagnostics() {
        collector.report(diagnostic.clone());
    }
}
