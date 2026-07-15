//! CLI adapter for the library-owned provider authentication policy.

use norn::config::{ProviderAuthBackend, resolve_provider_auth as resolve_library_auth};
pub(crate) use norn::config::{ProviderAuthConfigError, ResolvedProviderAuth};

use crate::cli::ProviderKind;

use super::ProviderConfigOverrides;

/// Resolve a provider's auth mode without reading the environment, credential
/// storage, or constructing a provider.
pub(crate) fn resolve_provider_auth(
    kind: ProviderKind,
    overrides: &ProviderConfigOverrides,
) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    let backend = match kind {
        ProviderKind::Openai => ProviderAuthBackend::OpenAi,
        ProviderKind::OpenaiCompatible => ProviderAuthBackend::OpenAiCompatible,
        ProviderKind::ClaudeRunner => ProviderAuthBackend::ClaudeRunner,
    };
    resolve_library_auth(backend, overrides.auth, overrides.api_key_env.as_deref())
}

#[cfg(test)]
#[path = "provider_auth_tests.rs"]
mod tests;
