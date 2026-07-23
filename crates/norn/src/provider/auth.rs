//! Authentication providers for LLM providers.
//!
//! Two production paths are exposed:
//!
//! - [`OAuthAuthProvider`] — primary path. OAuth 2.0 Authorization Code
//!   with PKCE against `auth.openai.com`.
//!   Tokens persist in Norn-owned storage at
//!   the active Norn-owned account. The compatibility slot remains at
//!   `$NORN_HOME/auth/auth.json` (default `~/.norn/auth/auth.json`).
//! - [`ApiKeyAuthProvider`] — testing only. Used by env-gated
//!   integration tests reading `OPENAI_TEST_KEY` and by API-key based
//!   providers such as OpenAI-compatible Chat Completions endpoints.
//!
//! Providers route construction through [`build_from_auth_source`].

use std::path::PathBuf;
use std::sync::Arc;

use super::CredentialIdentity;
use super::openai_oauth::{
    AuthCredentialsStoreMode, AuthManager, AuthManagerBuildError, LogoutReport, NornAuthRoot,
    OAuthCredentialIdentity, OAuthHttpOptions, RefreshTokenError, resolve_norn_auth_root,
};
use super::startup_trace;
use async_trait::async_trait;

use super::request::SecretString;
use crate::error::{ConfigError, NornError, OAuthCredentialFailureKind, ProviderError};

mod accounts;
mod login;
mod static_codex;

#[cfg(test)]
use super::openai_oauth::{LoginError, LoginStorageFailureKind};
pub use super::openai_oauth::{LoginPrompt, LoginPromptError, LoginPromptPresenter};
#[cfg(test)]
use crate::error::TransientKind;
pub use accounts::{
    command_account_root, list_auth_accounts, login_named, logout_all_auth_accounts, logout_named,
    provider_account_root, use_auth_account,
};
#[cfg(test)]
use login::map_login_error;
pub use login::{LoginConfig, login};
pub(crate) use static_codex::StaticCodexAuthProvider;
pub use static_codex::StaticCodexCredential;

/// Where a provider's authentication credentials come from.
///
/// OAuth is the default for the Codex subscription backend. The `ApiKey`
/// variant is used by direct API backends and OpenAI-compatible endpoints.
#[derive(Clone, Debug)]
pub enum AuthSource {
    /// OAuth via `OpenAI` `ChatGPT` auth. Reads and refreshes tokens in
    /// Norn-owned storage.
    OAuth {
        /// Optional absolute override for the Norn OAuth credential root.
        /// `None` resolves the active named account, falling back to
        /// `$NORN_HOME/auth` (default `~/.norn/auth`).
        /// Supplying a path declares it Norn-owned; this is not a Codex import
        /// surface and must not point at a foreign credential directory.
        auth_root: Option<PathBuf>,
    },

    /// Direct API key.
    ApiKey {
        /// The API key.
        key: SecretString,
    },
}

impl AuthSource {
    /// Returns OAuth construction using the active/default Norn account.
    #[must_use]
    pub const fn oauth_default() -> Self {
        Self::OAuth { auth_root: None }
    }
}

impl Default for AuthSource {
    fn default() -> Self {
        Self::oauth_default()
    }
}

/// Abstraction over how authentication is applied to outgoing HTTP
/// requests.
///
/// Implementations are `Send + Sync` and shared via
/// `Arc<dyn AuthProvider>`. The trait is object-safe: `async_trait`
/// boxes the futures so methods can be invoked through a trait object.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Stable opaque identity for this credential before provider authority
    /// scope is applied.
    ///
    /// `None` means no usable identity is currently pinned. Stateful providers
    /// must reject construction rather than dispatch unbound state. The
    /// `OpenAI` provider combines this value with its resolved backend and
    /// normalized endpoint before exposing session-facing state identity.
    fn credential_identity(&self) -> Option<CredentialIdentity> {
        None
    }

    /// Resolves credential identity while preserving a typed failure when
    /// credentials exist but cannot safely identify their principal.
    ///
    /// The default keeps existing embedder implementations source-compatible;
    /// authentication providers with a distinct indeterminate state override
    /// this method.
    fn resolve_credential_identity(&self) -> Result<Option<CredentialIdentity>, ProviderError> {
        Ok(self.credential_identity())
    }

    /// Applies authentication headers to the provided `RequestBuilder`.
    ///
    /// For OAuth this sets `Authorization: Bearer <access_token>` and,
    /// when an account id is available, `chatgpt-account-id:
    /// <account_id>`. For API key this sets only the bearer header.
    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError>;

    /// Called by the caller on `401 Unauthorized`.
    ///
    /// Returns `Ok(true)` if the credential was refreshed and the
    /// caller should retry the request once. Returns `Ok(false)` if
    /// no refresh is possible (e.g. API key). Returns `Err` if the
    /// credential is permanently invalid; the caller must fail the
    /// request.
    async fn on_unauthorized(&self) -> Result<bool, ProviderError>;
}

