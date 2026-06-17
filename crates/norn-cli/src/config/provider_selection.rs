//! Provider API-shape/profile selection.

use norn::config::{NornSettings, ProviderProfileSettings};
use norn::provider::{ApiShape, ProviderProfileId};

use crate::cli::{ApiShapeKind, BuildError, Cli, ProviderKind};

use super::model_aliases::ResolvedModelSelection;

/// Resolved provider selection for runtime construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    /// Existing provider implementation to instantiate.
    pub kind: ProviderKind,
    /// Optional named provider profile applied to provider overrides.
    pub profile_name: Option<String>,
}

/// Resolve `--provider`, `--api-shape`, and `--provider-profile`.
///
/// `--provider` is retained as a compatibility alias. The new shape/profile
/// path maps onto the provider implementations that exist today and fails
/// loudly for reserved shapes whose adapters are not implemented yet.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] for an unknown profile, invalid profile
/// id, missing profile API shape, or reserved/unimplemented API shape.
pub fn resolve_provider_selection(
    cli: &Cli,
    settings: &NornSettings,
    model_selection: &ResolvedModelSelection,
) -> Result<ProviderSelection, BuildError> {
    if let Some(kind) = cli.provider {
        if model_selection.provider_profile.is_some() || model_selection.api_shape.is_some() {
            return Err(BuildError::Argument(
                "model alias selects provider_profile/api_shape, but --provider was also supplied; \
                 choose one provider selection path"
                    .to_owned(),
            ));
        }
        return Ok(ProviderSelection {
            kind,
            profile_name: None,
        });
    }

    let profile_name = selected_provider_profile_name(cli, model_selection)?;
    let profile = selected_provider_profile(profile_name.as_deref(), settings)?;
    let shape = resolve_api_shape(cli, model_selection, profile)?;
    Ok(ProviderSelection {
        kind: provider_kind_for_shape(shape)?,
        profile_name,
    })
}

fn selected_provider_profile_name(
    cli: &Cli,
    model_selection: &ResolvedModelSelection,
) -> Result<Option<String>, BuildError> {
    match (
        cli.provider_profile.as_deref(),
        model_selection.provider_profile.as_deref(),
    ) {
        (Some(cli_profile), Some(alias_profile)) if cli_profile != alias_profile => {
            Err(BuildError::Argument(format!(
                "model alias selects provider_profile '{alias_profile}', but \
                 --provider-profile '{cli_profile}' was also supplied",
            )))
        }
        (Some(profile), _) | (None, Some(profile)) => Ok(Some(profile.to_owned())),
        (None, None) => Ok(None),
    }
}

fn selected_provider_profile<'a>(
    name: Option<&str>,
    settings: &'a NornSettings,
) -> Result<Option<&'a ProviderProfileSettings>, BuildError> {
    let Some(name) = name else {
        return Ok(None);
    };
    ProviderProfileId::new(name).map_err(|err| {
        BuildError::Argument(format!("invalid --provider-profile '{name}': {err}"))
    })?;
    let profiles = settings.provider_profiles.as_ref().ok_or_else(|| {
        BuildError::Argument(format!(
            "--provider-profile {name} was supplied, but settings.provider_profiles is empty",
        ))
    })?;
    profiles.get(name).map(Some).ok_or_else(|| {
        BuildError::Argument(format!(
            "unknown provider profile '{name}' in settings.provider_profiles",
        ))
    })
}

fn resolve_api_shape(
    cli: &Cli,
    model_selection: &ResolvedModelSelection,
    profile: Option<&ProviderProfileSettings>,
) -> Result<ApiShape, BuildError> {
    if let Some(shape) = cli.api_shape {
        if let Some(alias_shape) = model_selection.api_shape.as_deref() {
            let cli_shape = api_shape_arg_to_api_shape(shape);
            let alias_shape = alias_shape.parse::<ApiShape>().map_err(|err| {
                BuildError::Argument(format!("invalid model alias api_shape: {err}"))
            })?;
            if cli_shape != alias_shape {
                return Err(BuildError::Argument(
                    "model alias selects a different api_shape than --api-shape; \
                     choose one provider selection path"
                        .to_owned(),
                ));
            }
        }
        return Ok(api_shape_arg_to_api_shape(shape));
    }
    if let Some(alias_shape) = model_selection.api_shape.as_deref() {
        return alias_shape
            .parse::<ApiShape>()
            .map_err(|err| BuildError::Argument(format!("invalid model alias api_shape: {err}")));
    }
    if let Some(profile) = profile {
        let Some(shape) = profile.api_shape.as_deref() else {
            return Err(BuildError::Argument(
                "--provider-profile requires that the profile define api_shape, \
                 or that --api-shape be supplied"
                    .to_owned(),
            ));
        };
        return shape.parse::<ApiShape>().map_err(|err| {
            BuildError::Argument(format!("invalid provider profile api_shape: {err}"))
        });
    }
    Ok(ApiShape::OpenAiResponses)
}

