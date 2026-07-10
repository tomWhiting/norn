//! Tool-context extension installers shared by every launch path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::base::{pin_skill_search_path, resolve_search_path};
use crate::agent::variants::VariantCatalog;
use crate::config::NornSettings;
use crate::config::permissions::PermissionPolicy;
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::r#loop::config::ToolExecutor;
use crate::skill::SkillCatalog;
use crate::tool::catalog::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};
use crate::tool::context::ToolContext;
use crate::tool::output_budget::ToolOutputBudget;
use crate::tool::registry::ToolRegistry;
use crate::tools::agent::{AgentHandles, AgentWakeRegistry, ReclaimOnResultDelivery};
use crate::tools::context_paths::ContextSearchPaths;
use crate::tools::skill::{SkillSearchPaths, WorkspaceSkillRoot};
use crate::tools::task::SharedTaskStore;

/// Immutable working-directory authority captured when the root agent starts.
///
/// Live tool working directories may change after `bash cd`; configuration
/// provenance must continue to resolve against this launch root.
pub(crate) struct LaunchWorkingDir(pub(crate) PathBuf);

/// Publish the skill search paths and skill catalog on the tool context.
pub fn install_skill_infra(
    ctx: &ToolContext,
    paths: Vec<PathBuf>,
    catalog: Arc<SkillCatalog>,
    workspace_root: PathBuf,
) {
    let paths = paths
        .into_iter()
        .map(|path| pin_skill_search_path(&workspace_root, path))
        .collect();
    ctx.insert_extension(Arc::new(SkillSearchPaths(paths)));
    ctx.insert_extension(catalog);
    ctx.insert_extension(Arc::new(WorkspaceSkillRoot(workspace_root)));
}

/// Publish settings-declared context search paths on the tool context.
pub fn install_context_search_paths(ctx: &ToolContext, settings: &NornSettings, cwd: &Path) {
    let Some(context) = settings.context.as_ref() else {
        return;
    };
    let Some(entries) = context.search_paths.as_ref() else {
        return;
    };
    if entries.is_empty() {
        return;
    }
    let paths = entries
        .iter()
        .map(|entry| resolve_search_path(cwd, entry))
        .collect();
    ctx.insert_extension(Arc::new(ContextSearchPaths(paths)));
}

/// Publish the shared task store and diagnostic collector on the tool
/// context.
///
/// The hook registry is *not* published here: the builder publishes the
/// final merged registry (programmatic + settings + diagnostic stop hook)
/// itself, after all merging is complete, so sub-agent tools always see the
/// same registry the loop dispatches.
pub fn install_runtime_extensions(
    ctx: &ToolContext,
    task_store: &Arc<SharedTaskStore>,
    diagnostics: &Arc<DiagnosticCollector>,
) {
    ctx.insert_extension(Arc::clone(task_store));
    ctx.insert_extension(Arc::clone(diagnostics));
}

/// Publish the model-aware tool-output budget on the tool context.
///
/// A `None` context window keeps conservative defaults. A known window scales
/// the default read character budget but still clamps it under the hard caps in
/// [`ToolOutputBudget`], so large-context models never imply unbounded reads.
pub fn install_tool_output_budget(ctx: &ToolContext, context_window_tokens: Option<u64>) {
    ctx.insert_extension(Arc::new(ToolOutputBudget::for_context_window(
        context_window_tokens,
    )));
}

/// Compile the merged `permissions` settings into a [`PermissionPolicy`]
/// and publish it on the shared tool context so the agent loop's tool
/// dispatch enforces the consent boundary (deny hard-blocks, ask routes
/// through the pre-tool hook chain or blocks without one, allow / no
/// match proceeds) before every tool execution.
///
/// This is the embedded-path twin of the CLI runtime builder's policy
/// install: every launch path that assembles a runtime from
/// [`NornSettings`] must call it, otherwise `permissions.deny` /
/// `permissions.ask` rules are silently unenforced.
///
/// An absent `permissions` section or one with zero rules installs
/// nothing — an empty policy permits everything, and dispatch treats a
/// missing extension as "no consent boundary configured".
pub fn install_permission_policy(ctx: &ToolContext, settings: &NornSettings) {
    let Some(permissions) = settings.permissions.as_ref() else {
        return;
    };
    let policy = PermissionPolicy::from_settings(permissions);
    if policy.is_empty() {
        return;
    }
    tracing::info!(
        rule_counts = ?policy.rule_counts(),
        "installing consent-boundary permission policy on the shared tool context",
    );
    ctx.insert_extension(Arc::new(policy));
}