/// OAuth-backed [`AuthProvider`] wrapping an `OpenAI` OAuth [`AuthManager`].
///
/// Proactive refresh is performed inside `AuthManager::auth()` so each
/// call to [`apply_auth`](AuthProvider::apply_auth) sees a fresh token
/// when the cached one is approaching expiry. On `401`, the provider
/// triggers a token-authority refresh and reports the outcome via
/// [`on_unauthorized`](AuthProvider::on_unauthorized).
pub struct OAuthAuthProvider {
    manager: Arc<AuthManager>,
}

impl std::fmt::Debug for OAuthAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthAuthProvider").finish_non_exhaustive()
    }
}

impl OAuthAuthProvider {
    /// Constructs a new OAuth provider, initialising the underlying
    /// `AuthManager` with the documented default [`OAuthHttpOptions`].
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::AuthenticationFailed`] if the Norn auth root
    /// cannot be resolved, or
    /// [`ProviderError::ConnectionFailed`] if the manager's HTTP client
    /// cannot be built.
    pub async fn new(auth_root: Option<PathBuf>) -> Result<Self, ProviderError> {
        let provider_started = startup_trace::start("oauth_auth_provider_new_start");
        let auth_root = accounts::provider_root_from_override(auth_root)?;
        startup_trace::elapsed("oauth_auth_root_resolved", provider_started);
        let manager_started = startup_trace::start("oauth_auth_manager_shared_start");
        let manager = AuthManager::shared(
            auth_root,
            AuthCredentialsStoreMode::File,
            OAuthHttpOptions::default(),
        )
        .await
        .map_err(|error| match error {
            AuthManagerBuildError::HttpClient { .. } => ProviderError::ConnectionFailed {
                reason: error.to_string(),
                kind: crate::error::TransientKind::ConnectionReset,
            },
            AuthManagerBuildError::CredentialStorage(_)
            | AuthManagerBuildError::MalformedCredential { .. }
            | AuthManagerBuildError::ConfigurationConflict => ProviderError::AuthenticationFailed {
                reason: error.to_string(),
            },
            AuthManagerBuildError::CredentialCoordination { .. } => {
                ProviderError::ConnectionFailed {
                    reason: error.to_string(),
                    kind: crate::error::TransientKind::ConnectionReset,
                }
            }
        })?;
        startup_trace::elapsed("oauth_auth_manager_shared_done", manager_started);
        startup_trace::elapsed("oauth_auth_provider_new_done", provider_started);
        Ok(Self { manager })
    }

    /// Constructs an `OAuthAuthProvider` directly from a shared
    /// `AuthManager` for unit tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn from_manager(manager: Arc<AuthManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl AuthProvider for OAuthAuthProvider {
    fn credential_identity(&self) -> Option<CredentialIdentity> {
        match self.manager.credential_identity() {
            OAuthCredentialIdentity::Present(identity) => Some(identity),
            OAuthCredentialIdentity::MissingCredentials
            | OAuthCredentialIdentity::MissingUserIdentity => None,
        }
    }

    fn resolve_credential_identity(&self) -> Result<Option<CredentialIdentity>, ProviderError> {
        match self.manager.credential_identity() {
            OAuthCredentialIdentity::MissingCredentials => Ok(None),
            OAuthCredentialIdentity::MissingUserIdentity => {
                Err(ProviderError::AuthenticationFailed {
                    reason: "OAuth credential lacks the stable user identity required for stateful sessions"
                        .to_owned(),
                })
            }
            OAuthCredentialIdentity::Present(identity) => Ok(Some(identity)),
        }
    }

    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let auth = self
            .manager
            .auth()
            .await
            .map_err(map_refresh_token_error)?
            .ok_or_else(|| ProviderError::AuthenticationFailed {
                reason: "no OAuth token found; run `norn auth login`".to_string(),
            })?;
        let token = auth
            .get_token()
            .map_err(|e| ProviderError::AuthenticationFailed {
                reason: format!("failed to extract bearer token from OAuth credentials: {e}"),
            })?;
        let mut req = request.header("Authorization", format!("Bearer {token}"));
        if let Some(account_id) = auth.get_account_id() {
            req = req.header("chatgpt-account-id", account_id);
        }
        Ok(req)
    }

    async fn on_unauthorized(&self) -> Result<bool, ProviderError> {
        self.manager
            .refresh_token_from_authority()
            .await
            .map(|()| true)
            .map_err(map_refresh_token_error)
    }
}

fn map_refresh_token_error(error: RefreshTokenError) -> ProviderError {
    match error {
        RefreshTokenError::Permanent(failed) => ProviderError::OAuthCredentialFailure {
            kind: OAuthCredentialFailureKind::Permanent,
            reason: format!(
                "OAuth refresh failed permanently: {failed}; please re-run the login flow"
            ),
        },
        RefreshTokenError::Transient(failed) => ProviderError::ConnectionFailed {
            reason: format!(
                "OAuth token refresh failed transiently: {failed}; the request may be retried"
            ),
            kind: crate::error::TransientKind::ConnectionReset,
        },
        RefreshTokenError::Undurable(failed) => ProviderError::OAuthCredentialFailure {
            kind: OAuthCredentialFailureKind::Undurable,
            reason: format!(
                "OAuth token refresh completed but the credential is not durable: {failed}"
            ),
        },
        RefreshTokenError::Conflict(failed) => ProviderError::OAuthCredentialFailure {
            kind: OAuthCredentialFailureKind::Conflict,
            reason: format!("OAuth credential changed during refresh: {failed}"),
        },
        RefreshTokenError::Indeterminate(failed) => ProviderError::OAuthCredentialFailure {
            kind: OAuthCredentialFailureKind::Indeterminate,
            reason: format!(
                "OAuth authority may have rotated the credential without returning a usable lineage: {failed}; please re-run the login flow"
            ),
        },
        RefreshTokenError::Coordination(failed) => ProviderError::ConnectionFailed {
            reason: format!("OAuth credential coordination failed: {failed}"),
            kind: crate::error::TransientKind::ConnectionReset,
        },
    }
}

/// API-key-backed [`AuthProvider`].
///
/// Used by env-gated integration tests reading `OPENAI_TEST_KEY` and by
/// API-key based providers.
pub struct ApiKeyAuthProvider {
    key: SecretString,
    credential_identity: CredentialIdentity,
}

impl std::fmt::Debug for ApiKeyAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyAuthProvider").finish_non_exhaustive()
    }
}

impl ApiKeyAuthProvider {
    /// Constructs a new API-key provider wrapping the given secret.
    #[must_use]
    pub fn new(key: SecretString) -> Self {
        let credential_identity = CredentialIdentity::from_api_key(key.expose());
        Self {
            key,
            credential_identity,
        }
    }
}

#[async_trait]
impl AuthProvider for ApiKeyAuthProvider {
    fn credential_identity(&self) -> Option<CredentialIdentity> {
        Some(self.credential_identity)
    }

    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(request.header("Authorization", format!("Bearer {}", self.key.expose())))
    }

    async fn on_unauthorized(&self) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

/// Builds the concrete [`AuthProvider`] for the given [`AuthSource`].
///
/// # Errors
///
/// Propagates [`ProviderError`] from the underlying provider
/// constructor (e.g. failure to resolve the Norn auth root).
pub async fn build_from_auth_source(
    auth_source: &AuthSource,
) -> Result<Arc<dyn AuthProvider>, ProviderError> {
    match auth_source {
        AuthSource::OAuth { auth_root } => {
            let provider = OAuthAuthProvider::new(auth_root.clone()).await?;
            let arc: Arc<dyn AuthProvider> = Arc::new(provider);
            Ok(arc)
        }
        AuthSource::ApiKey { key } => {
            let provider = ApiKeyAuthProvider::new(key.clone());
            let arc: Arc<dyn AuthProvider> = Arc::new(provider);
            Ok(arc)
        }
    }
}

/// Clears local auth storage before attempting remote token revocation and
/// reports both outcomes independently.
///
/// # Errors
///
/// Returns [`NornError::Config`] when the trusted auth root cannot be resolved.
pub async fn logout(config: LoginConfig) -> Result<LogoutReport, NornError> {
    let base_root = command_norn_auth_root(config.auth_root)?;
    Ok(super::openai_oauth::logout_with_revoke(
        &base_root,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default(),
    )
    .await)
}

pub(super) fn command_norn_auth_root(
    override_path: Option<PathBuf>,
) -> Result<NornAuthRoot, NornError> {
    resolve_norn_auth_root(override_path).map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: error.to_string(),
        })
    })
}

#[cfg(any(test, feature = "test-utils"))]
mod mock;

#[cfg(any(test, feature = "test-utils"))]
pub use mock::MockAuthProvider;

#[cfg(test)]
mod tests;
