//! Public embedder coverage for the library-owned provider-auth policy.

use norn::config::{
    NornSettings, ProviderAuthBackend, ProviderAuthConfigError, ProviderAuthMode, ProviderSettings,
    ResolvedProviderAuth,
};
use norn::runtime_init::provider_settings_from_settings;

fn settings(
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&str>,
) -> Result<norn::runtime_init::ProviderSettingsResolved, norn::error::NornError> {
    provider_settings_from_settings(&NornSettings {
        provider: Some(ProviderSettings {
            auth,
            api_key_env: api_key_env.map(str::to_owned),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    })
}

#[test]
fn embedder_resolves_auth_before_accessing_a_secret() -> Result<(), Box<dyn std::error::Error>> {
    let openai_api_key = settings(
        Some(ProviderAuthMode::ApiKey),
        Some("MERIDIAN_OPENAI_API_KEY"),
    )?;
    assert_eq!(
        openai_api_key.resolve_auth(ProviderAuthBackend::OpenAi),
        Ok(ResolvedProviderAuth::ApiKeyEnv(
            "MERIDIAN_OPENAI_API_KEY".to_owned(),
        )),
    );

    let claude = settings(None, None)?;
    assert_eq!(
        claude.resolve_auth(ProviderAuthBackend::ClaudeRunner),
        Ok(ResolvedProviderAuth::None),
    );
    Ok(())
}

#[test]
fn embedder_receives_typed_rejections() -> Result<(), Box<dyn std::error::Error>> {
    let openai_oauth = settings(Some(ProviderAuthMode::OAuth), Some("MUST_NOT_BE_READ"))?;
    assert_eq!(
        openai_oauth.resolve_auth(ProviderAuthBackend::OpenAi),
        Err(ProviderAuthConfigError::OpenAiOAuthWithApiKeyEnv),
    );

    let compatible_oauth = settings(Some(ProviderAuthMode::OAuth), None)?;
    assert_eq!(
        compatible_oauth.resolve_auth(ProviderAuthBackend::OpenAiCompatible),
        Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
    );
    Ok(())
}
