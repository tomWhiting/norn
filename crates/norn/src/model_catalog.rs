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
    /// Provider wire value, e.g. OpenAI `priority`.
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
        assert!(find_model(default.provider, default.backend, default.model).is_some());
    }

    #[test]
    fn fast_maps_to_openai_priority_for_gpt_55_codex_subscription() {
        assert_eq!(
            service_tier_provider_value("openai", "codex_subscription", "gpt-5.5", "fast"),
            Some("priority"),
        );
    }
}
