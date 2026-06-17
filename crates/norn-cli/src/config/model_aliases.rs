//! User model-alias resolution.

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
}

/// Resolve `model` through `settings.model_aliases` when it names a user alias.
///
/// Exact built-in catalog model IDs win over aliases so a user cannot
/// accidentally shadow a real model in the bundled catalog. Unknown model IDs
/// pass through unchanged, which is required for local and hosted custom
/// providers whose model IDs are not in the bundled catalog.
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
    if is_catalog_model(model) {
        return Ok(ResolvedModelSelection {
            model: model.to_owned(),
            provider_profile: None,
            api_shape: None,
        });
    }
    let Some(aliases) = settings.model_aliases.as_ref() else {
        return Ok(ResolvedModelSelection {
            model: model.to_owned(),
            provider_profile: None,
            api_shape: None,
        });
    };
    let Some(target) = aliases.get(model) else {
        return Ok(ResolvedModelSelection {
            model: model.to_owned(),
            provider_profile: None,
            api_shape: None,
        });
    };
    Ok(ResolvedModelSelection {
        model: target.model().to_owned(),
        provider_profile: target.provider_profile().map(str::to_owned),
        api_shape: target.api_shape().map(str::to_owned),
    })
}

fn is_catalog_model(model: &str) -> bool {
    norn::model_catalog::catalog()
        .providers
        .iter()
        .flat_map(|provider| provider.backends)
        .flat_map(|backend| backend.models)
        .any(|entry| entry.id == model)
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
    }
}