fn api_shape_arg_to_api_shape(shape: ApiShapeKind) -> ApiShape {
    match shape {
        ApiShapeKind::OpenaiResponses => ApiShape::OpenAiResponses,
        ApiShapeKind::OpenaiChatCompletions => ApiShape::OpenAiChatCompletions,
        ApiShapeKind::AnthropicMessages => ApiShape::AnthropicMessages,
        ApiShapeKind::OpenaiHarmony => ApiShape::OpenAiHarmony,
        ApiShapeKind::LmstudioNative => ApiShape::LmStudioNative,
        ApiShapeKind::AgentRpc => ApiShape::AgentRpc,
        ApiShapeKind::AgentClientProtocol => ApiShape::AgentClientProtocol,
    }
}

fn provider_kind_for_shape(shape: ApiShape) -> Result<ProviderKind, BuildError> {
    match shape {
        ApiShape::OpenAiResponses => Ok(ProviderKind::Openai),
        ApiShape::OpenAiChatCompletions => Ok(ProviderKind::OpenaiCompatible),
        ApiShape::AnthropicMessages => Err(unimplemented_shape("anthropic_messages")),
        ApiShape::OpenAiHarmony => Err(unimplemented_shape("openai_harmony")),
        ApiShape::LmStudioNative => Err(unimplemented_shape("lmstudio_native")),
        ApiShape::AgentRpc => Err(unimplemented_shape("agent_rpc")),
        ApiShape::AgentClientProtocol => Err(unimplemented_shape("agent_client_protocol")),
    }
}

fn unimplemented_shape(shape: &str) -> BuildError {
    BuildError::Argument(format!(
        "api shape '{shape}' is reserved but not implemented in this runtime yet",
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use norn::config::{ProviderProfileSettings, ProviderSettings};

    use super::*;

    fn cli(args: &[&str]) -> Cli {
        <Cli as clap::Parser>::try_parse_from(args).unwrap()
    }

    #[test]
    fn default_selection_is_openai_responses() {
        let model = ResolvedModelSelection {
            model: "gpt-5.5".to_owned(),
            provider_profile: None,
            api_shape: None,
        };
        let selection =
            resolve_provider_selection(&cli(&["norn"]), &NornSettings::default(), &model).unwrap();
        assert_eq!(selection.kind, ProviderKind::Openai);
        assert!(selection.profile_name.is_none());
    }

    #[test]
    fn api_shape_selects_chat_completions_provider() {
        let model = ResolvedModelSelection {
            model: "local".to_owned(),
            provider_profile: None,
            api_shape: None,
        };
        let selection = resolve_provider_selection(
            &cli(&["norn", "--api-shape", "openai-chat-completions"]),
            &NornSettings::default(),
            &model,
        )
        .unwrap();
        assert_eq!(selection.kind, ProviderKind::OpenaiCompatible);
    }

    #[test]
    fn provider_profile_uses_profile_api_shape() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "lmstudio".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("openai_chat_completions".to_owned()),
                provider: ProviderSettings::default(),
            },
        );
        let settings = NornSettings {
            provider_profiles: Some(profiles),
            ..NornSettings::default()
        };
        let model = ResolvedModelSelection {
            model: "google/gemma-4-e4b".to_owned(),
            provider_profile: None,
            api_shape: None,
        };
        let selection = resolve_provider_selection(
            &cli(&["norn", "--provider-profile", "lmstudio"]),
            &settings,
            &model,
        )
        .unwrap();
        assert_eq!(selection.kind, ProviderKind::OpenaiCompatible);
        assert_eq!(selection.profile_name.as_deref(), Some("lmstudio"));
    }

    #[test]
    fn model_alias_backend_selection_is_used() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "lmstudio".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("openai_chat_completions".to_owned()),
                provider: ProviderSettings::default(),
            },
        );
        let settings = NornSettings {
            provider_profiles: Some(profiles),
            ..NornSettings::default()
        };
        let model = ResolvedModelSelection {
            model: "google/gemma-4-e4b".to_owned(),
            provider_profile: Some("lmstudio".to_owned()),
            api_shape: None,
        };
        let selection = resolve_provider_selection(&cli(&["norn"]), &settings, &model).unwrap();
        assert_eq!(selection.kind, ProviderKind::OpenaiCompatible);
        assert_eq!(selection.profile_name.as_deref(), Some("lmstudio"));
    }

    #[test]
    fn reserved_api_shape_errors_loudly() {
        let model = ResolvedModelSelection {
            model: "gpt-5.5".to_owned(),
            provider_profile: None,
            api_shape: None,
        };
        let err = resolve_provider_selection(
            &cli(&["norn", "--api-shape", "anthropic-messages"]),
            &NornSettings::default(),
            &model,
        )
        .unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }
}
