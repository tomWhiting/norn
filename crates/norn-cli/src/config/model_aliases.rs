//! User and bundled model-alias resolution.

use norn::config::NornSettings;

use crate::cli::BuildError;

/// Resolved model plus optional backend selection from a user alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModelSelection {
    /// Provider model id.
    pub model: String,
    /// Optional provider profile selected by the alias.
    pub provider_profile: Option<String>,
    /// Optional API shape selected by the alias.
    pub api_shape: Option<String>,
    /// Provider/backend provenance when selection came from the bundled
    /// catalog rather than an explicitly routed user alias.
    pub catalog: Option<norn::model_catalog::CatalogModelSelection>,
}

/// Resolve `model` through the configured and bundled model aliases.
///
/// Resolution precedence is: exact built-in catalog model ID, user-defined
/// `settings.model_aliases`, bundled catalog alias, then unchanged passthrough.
/// Unknown model IDs pass through unchanged, which is required for local and
/// hosted custom providers whose model IDs are not in the bundled catalog.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when alias backend fields are invalid.
pub fn resolve_model_alias(model: &str, settings: &NornSettings) -> Result<String, BuildError> {
    Ok(resolve_model_selection(model, settings)?.model)
}

/// Resolve `model` into a full model/backend selection.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when alias backend fields are invalid.
pub fn resolve_model_selection(
    model: &str,
    settings: &NornSettings,
) -> Result<ResolvedModelSelection, BuildError> {
    if let Some(catalog) = norn::model_catalog::resolve_catalog_model(model)
        && catalog.model == model
    {
        return Ok(catalog_selection(catalog));
    }

    if let Some(target) = settings
        .model_aliases
        .as_ref()
        .and_then(|aliases| aliases.get(model))
    {
        let provider_profile = target.provider_profile().map(str::to_owned);
        let api_shape = target.api_shape().map(str::to_owned);
        if provider_profile.is_some() || api_shape.is_some() {
            return Ok(ResolvedModelSelection {
                model: target.model().to_owned(),
                provider_profile,
                api_shape,
                catalog: None,
            });
        }
        return Ok(norn::model_catalog::resolve_catalog_model(target.model())
            .map_or_else(|| model_only_selection(target.model()), catalog_selection));
    }

    Ok(norn::model_catalog::resolve_catalog_model(model)
        .map_or_else(|| model_only_selection(model), catalog_selection))
}

fn model_only_selection(model: &str) -> ResolvedModelSelection {
    ResolvedModelSelection {
        model: model.to_owned(),
        provider_profile: None,
        api_shape: None,
        catalog: None,
    }
}

fn catalog_selection(
    catalog: norn::model_catalog::CatalogModelSelection,
) -> ResolvedModelSelection {
    ResolvedModelSelection {
        model: catalog.model.to_owned(),
        provider_profile: None,
        api_shape: None,
        catalog: Some(catalog),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use norn::config::{ModelAliasSelection, ModelAliasSettings};

    use super::*;

    #[test]
    fn unknown_model_passes_through() {
        let settings = NornSettings::default();
        assert_eq!(
            resolve_model_alias("google/gemma-4-e4b", &settings).unwrap(),
            "google/gemma-4-e4b",
        );
    }

    #[test]
    fn alias_resolves_to_model() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "55".to_owned(),
            ModelAliasSettings::Model("gpt-5.5".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(resolve_model_alias("55", &settings).unwrap(), "gpt-5.5");
    }

    #[test]
    fn bundled_catalog_alias_resolves_to_model() {
        let selection = resolve_model_selection("sol", &NornSettings::default()).unwrap();
        assert_eq!(selection.model, "gpt-5.6-sol");
        assert!(selection.provider_profile.is_none());
        assert!(selection.api_shape.is_none());
        assert_eq!(
            selection.catalog,
            Some(norn::model_catalog::CatalogModelSelection {
                provider: "openai",
                backend: "codex_subscription",
                model: "gpt-5.6-sol",
            }),
        );
    }

    #[test]
    fn user_alias_wins_over_bundled_catalog_alias() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "sol".to_owned(),
            ModelAliasSettings::Model("custom-sol-model".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(
            resolve_model_alias("sol", &settings).unwrap(),
            "custom-sol-model",
        );
    }

    #[test]
    fn catalog_model_wins_over_same_named_alias() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "gpt-5.5".to_owned(),
            ModelAliasSettings::Model("other-model".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(
            resolve_model_alias("gpt-5.5", &settings).unwrap(),
            "gpt-5.5",
        );
    }

    #[test]
    fn full_backend_alias_returns_backend_selection() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "local".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                provider_profile: Some("lmstudio".to_owned()),
                api_shape: Some("openai_chat_completions".to_owned()),
                model: "google/gemma-4-e4b".to_owned(),
            }),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        let selection = resolve_model_selection("local", &settings).unwrap();
        assert_eq!(selection.model, "google/gemma-4-e4b");
        assert_eq!(selection.provider_profile.as_deref(), Some("lmstudio"));
        assert_eq!(
            selection.api_shape.as_deref(),
            Some("openai_chat_completions"),
        );
        assert!(selection.catalog.is_none());
    }

    #[test]
    fn claude_catalog_model_preserves_subscription_route() {
        let selection = resolve_model_selection("claude-opus-5", &NornSettings::default()).unwrap();
        assert_eq!(selection.model, "claude-opus-5");
        assert_eq!(
            selection.catalog,
            Some(norn::model_catalog::CatalogModelSelection {
                provider: "anthropic",
                backend: "claude_code_subscription",
                model: "claude-opus-5",
            }),
        );
    }

    #[test]
    fn explicitly_routed_user_alias_does_not_inherit_catalog_route() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "private-claude".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                provider_profile: Some("private".to_owned()),
                api_shape: Some("openai_chat_completions".to_owned()),
                model: "claude-opus-5".to_owned(),
            }),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        let selection = resolve_model_selection("private-claude", &settings).unwrap();
        assert_eq!(selection.model, "claude-opus-5");
        assert_eq!(selection.provider_profile.as_deref(), Some("private"));
        assert!(selection.catalog.is_none());
    }
}
