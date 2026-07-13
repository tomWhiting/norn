//! Spawn-time variant resolution (brief `agent-variants` R3).
//!
//! Owns the argument-resolution steps of the spawn surface — variant
//! lookup, role and model resolution, and parent-model ground truth —
//! split out of `spawn.rs` per the 500-line file budget. The rhai spawn
//! surface reuses the same pieces so the two paths cannot drift.
//!
//! Every failure is a typed [`ToolError`] whose message names the exact
//! configuration key or argument that fixes it. There are no fallbacks:
//! an unknown variant lists the catalog, a model-requiring variant
//! without a model names `variants.<name>.model`, and a parent whose own
//! model cannot be resolved from runtime ground truth is an error —
//! never a re-read of settings, never an invented literal.

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::registry::AgentRegistry;
use crate::agent::variants::{ResolvedVariant, VariantCatalog};
use crate::error::ToolError;
use crate::tool::context::ToolContext;

use super::infra::AgentModel;

/// Look up `name` in the variant catalog.
///
/// # Errors
///
/// - No catalog available on this agent's context → typed wiring error.
///   The assembly path did not install one: runtimes assembled without
///   the runtime base (`load_runtime_base`) install no catalog, and
///   child contexts inherit the parent's catalog at construction — so a
///   missing catalog on a child means the root never had one (a child
///   whose parent HAD a catalog but lacks it itself would be a
///   forwarding bug in `spawn_context`/`fork_context`).
/// - Unknown name → typed error listing every available variant
///   (built-ins ∪ configured), sorted.
pub(crate) fn lookup_variant<'a>(
    catalog: Option<&'a VariantCatalog>,
    name: &str,
    surface: &str,
) -> Result<&'a ResolvedVariant, ToolError> {
    let catalog = catalog.ok_or_else(|| ToolError::ExecutionFailed {
        reason: format!(
            "{surface}: variant '{name}' requested but no variant catalog is \
             available on this agent's context; the assembly path did not \
             install one — runtimes assembled without the runtime base \
             (load_runtime_base) get no catalog, built-ins included, and \
             children inherit the parent's catalog at construction",
        ),
    })?;
    catalog.get(name).ok_or_else(|| {
        let available: Vec<&str> = catalog.names().collect();
        ToolError::ExecutionFailed {
            reason: format!(
                "{surface}: unknown variant '{name}' — available variants: {}",
                available.join(", "),
            ),
        }
    })
}

/// Resolve the child's model: explicit spawn-time `model`, else the
/// variant's model, else — when the variant does not require one — the
/// parent's own model via `parent_model`.
///
/// # Errors
///
/// - A `model_required` variant with no model anywhere → typed error
///   naming `variants.<name>.model` (the reviewer ruling: no default, no
///   parent inherit, no invented pin).
/// - Propagates `parent_model`'s error when inheritance is needed but no
///   runtime ground truth exists.
pub(crate) fn resolve_child_model(
    explicit: Option<String>,
    variant: Option<&ResolvedVariant>,
    surface: &str,
    parent_model: impl FnOnce() -> Result<String, ToolError>,
) -> Result<String, ToolError> {
    if let Some(model) = explicit {
        return Ok(model);
    }
    if let Some(variant) = variant {
        if let Some(model) = variant.model.clone() {
            return Ok(model);
        }
        if variant.model_required {
            return Err(ToolError::ExecutionFailed {
                reason: format!(
                    "{surface}: variant '{name}' requires a model: set \
                     variants.{name}.model or pass model explicitly",
                    name = variant.name,
                ),
            });
        }
    }
    parent_model()
}

/// Resolve the calling agent's own model from runtime ground truth: the
/// live [`AgentModel`] extension FIRST — published at assembly and
/// refreshed at every step start by the agent loop with the model that
/// step's provider request actually uses — then the caller's
/// [`AgentRegistry`] entry. The extension wins because the registry row
/// is stamped once at build/launch and goes stale across runtime model
/// switches (the CLI's `/model` re-reads `SlashState.model` before each
/// step; only the per-step refresh tracks it). Settings are never
/// re-read and nothing is invented: with neither source the caller gets
/// a typed error telling it to pass `model` explicitly.
///
/// # Errors
///
/// Typed [`ToolError::ExecutionFailed`] when no runtime source exists.
pub(crate) fn resolve_parent_model(
    registry: &RwLock<AgentRegistry>,
    agent_id: Uuid,
    ctx: Option<&ToolContext>,
    surface: &str,
) -> Result<String, ToolError> {
    if let Some(live) = ctx.and_then(ToolContext::get_extension::<AgentModel>) {
        return Ok(live.model.clone());
    }
    if let Some(entry) = registry.read().get(agent_id) {
        return Ok(entry.model);
    }
    Err(ToolError::ExecutionFailed {
        reason: format!(
            "{surface}: no model was given and the calling agent's own model \
             could not be resolved from runtime ground truth (no AgentModel \
             extension on its context and no agent-registry entry for this \
             agent) — pass model explicitly",
        ),
    })
}

