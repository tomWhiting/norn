//! Trusted backend resolution for the `OpenAI` Responses provider.

use url::Url;

use super::request::{CATALOG_BACKEND_CODEX_SUBSCRIPTION, CATALOG_BACKEND_RESPONSES_API};
use crate::error::ProviderError;
use crate::provider::auth::AuthSource;

pub(super) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub(super) const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

const CHATGPT_SCHEME: &str = "https";
const CHATGPT_HOST: &str = "chatgpt.com";
const CHATGPT_PORT: u16 = 443;
const CHATGPT_PATH: &str = "/backend-api/codex";
const CHATGPT_PATH_WITH_TRAILING_SLASH: &str = "/backend-api/codex/";

/// Immutable backend identity resolved before credentials are initialized.
#[derive(Clone, Eq, PartialEq)]
pub(super) enum OpenAiBackend {
    /// `ChatGPT` Codex subscription endpoint authenticated by OAuth.
    CodexSubscription,
    /// Direct/public Responses endpoint authenticated by an API key.
    ResponsesApi { base_url: String },
}

impl std::fmt::Debug for OpenAiBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

impl OpenAiBackend {
    /// Resolves and validates the backend selected by auth plus endpoint config.
    pub(super) fn resolve(
        auth_source: &AuthSource,
        configured_base_url: Option<&str>,
    ) -> Result<Self, ProviderError> {
        match auth_source {
            AuthSource::OAuth { .. } => resolve_codex_subscription(configured_base_url),
            AuthSource::ApiKey { .. } => {
                let base_url = crate::provider::endpoint::validated_credential_base_url(
                    configured_base_url.unwrap_or(DEFAULT_BASE_URL),
                )?;
                crate::provider::endpoint::reject_chatgpt_api_key_destination(&base_url)?;
                Ok(Self::ResponsesApi { base_url })
            }
        }
    }

    /// Network base URL owned by this backend identity.
    pub(super) fn base_url(&self) -> &str {
        match self {
            Self::CodexSubscription => CHATGPT_BASE_URL,
            Self::ResponsesApi { base_url } => base_url,
        }
    }

    /// Whether this is the `ChatGPT` Codex subscription backend.
    pub(super) const fn is_codex_subscription(&self) -> bool {
        matches!(self, Self::CodexSubscription)
    }

    /// Model-catalog backend identifier for request policy resolution.
    pub(super) const fn catalog_backend(&self) -> &'static str {
        match self {
            Self::CodexSubscription => CATALOG_BACKEND_CODEX_SUBSCRIPTION,
            Self::ResponsesApi { .. } => CATALOG_BACKEND_RESPONSES_API,
        }
    }

    /// Non-secret label suitable for diagnostics.
    pub(super) const fn label(&self) -> &'static str {
        match self {
            Self::CodexSubscription => CATALOG_BACKEND_CODEX_SUBSCRIPTION,
            Self::ResponsesApi { .. } => CATALOG_BACKEND_RESPONSES_API,
        }
    }
}

fn resolve_codex_subscription(
    configured_base_url: Option<&str>,
) -> Result<OpenAiBackend, ProviderError> {
    if let Some(candidate) = configured_base_url {
        validate_codex_base_url(candidate)?;
    }
    Ok(OpenAiBackend::CodexSubscription)
}

fn validate_codex_base_url(candidate: &str) -> Result<(), ProviderError> {
    let candidate = candidate.trim();
    let has_ambiguous_syntax = !candidate.is_ascii()
        || candidate.contains('\\')
        || candidate.contains('%')
        || candidate
            .split('/')
            .any(|segment| matches!(segment, "." | ".."));
    if has_ambiguous_syntax {
        return Err(oauth_destination_error());
    }

    let url = match Url::parse(candidate) {
        Ok(url) => url,
        Err(parse_error) => {
            tracing::debug!(
                error = %parse_error,
                "rejected malformed Codex OAuth base URL"
            );
            return Err(oauth_destination_error());
        }
    };
    let path_is_canonical = matches!(url.path(), CHATGPT_PATH | CHATGPT_PATH_WITH_TRAILING_SLASH);
    let destination_is_canonical = url.scheme() == CHATGPT_SCHEME
        && url.host_str() == Some(CHATGPT_HOST)
        && url.port_or_known_default() == Some(CHATGPT_PORT)
        && !crate::provider::endpoint::has_explicit_userinfo(candidate)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && path_is_canonical;

    if destination_is_canonical {
        Ok(())
    } else {
        Err(oauth_destination_error())
    }
}

