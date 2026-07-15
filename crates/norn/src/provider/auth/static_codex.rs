//! Dispatch-scoped static Codex credentials.

use async_trait::async_trait;

use super::AuthProvider;
use crate::error::ProviderError;
use crate::provider::request::SecretString;

/// Validated, non-refreshing `ChatGPT` credential for an embedded Codex
/// provider.
///
/// Construction accepts only the access token plus optional account id that
/// the credential owner selected for this dispatch. Refresh tokens, ID tokens,
/// API-key slots, storage paths, and token-authority controls cannot cross this
/// boundary. The only production consumer is
/// [`OpenAiProvider::with_static_codex_credential`](crate::provider::openai::OpenAiProvider::with_static_codex_credential),
/// which additionally pins the credential to the compiled Codex backend.
pub struct StaticCodexCredential {
    access_token: SecretString,
    account_id: Option<SecretString>,
}

impl std::fmt::Debug for StaticCodexCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticCodexCredential")
            .field("access_token", &"[REDACTED]")
            .field("account_id_present", &self.account_id.is_some())
            .finish()
    }
}

impl StaticCodexCredential {
    /// Validates and seals dispatch-scoped `ChatGPT` request credentials.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::AuthenticationFailed`] when either supplied
    /// value cannot be represented safely as its HTTP header. Errors are
    /// structural and never include credential values.
    pub fn new(
        access_token: SecretString,
        account_id: Option<SecretString>,
    ) -> Result<Self, ProviderError> {
        validate_static_header_value("access token", access_token.expose(), true)?;
        if let Some(account_id) = account_id.as_ref() {
            validate_static_header_value("account id", account_id.expose(), false)?;
        }

        Ok(Self {
            access_token,
            account_id,
        })
    }
}

fn validate_static_header_value(
    field: &str,
    value: &str,
    bearer_prefix: bool,
) -> Result<(), ProviderError> {
    if value.is_empty() || value.trim() != value {
        return Err(static_credential_error(&format!(
            "static Codex credential {field} is empty or has surrounding whitespace",
        )));
    }
    let header_value = if bearer_prefix {
        format!("Bearer {value}")
    } else {
        value.to_owned()
    };
    reqwest::header::HeaderValue::try_from(header_value.as_str()).map_err(|error| {
        tracing::debug!(
            field,
            error = %error,
            "rejected non-header-safe static Codex credential field",
        );
        static_credential_error(&format!(
            "static Codex credential {field} is not a valid HTTP header value",
        ))
    })?;
    Ok(())
}

fn static_credential_error(reason: &str) -> ProviderError {
    ProviderError::AuthenticationFailed {
        reason: reason.to_owned(),
    }
}

/// Internal request authenticator backed by a sealed static credential.
pub(crate) struct StaticCodexAuthProvider {
    credential: StaticCodexCredential,
}

impl StaticCodexAuthProvider {
    /// Binds a sealed credential to the non-refreshing authenticator.
    pub(crate) const fn new(credential: StaticCodexCredential) -> Self {
        Self { credential }
    }
}

#[async_trait]
impl AuthProvider for StaticCodexAuthProvider {
    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let mut request = request.header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.credential.access_token.expose()),
        );
        if let Some(account_id) = self.credential.account_id.as_ref() {
            request = request.header("chatgpt-account-id", account_id.expose());
        }
        Ok(request)
    }

    async fn on_unauthorized(&self) -> Result<bool, ProviderError> {
        Err(ProviderError::AuthenticationFailed {
            reason: "HTTP 401 Unauthorized for a dispatch-scoped static Codex credential; the credential owner must replace or refresh it before retrying"
                .to_owned(),
        })
    }
}
