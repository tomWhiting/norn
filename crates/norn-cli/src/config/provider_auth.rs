//! Pure provider authentication-mode resolution.
//!
//! This module deliberately has no environment, filesystem, provider, or
//! network dependencies. Runtime assembly resolves the complete mode/companion
//! matrix here before any code is allowed to look up a secret or credential.

use norn::config::ProviderAuthMode;

use crate::cli::ProviderKind;

use super::ProviderConfigOverrides;

/// Documented compatibility fallback for OpenAI-compatible providers when
/// neither `auth` nor `api_key_env` is configured.
pub(crate) const DEFAULT_OPENAI_COMPAT_API_KEY_ENV: &str = "NORN_OPENAI_COMPAT_API_KEY";

/// Authentication source selected by the validated configuration matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedProviderAuth {
    /// Norn-owned OAuth credential storage.
    OAuth,
    /// API key read later from the named environment variable.
    ApiKeyEnv(String),
    /// Backend has no Norn-managed authentication source.
    None,
}

/// Invalid provider authentication configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProviderAuthConfigError {
    /// `OpenAI` Responses was pinned to OAuth while an API-key source remained.
    #[error("provider openai with auth=oauth forbids provider.api_key_env")]
    OpenAiOAuthWithApiKeyEnv,
    /// `OpenAI` Responses was pinned to API-key mode without a source name.
    #[error("provider openai with auth=api_key requires provider.api_key_env")]
    OpenAiApiKeyWithoutEnv,
    /// OpenAI-compatible backends do not support Norn OAuth credentials.
    #[error("provider openai-compatible does not support auth=oauth")]
    OpenAiCompatibleOAuth,
    /// Explicit API-key mode requires an explicit source for this backend.
    #[error("provider openai-compatible with auth=api_key requires provider.api_key_env")]
    OpenAiCompatibleApiKeyWithoutEnv,
    /// The configured API-key environment-variable name was empty.
    #[error("provider.api_key_env must be non-empty")]
    EmptyApiKeyEnv,
    /// Claude Runner owns its authentication outside Norn.
    #[error("provider claude-runner does not accept provider.auth")]
    ClaudeRunnerAuth,
    /// Claude Runner does not consume API keys from Norn configuration.
    #[error("provider claude-runner does not accept provider.api_key_env")]
    ClaudeRunnerApiKeyEnv,
}

/// Resolve a provider's auth mode without reading the environment, credential
/// storage, or constructing a provider.
///
/// Omitted mode preserves the established compatibility behavior. Explicit
/// mode never falls back: its required and forbidden companion fields are
/// enforced exactly.
pub(crate) fn resolve_provider_auth(
    kind: ProviderKind,
    overrides: &ProviderConfigOverrides,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match kind {
        ProviderKind::Openai => resolve_openai(overrides),
        ProviderKind::OpenaiCompatible => resolve_openai_compatible(overrides),
        ProviderKind::ClaudeRunner => resolve_claude_runner(overrides),
    }
}

fn resolve_openai(
    overrides: &ProviderConfigOverrides,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match (overrides.auth, overrides.api_key_env.as_deref()) {
        (None | Some(ProviderAuthMode::OAuth), None) => Ok(ResolvedProviderAuth::OAuth),
        (None | Some(ProviderAuthMode::ApiKey), Some(name)) => api_key_env(name),
        (Some(ProviderAuthMode::OAuth), Some(_)) => {
            Err(ProviderAuthConfigError::OpenAiOAuthWithApiKeyEnv)
        }
        (Some(ProviderAuthMode::ApiKey), None) => {
            Err(ProviderAuthConfigError::OpenAiApiKeyWithoutEnv)
        }
    }
}

fn resolve_openai_compatible(
    overrides: &ProviderConfigOverrides,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match (overrides.auth, overrides.api_key_env.as_deref()) {
        (Some(ProviderAuthMode::OAuth), _) => Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        (Some(ProviderAuthMode::ApiKey), None) => {
            Err(ProviderAuthConfigError::OpenAiCompatibleApiKeyWithoutEnv)
        }
        (None, None) => api_key_env(DEFAULT_OPENAI_COMPAT_API_KEY_ENV),
        (None | Some(ProviderAuthMode::ApiKey), Some(name)) => api_key_env(name),
    }
}

fn resolve_claude_runner(
    overrides: &ProviderConfigOverrides,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    if overrides.auth.is_some() {
        return Err(ProviderAuthConfigError::ClaudeRunnerAuth);
    }
    if overrides.api_key_env.is_some() {
        return Err(ProviderAuthConfigError::ClaudeRunnerApiKeyEnv);
    }
    Ok(ResolvedProviderAuth::None)
}

fn api_key_env(name: &str) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    if name.trim().is_empty() {
        return Err(ProviderAuthConfigError::EmptyApiKeyEnv);
    }
    Ok(ResolvedProviderAuth::ApiKeyEnv(name.to_owned()))
}

#[cfg(test)]
#[path = "provider_auth_tests.rs"]
mod tests;
