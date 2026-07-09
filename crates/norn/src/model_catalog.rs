//! Generated model metadata loaded from `assets/models.json`.

/// Default provider identifier from the model catalog.
pub const DEFAULT_PROVIDER: &str = generated::DEFAULT_PROVIDER;
/// Default backend identifier from the model catalog.
pub const DEFAULT_BACKEND: &str = generated::DEFAULT_BACKEND;
/// Default model identifier from the model catalog.
pub const DEFAULT_MODEL: &str = generated::DEFAULT_MODEL;

/// The complete generated model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalog {
    /// Catalog schema version.
    pub schema_version: u64,
    /// Default provider/backend/model selection.
    pub default: ModelSelection,
    /// Providers available in the catalog.
    pub providers: &'static [ProviderEntry],
}

/// Provider/backend/model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSelection {
    /// Provider identifier.
    pub provider: &'static str,
    /// Backend identifier.
    pub backend: &'static str,
    /// Model identifier.
    pub model: &'static str,
}

/// Provider entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderEntry {
    /// Provider identifier.
    pub id: &'static str,
    /// Human-readable provider name.
    pub display_name: &'static str,
    /// Backends available for this provider.
    pub backends: &'static [BackendEntry],
}

/// Provider backend entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendEntry {
    /// Backend identifier.
    pub id: &'static str,
    /// Human-readable backend name.
    pub display_name: &'static str,
    /// Authentication mode used by this backend.
    pub auth: &'static str,
    /// API surface used by this backend.
    pub api_surface: &'static str,
    /// Models available through this backend.
    pub models: &'static [ModelEntry],
}

/// Backend-specific model metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelEntry {
    /// Model identifier.
    pub id: &'static str,
    /// Short catalog alias resolving to [`Self::id`].
    pub alias: &'static str,
    /// Human-readable model name.
    pub display_name: &'static str,
    /// Human-readable model description.
    pub description: &'static str,
    /// Default context window for this backend.
    pub context_window: u64,
    /// Maximum context window available through this backend.
    pub max_context_window: u64,
    /// Default reasoning effort.
    pub default_reasoning_effort: &'static str,
    /// Supported reasoning effort identifiers.
    pub supported_reasoning_efforts: &'static [&'static str],
    /// Default reasoning summary mode.
    pub default_reasoning_summary: &'static str,
    /// Whether reasoning summaries are available.
    pub supports_reasoning_summaries: bool,
    /// Service tiers available for this backend/model pair.
    pub service_tiers: &'static [ServiceTierEntry],
    /// Web-search tool surface.
    pub web_search_tool_type: &'static str,
    /// Supported input modalities.
    pub input_modalities: &'static [&'static str],
    /// Whether original-detail image inputs are supported.
    pub supports_image_detail_original: bool,
    /// Whether hosted search is supported.
    pub supports_search_tool: bool,
    /// Whether parallel tool calls are supported.
    pub supports_parallel_tool_calls: bool,
    /// Apply-patch tool surface.
    pub apply_patch_tool_type: &'static str,
}

/// Provider-specific value for a user-facing service tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceTierEntry {
    /// Norn-facing tier identifier, e.g. `fast`.
    pub id: &'static str,
    /// Provider wire value, e.g. `OpenAI` `priority`.
    pub provider_value: &'static str,
    /// Human-readable tier name.
    pub display_name: &'static str,
    /// Human-readable tier description.
    pub description: &'static str,
}

/// Return the generated catalog.
#[must_use]
pub const fn catalog() -> &'static ModelCatalog {
    &generated::CATALOG
}

/// Return the default model selection.
#[must_use]
pub const fn default_selection() -> ModelSelection {
    catalog().default
}

/// Find a provider by identifier.
#[must_use]
pub fn find_provider(provider: &str) -> Option<&'static ProviderEntry> {
    catalog()
        .providers
        .iter()
        .find(|entry| entry.id == provider)
}

/// Find a backend by provider and backend identifier.
#[must_use]
pub fn find_backend(provider: &str, backend: &str) -> Option<&'static BackendEntry> {
    find_provider(provider)?
        .backends
        .iter()
        .find(|entry| entry.id == backend)
}

/// Find a backend-specific model entry.
#[must_use]
pub fn find_model(provider: &str, backend: &str, model: &str) -> Option<&'static ModelEntry> {
    find_backend(provider, backend)?
        .models
        .iter()
        .find(|entry| entry.id == model)
}

/// Resolve a canonical model identifier or catalog alias.
///
/// Canonical identifiers take precedence and resolve to themselves. Alias
/// uniqueness is enforced by the catalog generator, so every other successful
/// lookup resolves to exactly one canonical model identifier even when that
/// model is available through multiple backends.
#[must_use]
pub fn resolve_model_alias(model: &str) -> Option<&'static str> {
    let models = || {
        catalog()
            .providers
            .iter()
            .flat_map(|provider| provider.backends)
            .flat_map(|backend| backend.models)
    };

    models()
        .find(|entry| entry.id == model)
        .or_else(|| models().find(|entry| entry.alias == model))
        .map(|entry| entry.id)
}