/// Inputs to [`resolve_child_reasoning_effort`] — the effort candidates
/// in precedence order plus the labels the validation errors/warnings
/// name.
pub(crate) struct ChildEffortInputs<'a> {
    /// The variant's parsed effort, if the spawn resolved a variant that
    /// carries one.
    pub(crate) variant_effort: Option<crate::provider::request::ReasoningEffort>,
    /// The variant's name (labels the `variants.<name>.reasoning_effort`
    /// setting in rejections).
    pub(crate) variant_name: Option<&'a str>,
    /// A profile-set effort already threaded onto the child's loop
    /// context by `from_profile`, if any.
    pub(crate) profile_effort: Option<crate::provider::request::ReasoningEffort>,
    /// The profile's name (labels the rejection).
    pub(crate) profile_name: Option<&'a str>,
    /// The parent's ACTIVE effort from its live per-step
    /// [`AgentModel`] stamp.
    pub(crate) parent_live_effort: Option<crate::provider::request::ReasoningEffort>,
    /// The child's resolved model — what the catalog support check runs
    /// against.
    pub(crate) child_model: &'a str,
    /// The child's role/variant label (names the child in the
    /// inherited-degrade warning).
    pub(crate) child_role: &'a str,
    /// The calling surface (`"spawn_agent"`) for message attribution.
    pub(crate) surface: &'a str,
}

/// Resolve and validate the child's reasoning effort (spec §3.6 + owner
/// rulings 2026-07-07): the variant's effort wins, else a profile-set
/// effort, else the child INHERITS the parent's ACTIVE effort from the
/// live per-step [`AgentModel`] stamp (never a re-read of settings, never
/// an invented literal; a parent running with no effort passes `None`
/// through unchanged). The resolved value is validated against the model
/// catalog for the CHILD's resolved model via
/// [`arm_child_reasoning_effort`](crate::agent::arming::arm_child_reasoning_effort)
/// (root `/effort` parity): an explicitly configured unsupported pairing
/// is a typed error naming the setting; an unsupported inherited effort
/// degrades to `None` with a warning.
///
/// # Errors
///
/// [`ToolError::ExecutionFailed`] for the explicit-unsupported case only.
pub(crate) fn resolve_child_reasoning_effort(
    inputs: &ChildEffortInputs<'_>,
) -> Result<Option<crate::provider::request::ReasoningEffort>, ToolError> {
    use crate::agent::arming::{ChildEffortSource, arm_child_reasoning_effort};

    let (effort, explicit_setting) = if inputs.variant_effort.is_some() {
        (
            inputs.variant_effort,
            Some(format!(
                "variants.{}.reasoning_effort",
                inputs.variant_name.unwrap_or("<variant>"),
            )),
        )
    } else if inputs.profile_effort.is_some() {
        (
            inputs.profile_effort,
            Some(format!(
                "profile '{}' reasoning_effort",
                inputs.profile_name.unwrap_or("<profile>"),
            )),
        )
    } else {
        (inputs.parent_live_effort, None)
    };
    let source = match explicit_setting.as_deref() {
        Some(setting) => ChildEffortSource::Explicit(setting),
        None => ChildEffortSource::Inherited {
            child: inputs.child_role,
        },
    };
    arm_child_reasoning_effort(effort, &source, inputs.child_model).map_err(|e| {
        ToolError::ExecutionFailed {
            reason: format!("{}: {e}", inputs.surface),
        }
    })
}

/// The fully resolved spawn identity: role, model, and the variant's
/// contribution to the child's prompt, tool base, and reasoning effort.
#[derive(Debug)]
pub(crate) struct SpawnResolution {
    /// Resolved role label (explicit `role`, else the variant name).
    pub(crate) role: String,
    /// Resolved model id (explicit, variant, or the parent's own model).
    pub(crate) model: String,
    /// The variant name when one was used — disclosed on the child's
    /// [`SubagentDescriptor`](crate::provider::agent_event::SubagentDescriptor)
    /// `profile` field and carried into the child's name stem.
    pub(crate) variant_name: Option<String>,
    /// The variant's loaded prompt text, if any.
    pub(crate) variant_prompt: Option<String>,
    /// The variant's tool allowlist, if any.
    pub(crate) variant_tools: Option<Vec<String>>,
    /// The variant's MCP server subset, if any.
    pub(crate) variant_mcp_servers: Option<Vec<String>>,
    /// The variant's parsed reasoning effort, if any.
    pub(crate) reasoning_effort: Option<crate::provider::request::ReasoningEffort>,
}