/// Build the agent-variant catalog from the merged `variants` settings and
/// publish it as an `Arc<VariantCatalog>` on the shared tool context so
/// `spawn_agent` / `fork` (and grandchildren, which inherit the extension)
/// can resolve variants at spawn time.
///
/// Exactly one production call site exists: the runtime-base seam
/// (`install_runtime_base_extensions` in `agent/assembly.rs`), which the CLI
/// and every embedded builder that sets `load_runtime_base` reach through
/// `AgentBuilder::build`. Builders without `load_runtime_base` install no
/// catalog: variant use there — built-ins included — fails with the typed
/// no-catalog error (decision recorded 2026-07-07; revisit if an embedded
/// consumer needs built-ins without the runtime base).
///
/// The catalog always contains the built-in variants (`explorer`, `reviewer`,
/// `implementer`), so an absent `variants` section still installs a working
/// catalog rather than nothing — built-ins must be available with zero
/// configuration.
///
/// `cwd` anchors relative `prompt_file` paths and MUST be the same working-
/// directory value the assembly threads to
/// [`install_context_search_paths`]. Prompt files are read eagerly by
/// [`VariantCatalog::build`], so a missing or unreadable file fails loudly
/// here at startup, never later at spawn time.
///
/// # Errors
///
/// Returns [`NornError::Config`] when the catalog build fails — a
/// `prompt`/`prompt_file` conflict, an unreadable `prompt_file`, or an
/// unrecognised `reasoning_effort`. The typed
/// [`crate::agent::variants::VariantCatalogError`] message is carried through
/// verbatim; the failure is a startup error and is never swallowed.
pub fn install_variant_catalog(
    ctx: &ToolContext,
    settings: &NornSettings,
    cwd: &Path,
) -> Result<(), NornError> {
    let catalog =
        VariantCatalog::build_at_launch_root(settings.variants.as_ref(), cwd).map_err(|err| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: err.to_string(),
            })
        })?;
    ctx.insert_extension(Arc::new(catalog));
    Ok(())
}

/// Declare delivery-anchored reclamation of finished children on the
/// shared tool context (embedded / headless runtimes).
///
/// Installs the [`ReclaimOnResultDelivery`] marker: once a spawned or
/// forked child's result has been delivered through the child result
/// channel, the launch wrapper reclaims the child's terminal
/// [`crate::agent::registry::AgentRegistry`] entry and drops the
/// parent-held [`crate::tools::agent::AgentHandle`], so long-running
/// embedded processes do not pin one event store per finished child
/// forever.
///
/// Do **not** call this on runtimes where an external status observer
/// (e.g. the TUI agent status panel) displays terminal entries through a
/// hold window and reclaims them itself — see
/// [`crate::tools::agent::reclaim`] for the full ownership rule.
pub fn install_terminal_reclamation(ctx: &ToolContext) {
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));
}

/// Publish an empty [`AgentHandles`] collection on the shared tool
/// context.
///
/// `spawn_agent` and `fork` refuse to run without the collection — it is
/// where the parent's handle to each launched child is stored — so every
/// launch path that wires the agent-coordination tools must install it
/// before the first dispatch. Both
/// [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build)
/// assembly and the CLI's `build_runtime` call this single installer so
/// the two paths cannot drift.
pub fn install_agent_handles(ctx: &ToolContext) {
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
}

