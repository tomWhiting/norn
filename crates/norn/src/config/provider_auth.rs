//! Provider authentication policy shared by CLI and library embedders.

use super::ProviderAuthMode;

/// Compatibility API-key source for OpenAI-compatible providers when neither
/// authentication field is configured.
const DEFAULT_OPENAI_COMPAT_API_KEY_ENV: &str = "NORN_OPENAI_COMPAT_API_KEY";

/// Provider family whose settings authentication policy is being resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAuthBackend {
    /// `OpenAI` Responses through the compiled Codex or direct API backend.
    OpenAi,
    /// A configurable OpenAI-compatible endpoint.
    OpenAiCompatible,
    /// The Claude Code subprocess integration.
    ClaudeRunner,
}

/// Authentication source selected by the validated settings matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedProviderAuth {
    /// Norn-owned OAuth credential storage.
    OAuth,
    /// API key read later from the named environment variable.
    ApiKeyEnv(String),
    /// Backend has no Norn-managed authentication source.
    None,
}

/// Invalid provider authentication configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProviderAuthConfigError {
    /// `OpenAI` Responses selected OAuth while an API-key source remained.
    #[error("provider openai with auth=oauth forbids provider.api_key_env")]
    OpenAiOAuthWithApiKeyEnv,
    /// `OpenAI` Responses selected API-key mode without a source name.
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

/// Resolve provider authentication without reading the environment,
/// credential storage, or constructing a provider.
///
/// Omitted mode preserves the established backend defaults. Explicit mode
/// never falls back: its required and forbidden companion fields are enforced
/// exactly.
///
/// # Errors
///
/// Returns [`ProviderAuthConfigError`] when the mode is incompatible with the
/// backend, a required API-key source is absent, or a forbidden source remains.
pub fn resolve_provider_auth(
    backend: ProviderAuthBackend,
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&str>,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match backend {
        ProviderAuthBackend::OpenAi => resolve_openai(auth, api_key_env),
        ProviderAuthBackend::OpenAiCompatible => resolve_openai_compatible(auth, api_key_env),
        ProviderAuthBackend::ClaudeRunner => resolve_claude_runner(auth, api_key_env),
    }
}

fn resolve_openai(
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&str>,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match (auth, api_key_env) {
        (None | Some(ProviderAuthMode::OAuth), None) => Ok(ResolvedProviderAuth::OAuth),
        (None | Some(ProviderAuthMode::ApiKey), Some(name)) => api_key_env_source(name),
        (Some(ProviderAuthMode::OAuth), Some(_)) => {
            Err(ProviderAuthConfigError::OpenAiOAuthWithApiKeyEnv)
        }
        (Some(ProviderAuthMode::ApiKey), None) => {
            Err(ProviderAuthConfigError::OpenAiApiKeyWithoutEnv)
        }
    }
}

fn resolve_openai_compatible(
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&str>,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    match (auth, api_key_env) {
        (Some(ProviderAuthMode::OAuth), _) => Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        (Some(ProviderAuthMode::ApiKey), None) => {
            Err(ProviderAuthConfigError::OpenAiCompatibleApiKeyWithoutEnv)
        }
        (None, None) => api_key_env_source(DEFAULT_OPENAI_COMPAT_API_KEY_ENV),
        (None | Some(ProviderAuthMode::ApiKey), Some(name)) => api_key_env_source(name),
    }
}

fn resolve_claude_runner(
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&str>,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    if auth.is_some() {
        return Err(ProviderAuthConfigError::ClaudeRunnerAuth);
    }
    if api_key_env.is_some() {
        return Err(ProviderAuthConfigError::ClaudeRunnerApiKeyEnv);
    }
    Ok(ResolvedProviderAuth::None)
}

fn api_key_env_source(name: &str) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    if name.trim().is_empty() {
        return Err(ProviderAuthConfigError::EmptyApiKeyEnv);
    }
    Ok(ResolvedProviderAuth::ApiKeyEnv(name.to_owned()))
}

#[cfg(test)]
#[path = "provider_auth_tests.rs"]
mod tests;