/// Arguments to [`resolve_spawn`] — the spawn tool's raw identity inputs.
pub(crate) struct SpawnIdentityArgs {
    /// The optional `variant` argument.
    pub(crate) variant: Option<String>,
    /// The optional `profile` argument (mutually exclusive with variant).
    pub(crate) profile: Option<String>,
    /// The optional `role` argument.
    pub(crate) role: Option<String>,
    /// The optional `model` argument.
    pub(crate) model: Option<String>,
}

/// Run the spawn tool's resolution sequence (spec §3 steps 1–4):
/// variant/profile mutual exclusion, catalog lookup, role resolution
/// (`role` else variant name else typed error), and model resolution via
/// [`resolve_child_model`].
///
/// # Errors
///
/// Typed [`ToolError`]s per step; see the item docs above.
pub(crate) fn resolve_spawn(
    args: SpawnIdentityArgs,
    catalog: Option<&VariantCatalog>,
    surface: &str,
    parent_model: impl FnOnce() -> Result<String, ToolError>,
) -> Result<SpawnResolution, ToolError> {
    if args.variant.is_some() && args.profile.is_some() {
        return Err(ToolError::ExecutionFailed {
            reason: format!(
                "{surface}: variant and profile are mutually exclusive — pass one \
                 or the other",
            ),
        });
    }
    let variant = match args.variant.as_deref() {
        Some(name) => Some(lookup_variant(catalog, name, surface)?),
        None => None,
    };
    let role = match (args.role, variant) {
        (Some(role), _) => role,
        (None, Some(variant)) => variant.name.clone(),
        (None, None) => {
            return Err(ToolError::ExecutionFailed {
                reason: format!("{surface}: 'role' or 'variant' is required"),
            });
        }
    };
    let model = resolve_child_model(args.model, variant, surface, parent_model)?;
    Ok(SpawnResolution {
        role,
        model,
        variant_name: variant.map(|v| v.name.clone()),
        variant_prompt: variant.and_then(|v| v.prompt.clone()),
        variant_tools: variant.and_then(|v| v.tools.clone()),
        variant_mcp_servers: variant.and_then(|v| v.mcp_servers.clone()),
        reasoning_effort: variant.and_then(|v| v.reasoning_effort),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    use crate::config::types::VariantSettings;
    use crate::provider::request::ReasoningEffort;

    fn catalog() -> VariantCatalog {
        VariantCatalog::build(None, &std::env::temp_dir()).expect("built-ins build")
    }

    fn identity(
        variant: Option<&str>,
        profile: Option<&str>,
        role: Option<&str>,
        model: Option<&str>,
    ) -> SpawnIdentityArgs {
        SpawnIdentityArgs {
            variant: variant.map(str::to_owned),
            profile: profile.map(str::to_owned),
            role: role.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }

    fn no_parent_model() -> Result<String, ToolError> {
        panic!("parent model must not be consulted on this path");
    }

    /// §3.1: variant and profile together are refused before any lookup.
    #[test]
    fn variant_and_profile_are_mutually_exclusive() {
        let catalog = catalog();
        let err = resolve_spawn(
            identity(Some("explorer"), Some("developer"), None, None),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect_err("mutual exclusion");
        assert!(
            err.to_string().contains("mutually exclusive"),
            "names the conflict: {err}",
        );
    }

    /// §3.2: an unknown variant lists the available names, sorted.
    #[test]
    fn unknown_variant_lists_catalog_names_sorted() {
        let catalog = catalog();
        let err = resolve_spawn(
            identity(Some("ghost"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect_err("unknown variant");
        let message = err.to_string();
        assert!(message.contains("unknown variant 'ghost'"), "{message}");
        assert!(
            message.contains("explorer, implementer, reviewer"),
            "sorted catalog listing: {message}",
        );
    }

    /// A variant requested with no catalog installed is a typed wiring
    /// error, never a silent no-variant spawn.
    #[test]
    fn variant_without_catalog_is_a_wiring_error() {
        let err = resolve_spawn(
            identity(Some("explorer"), None, None, None),
            None,
            "spawn_agent",
            no_parent_model,
        )
        .expect_err("missing catalog");
        let message = err.to_string();
        assert!(
            message.contains("no variant catalog is available"),
            "{message}",
        );
        assert!(
            message.contains("the assembly path did not install one"),
            "names the install boundary, not a blanket assembly blame: {message}",
        );
    }

    /// §3.3: role falls back to the variant name; with neither the spawn
    /// is refused.
    #[test]
    fn role_falls_back_to_variant_name_and_is_otherwise_required() {
        let catalog = catalog();
        let resolved = resolve_spawn(
            identity(Some("explorer"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            || Ok("parent-model".to_owned()),
        )
        .expect("explorer resolves");
        assert_eq!(resolved.role, "explorer");
        assert_eq!(resolved.variant_name.as_deref(), Some("explorer"));

        let err = resolve_spawn(
            identity(None, None, None, Some("gpt-5.5")),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect_err("no role, no variant");
        assert!(
            err.to_string().contains("'role' or 'variant' is required"),
            "{err}",
        );
    }

    /// §3.4: explicit model wins; a variant model fills next; explorer
    /// (no model, not required) inherits the parent's.
    #[test]
    fn model_resolution_order_explicit_variant_parent() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            VariantSettings {
                prompt: Some("Scout.".to_owned()),
                model: Some("scout-model".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog =
            VariantCatalog::build(Some(&configured), &std::env::temp_dir()).expect("build");

        let explicit = resolve_spawn(
            identity(Some("scout"), None, None, Some("explicit-model")),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect("resolves");
        assert_eq!(explicit.model, "explicit-model");

        let from_variant = resolve_spawn(
            identity(Some("scout"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect("resolves");
        assert_eq!(from_variant.model, "scout-model");

        let inherited = resolve_spawn(
            identity(Some("explorer"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            || Ok("parent-model".to_owned()),
        )
        .expect("resolves");
        assert_eq!(inherited.model, "parent-model");
    }

    /// The reviewer ruling: `model_required` with no model anywhere is a
    /// typed error naming `variants.reviewer.model` — the parent's model
    /// is never consulted.
    #[test]
    fn reviewer_without_model_names_the_config_key() {
        let catalog = catalog();
        let err = resolve_spawn(
            identity(Some("reviewer"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            no_parent_model,
        )
        .expect_err("reviewer without a model");
        assert!(
            err.to_string().contains("variants.reviewer.model"),
            "names the missing key: {err}",
        );
    }

    /// The variant's prompt, tools, and reasoning effort ride the
    /// resolution for the spawn site to apply.
    #[test]
    fn resolution_carries_variant_prompt_tools_and_effort() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            VariantSettings {
                prompt: Some("Scout the area.".to_owned()),
                tools: Some(vec!["read".to_owned()]),
                mcp_servers: Some(vec!["docs".to_owned()]),
                reasoning_effort: Some("low".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog =
            VariantCatalog::build(Some(&configured), &std::env::temp_dir()).expect("build");
        let resolved = resolve_spawn(
            identity(Some("scout"), None, None, None),
            Some(&catalog),
            "spawn_agent",
            || Ok("parent-model".to_owned()),
        )
        .expect("resolves");
        assert_eq!(resolved.variant_prompt.as_deref(), Some("Scout the area."));
        assert_eq!(resolved.variant_tools, Some(vec!["read".to_owned()]));
        assert_eq!(resolved.variant_mcp_servers, Some(vec!["docs".to_owned()]));
        assert_eq!(resolved.reasoning_effort, Some(ReasoningEffort::Low));
    }

    /// Parent-model ground truth: the live [`AgentModel`] extension
    /// wins over the registry entry (the row is stamped at build and
    /// goes stale across runtime `/model` switches), the registry backs
    /// it up when no extension exists, and with neither source the
    /// caller gets a typed error.
    #[test]
    fn parent_model_prefers_live_extension_over_registry_then_errors() {
        let registry = AgentRegistry::shared();
        let policy = ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 4,
            },
            inbound_capacity: 8,
            loop_config: None,
        };
        let guard = AgentRegistry::reserve(
            &registry,
            "/registered".to_owned(),
            "worker".to_owned(),
            "registered-model".to_owned(),
            None,
            policy,
            None,
        )
        .expect("reserve");
        let registered = guard.id();
        guard.confirm().expect("confirm");

        // Both sources present: the live extension wins — the registry
        // row is the stale build-time stamp after a runtime model switch.
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(AgentModel {
            model: "live-step-model".to_owned(),
            reasoning_effort: None,
        }));
        assert_eq!(
            resolve_parent_model(&registry, registered, Some(&ctx), "spawn_agent")
                .expect("extension wins"),
            "live-step-model",
        );

        // Registry only (no context / no extension): the entry backs it up.
        assert_eq!(
            resolve_parent_model(&registry, registered, None, "spawn_agent").expect("registry"),
            "registered-model",
        );

        // Extension only (unregistered caller): still resolves.
        assert_eq!(
            resolve_parent_model(&registry, Uuid::new_v4(), Some(&ctx), "spawn_agent")
                .expect("extension"),
            "live-step-model",
        );

        let err = resolve_parent_model(&registry, Uuid::new_v4(), None, "spawn_agent")
            .expect_err("no source");
        assert!(
            err.to_string().contains("pass model explicitly"),
            "actionable error: {err}",
        );
    }
}