/// Publish the searchable tool catalog (registry tools plus any
/// consumer-contributed extras) on the registry's shared tool context.
///
/// Entries come from each tool's
/// [`Tool::catalog_entries`](crate::tool::traits::Tool::catalog_entries),
/// so field hints and composite subcommand entries are derived from the
/// tools' own schemas.
pub fn install_tool_catalog(registry: &ToolRegistry) {
    let Some(ctx) = registry.shared_context() else {
        return;
    };
    let mut entries: Vec<ToolCatalogEntry> = registry
        .names()
        .filter_map(|name| registry.get(name))
        .flat_map(crate::tool::traits::Tool::catalog_entries)
        .collect();
    if let Some(extras) = ctx.get_extension::<ToolCatalogExtras>() {
        entries.extend(extras.0.iter().cloned());
    }
    ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries))));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::PermissionSettings;
    use crate::config::permissions::PermissionDecision;

    /// Embedded-path regression: `permissions.deny` rules from merged
    /// settings must end up enforced — the compiled policy is published
    /// on the shared tool context the loop's dispatch reads.
    #[test]
    fn install_permission_policy_publishes_compiled_policy() {
        let settings = NornSettings {
            permissions: Some(PermissionSettings {
                allow: None,
                deny: Some(vec!["bash(rm *)".to_owned()]),
                ask: Some(vec!["write".to_owned()]),
            }),
            ..NornSettings::default()
        };
        let ctx = ToolContext::empty();
        install_permission_policy(&ctx, &settings);

        let policy = ctx
            .get_extension::<PermissionPolicy>()
            .expect("policy must be installed when rules are configured");
        assert!(matches!(
            policy.evaluate("bash", &json!({"command": "rm -rf /"})),
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            policy.evaluate("write", &json!({"path": "x"})),
            PermissionDecision::Ask { .. }
        ));
    }

    /// No permissions section → nothing installed (dispatch treats a
    /// missing extension as "no consent boundary configured").
    #[test]
    fn install_permission_policy_noop_without_rules() {
        let ctx = ToolContext::empty();
        install_permission_policy(&ctx, &NornSettings::default());
        assert!(ctx.get_extension::<PermissionPolicy>().is_none());

        let empty_rules = NornSettings {
            permissions: Some(PermissionSettings {
                allow: None,
                deny: None,
                ask: None,
            }),
            ..NornSettings::default()
        };
        install_permission_policy(&ctx, &empty_rules);
        assert!(
            ctx.get_extension::<PermissionPolicy>().is_none(),
            "an all-empty permissions section installs nothing",
        );
    }

    /// The single `AgentHandles` installer publishes an empty collection —
    /// the precondition `spawn_agent` / `fork` check before launching.
    #[test]
    fn install_agent_handles_publishes_empty_collection() {
        let ctx = ToolContext::empty();
        assert!(ctx.get_extension::<AgentHandles>().is_none());
        install_agent_handles(&ctx);
        let handles = ctx
            .get_extension::<AgentHandles>()
            .expect("collection must be installed");
        assert!(handles.is_empty(), "freshly installed collection is empty");
    }

    /// Embedded runtimes declare delivery-anchored reclamation by
    /// publishing the marker on the shared tool context.
    #[test]
    fn install_terminal_reclamation_publishes_marker() {
        let ctx = ToolContext::empty();
        assert!(ctx.get_extension::<ReclaimOnResultDelivery>().is_none());
        install_terminal_reclamation(&ctx);
        assert!(
            ctx.get_extension::<ReclaimOnResultDelivery>().is_some(),
            "marker must be installed",
        );
    }

    /// With zero `variants` settings the catalog still installs and carries
    /// the built-in variants — a spawn naming `explorer` must resolve even
    /// with no configuration.
    #[test]
    fn install_variant_catalog_publishes_builtins_with_no_settings() {
        let ctx = ToolContext::empty();
        assert!(ctx.get_extension::<VariantCatalog>().is_none());
        install_variant_catalog(&ctx, &NornSettings::default(), Path::new("."))
            .expect("catalog with zero settings builds the built-ins");
        let catalog = ctx
            .get_extension::<VariantCatalog>()
            .expect("catalog must be installed");
        for name in ["explorer", "reviewer", "implementer"] {
            assert!(
                catalog.get(name).is_some(),
                "built-in '{name}' must resolve after install",
            );
        }
    }

    /// A configured variant overlays the built-ins and is published on the
    /// context alongside them.
    #[test]
    fn install_variant_catalog_publishes_configured_variant() {
        use std::collections::BTreeMap;

        use crate::config::types::VariantSettings;

        let mut variants = BTreeMap::new();
        variants.insert(
            "scout".to_owned(),
            VariantSettings {
                prompt: Some("Scout the area.".to_owned()),
                ..VariantSettings::default()
            },
        );
        let settings = NornSettings {
            variants: Some(variants),
            ..NornSettings::default()
        };
        let ctx = ToolContext::empty();
        install_variant_catalog(&ctx, &settings, Path::new("."))
            .expect("configured catalog builds");
        let catalog = ctx
            .get_extension::<VariantCatalog>()
            .expect("catalog installed");
        assert!(catalog.get("scout").is_some(), "configured variant present");
        assert!(catalog.get("explorer").is_some(), "built-ins still present");
    }

    /// A catalog build failure (bad `reasoning_effort`) propagates as a typed
    /// config error — the install never swallows it and never installs a
    /// partial catalog.
    #[test]
    fn install_variant_catalog_propagates_build_failure() {
        use std::collections::BTreeMap;

        use crate::config::types::VariantSettings;

        let mut variants = BTreeMap::new();
        variants.insert(
            "hasty".to_owned(),
            VariantSettings {
                reasoning_effort: Some("turbo".to_owned()),
                ..VariantSettings::default()
            },
        );
        let settings = NornSettings {
            variants: Some(variants),
            ..NornSettings::default()
        };
        let ctx = ToolContext::empty();
        let err = install_variant_catalog(&ctx, &settings, Path::new("."))
            .expect_err("bad reasoning_effort must fail the install");
        let NornError::Config(ConfigError::InvalidConfig { reason }) = err else {
            panic!("expected Config(InvalidConfig), got {err:?}");
        };
        assert!(
            reason.contains("hasty"),
            "reason names the variant: {reason}"
        );
        assert!(
            ctx.get_extension::<VariantCatalog>().is_none(),
            "a failed build installs nothing",
        );
    }
}