/// Return the smallest catalogued context window for a model id.
///
/// The same provider model id can appear under several backends with different
/// limits. Budgeting code should use the smallest known value unless it has a
/// more specific provider/backend selection.
#[must_use]
pub fn smallest_context_window_for_model(model: &str) -> Option<u64> {
    catalog()
        .providers
        .iter()
        .flat_map(|provider| provider.backends)
        .flat_map(|backend| backend.models)
        .filter(|entry| entry.id == model)
        .map(|entry| entry.context_window)
        .min()
}

/// Return the largest catalogued `max_context_window` for a model id.
///
/// The counterpart to [`smallest_context_window_for_model`] for
/// *validation*: an explicitly configured window is legitimate as long as
/// at least one backend serving this model id can honour it (e.g.
/// gpt-5.4's standard window is 272k but its maximum is 1M, so an
/// explicit 1M passes), and rejected only when it exceeds every backend's
/// ceiling — the shape of the 2026-07-05 incident, where a global 272k
/// override mis-armed a 128k model.
#[must_use]
pub fn largest_max_context_window_for_model(model: &str) -> Option<u64> {
    catalog()
        .providers
        .iter()
        .flat_map(|provider| provider.backends)
        .flat_map(|backend| backend.models)
        .filter(|entry| entry.id == model)
        .map(|entry| entry.max_context_window)
        .max()
}

/// Find a service tier supported by the selected backend/model pair.
#[must_use]
pub fn find_service_tier(
    provider: &str,
    backend: &str,
    model: &str,
    tier: &str,
) -> Option<&'static ServiceTierEntry> {
    find_model(provider, backend, model)?
        .service_tiers
        .iter()
        .find(|entry| entry.id == tier)
}

/// Return the provider wire value for a service tier.
#[must_use]
pub fn service_tier_provider_value(
    provider: &str,
    backend: &str,
    model: &str,
    tier: &str,
) -> Option<&'static str> {
    Some(find_service_tier(provider, backend, model, tier)?.provider_value)
}

mod generated {
    use super::{
        BackendEntry, ModelCatalog, ModelEntry, ModelSelection, ProviderEntry, ServiceTierEntry,
    };

    include!(concat!(env!("OUT_DIR"), "/model_catalog_generated.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selection_points_to_existing_model() {
        let default = default_selection();
        assert_eq!(default.model, "gpt-5.6-sol");
        assert!(find_model(default.provider, default.backend, default.model).is_some());
    }

    #[test]
    fn new_codex_models_have_expected_metadata() {
        for (model, default_effort) in [
            ("gpt-5.6-sol", "low"),
            ("gpt-5.6-terra", "medium"),
            ("gpt-5.6-luna", "medium"),
        ] {
            let entry = find_model("openai", "codex_subscription", model);
            assert!(entry.is_some(), "{model} must be in the catalog");
            if let Some(entry) = entry {
                assert_eq!(resolve_model_alias(entry.alias), Some(model), "{model}");
                assert_eq!(entry.context_window, 372_000, "{model}");
                assert_eq!(entry.max_context_window, 372_000, "{model}");
                assert_eq!(entry.default_reasoning_effort, default_effort, "{model}");
                assert!(
                    entry.supported_reasoning_efforts.contains(&"max"),
                    "{model}"
                );
                assert!(
                    !entry.supported_reasoning_efforts.contains(&"ultra"),
                    "{model} must not expose ultra as a distinct effort",
                );
                assert_eq!(
                    service_tier_provider_value("openai", "codex_subscription", model, "fast",),
                    Some("priority"),
                    "{model}",
                );
            }
        }
    }

    #[test]
    fn model_aliases_resolve_to_canonical_ids() {
        for (alias, canonical_id) in [
            ("sol", "gpt-5.6-sol"),
            ("terra", "gpt-5.6-terra"),
            ("luna", "gpt-5.6-luna"),
            ("codex-spark", "gpt-5.3-codex-spark"),
        ] {
            assert_eq!(resolve_model_alias(alias), Some(canonical_id), "{alias}");
            assert_eq!(resolve_model_alias(canonical_id), Some(canonical_id));
        }
        assert_eq!(resolve_model_alias("not-in-catalog"), None);
    }

    #[test]
    fn every_emitted_alias_resolves_to_its_entry_id() {
        for provider in catalog().providers {
            for backend in provider.backends {
                for entry in backend.models {
                    assert_eq!(resolve_model_alias(entry.alias), Some(entry.id));
                }
            }
        }
    }

    #[test]
    fn fast_maps_to_openai_priority_for_gpt_55_codex_subscription() {
        assert_eq!(
            service_tier_provider_value("openai", "codex_subscription", "gpt-5.5", "fast"),
            Some("priority"),
        );
    }

    #[test]
    fn smallest_context_window_returns_catalogued_model_limit() {
        assert!(smallest_context_window_for_model(default_selection().model).is_some());
        assert_eq!(smallest_context_window_for_model("not-in-catalog"), None);
    }
}
