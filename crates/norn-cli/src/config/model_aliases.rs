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

/// Resolve an operator token into a full model/backend selection.
/// Surrounding whitespace is trimmed; alias targets retain their exact identity
/// for validation by the selected backend's model-selection preflight.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when alias backend fields are invalid.
pub fn resolve_model_selection(
    model: &str,
    settings: &NornSettings,
) -> Result<ResolvedModelSelection, BuildError> {
    let aliases = settings.model_aliases.clone().unwrap_or_default();
    let resolved = norn::model_selection::resolve_alias(model.trim(), &aliases);
    let catalog = if resolved.provider_profile.is_none() && resolved.api_shape.is_none() {
        norn::model_catalog::resolve_catalog_model(&resolved.model)
    } else {
        None
    };
    Ok(ResolvedModelSelection {
        catalog,
        model: resolved.model,
        provider_profile: resolved.provider_profile,
        api_shape: resolved.api_shape,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norn::config::{ModelAliasSelection, ModelAliasSettings};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn unknown_model_passes_through() -> TestResult {
        let settings = NornSettings::default();
        assert_eq!(
            resolve_model_alias("google/gemma-4-e4b", &settings)?,
            "google/gemma-4-e4b",
        );
        Ok(())
    }

    #[test]
    fn alias_resolves_to_model() -> TestResult {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "55".to_owned(),
            ModelAliasSettings::Model("gpt-5.5".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(resolve_model_alias("55", &settings)?, "gpt-5.5");
        Ok(())
    }

    #[test]
    fn bundled_catalog_alias_resolves_to_model() -> TestResult {
        let selection = resolve_model_selection("sol", &NornSettings::default())?;
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
        Ok(())
    }

    #[test]
    fn user_alias_wins_over_bundled_catalog_alias() -> TestResult {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "sol".to_owned(),
            ModelAliasSettings::Model("custom-sol-model".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(resolve_model_alias("sol", &settings)?, "custom-sol-model",);
        Ok(())
    }

    #[test]
    fn catalog_model_wins_over_same_named_alias() -> TestResult {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "gpt-5.5".to_owned(),
            ModelAliasSettings::Model("other-model".to_owned()),
        );
        let settings = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        assert_eq!(resolve_model_alias("gpt-5.5", &settings)?, "gpt-5.5",);
        Ok(())
    }

    #[test]
    fn full_backend_alias_returns_backend_selection() -> TestResult {
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
        let selection = resolve_model_selection("local", &settings)?;
        assert_eq!(selection.model, "google/gemma-4-e4b");
        assert_eq!(selection.provider_profile.as_deref(), Some("lmstudio"));
        assert_eq!(
            selection.api_shape.as_deref(),
            Some("openai_chat_completions"),
        );
        assert!(selection.catalog.is_none());
        Ok(())
    }

    #[test]
    fn claude_catalog_model_preserves_subscription_route() -> TestResult {
        let selection = resolve_model_selection("claude-opus-5", &NornSettings::default())?;
        assert_eq!(selection.model, "claude-opus-5");
        assert_eq!(
            selection.catalog,
            Some(norn::model_catalog::CatalogModelSelection {
                provider: "anthropic",
                backend: "claude_code_subscription",
                model: "claude-opus-5",
            }),
        );
        Ok(())
    }

    #[test]
    fn explicitly_routed_user_alias_does_not_inherit_catalog_route() -> TestResult {
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
        let selection = resolve_model_selection("private-claude", &settings)?;
        assert_eq!(selection.model, "claude-opus-5");
        assert_eq!(selection.provider_profile.as_deref(), Some("private"));
        assert!(selection.catalog.is_none());
        Ok(())
    }
}