fn oauth_destination_error() -> ProviderError {
    ProviderError::InvalidRequest {
        message: "Codex OAuth credentials are restricted to the built-in ChatGPT endpoint; remove provider.base_url or use API-key authentication for a custom endpoint"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::request::SecretString;

    fn oauth_source() -> AuthSource {
        AuthSource::OAuth { auth_root: None }
    }

    fn api_key_source() -> AuthSource {
        AuthSource::ApiKey {
            key: SecretString::new("test-key"),
        }
    }

    #[test]
    fn oauth_without_override_resolves_to_compiled_codex_backend() -> Result<(), ProviderError> {
        let backend = OpenAiBackend::resolve(&oauth_source(), None)?;
        assert_eq!(backend, OpenAiBackend::CodexSubscription);
        assert_eq!(backend.base_url(), CHATGPT_BASE_URL);
        Ok(())
    }

    #[test]
    fn normalized_canonical_oauth_spellings_use_compiled_url() -> Result<(), ProviderError> {
        for candidate in [
            CHATGPT_BASE_URL,
            "https://chatgpt.com/backend-api/codex/",
            "HTTPS://CHATGPT.COM/backend-api/codex",
            "https://chatgpt.com:443/backend-api/codex",
            "  https://chatgpt.com/backend-api/codex  ",
        ] {
            let backend = OpenAiBackend::resolve(&oauth_source(), Some(candidate))?;
            assert_eq!(backend, OpenAiBackend::CodexSubscription);
            assert_eq!(backend.base_url(), CHATGPT_BASE_URL);
        }
        Ok(())
    }

    #[test]
    fn oauth_rejects_every_noncanonical_destination_without_echoing_it() {
        for candidate in [
            "http://chatgpt.com/backend-api/codex",
            "https://attacker.example/backend-api/codex",
            "https://chatgpt.com.evil.example/backend-api/codex",
            "https://chatgpt.com./backend-api/codex",
            "https://chatgpt.com:444/backend-api/codex",
            "https://@chatgpt.com/backend-api/codex",
            "https://user:secret@chatgpt.com/backend-api/codex",
            "https://127.0.0.1/backend-api/codex",
            "https://localhost/backend-api/codex",
            "https://chatgpt.com/backend-api",
            "https://chatgpt.com/backend-api/codex/responses",
            "https://chatgpt.com/backend-api//codex",
            "https://chatgpt.com/backend-api/codex?redirect=evil",
            "https://chatgpt.com/backend-api/codex#fragment",
            "https://xn--chatgpt-9za.com/backend-api/codex",
            "https://chatgpt.com/backend-api/x/../codex",
            "https://chatgpt.com/backend-api/%2e%2e/codex",
            "https:\\chatgpt.com\\backend-api\\codex",
            "https://chatgpt。com/backend-api/codex",
            "not a URL",
            "",
        ] {
            let result = OpenAiBackend::resolve(&oauth_source(), Some(candidate));
            assert!(
                matches!(result, Err(ProviderError::InvalidRequest { .. })),
                "OAuth destination should be rejected: {candidate}"
            );
            let rendered = format!("{result:?}");
            assert!(!rendered.contains("user:secret"));
            assert!(!rendered.contains("attacker.example"));
        }
    }

    #[test]
    fn api_key_custom_endpoint_remains_a_responses_backend() -> Result<(), ProviderError> {
        let backend = OpenAiBackend::resolve(&api_key_source(), Some("http://localhost:11434/v1"))?;
        assert_eq!(
            backend,
            OpenAiBackend::ResponsesApi {
                base_url: "http://localhost:11434/v1".to_owned(),
            }
        );
        assert_eq!(backend.catalog_backend(), CATALOG_BACKEND_RESPONSES_API);
        Ok(())
    }

    #[test]
    fn api_key_auth_cannot_select_the_private_codex_endpoint() {
        for candidate in [
            CHATGPT_BASE_URL,
            "https://chatgpt.com:443/backend-api/codex/",
            "HTTPS://CHATGPT.COM/backend-api/codex",
            "https://chatgpt.com./backend-api/codex",
            "https://chatgpt.com/backend-api/%63odex",
        ] {
            let result = OpenAiBackend::resolve(&api_key_source(), Some(candidate));
            assert!(
                matches!(result, Err(ProviderError::InvalidRequest { .. })),
                "API-key auth should not select the Codex endpoint: {candidate}",
            );
        }
    }
}
