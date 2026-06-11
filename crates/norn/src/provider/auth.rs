//! Authentication providers for LLM providers.
//!
//! Two production paths are exposed:
//!
//! - [`OAuthAuthProvider`] — primary path. OAuth 2.0 Authorization Code
//!   with PKCE against `auth.openai.com`.
//!   Tokens persist at `$CODEX_HOME/auth.json` (default
//!   `~/.codex/auth.json`), shared with the Codex CLI.
//! - [`ApiKeyAuthProvider`] — testing only. Used by env-gated
//!   integration tests reading `OPENAI_TEST_KEY`. Not a recommended
//!   production path.
//!
//! Providers route construction through [`build_from_auth_source`].

use std::path::PathBuf;
use std::sync::Arc;

use super::openai_oauth::{
    AuthCredentialsStoreMode, AuthManager, CLIENT_ID, RefreshTokenError, ServerOptions,
};
use async_trait::async_trait;

use super::request::SecretString;
use crate::error::{ConfigError, NornError, ProviderError};

/// Where a provider's authentication credentials come from.
///
/// OAuth is the default. The `ApiKey` variant exists for env-gated
/// integration tests; it is not a recommended production path.
#[derive(Clone, Debug)]
pub enum AuthSource {
    /// OAuth via `OpenAI` `ChatGPT` auth. Reads and refreshes tokens
    /// stored at `$CODEX_HOME/auth.json`.
    OAuth {
        /// Optional override for the Codex home directory. `None`
        /// resolves to `$CODEX_HOME` if set, otherwise `~/.codex`.
        codex_home: Option<PathBuf>,
    },

    /// Direct API key. **Testing only.**
    ApiKey {
        /// The API key.
        key: SecretString,
    },
}

impl AuthSource {
    /// Returns the default OAuth construction with no codex-home
    /// override.
    #[must_use]
    pub const fn oauth_default() -> Self {
        Self::OAuth { codex_home: None }
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
    /// `AuthManager`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::AuthenticationFailed`] if the codex
    /// home directory cannot be resolved.
    pub async fn new(codex_home: Option<PathBuf>) -> Result<Self, ProviderError> {
        let codex_home = resolve_codex_home(codex_home)?;
        let manager = AuthManager::shared(
            codex_home,
            /* enable_codex_api_key_env */ false,
            AuthCredentialsStoreMode::File,
            /* chatgpt_base_url */ None,
        )
        .await;
        Ok(Self { manager })
    }

    /// Constructs an `OAuthAuthProvider` directly from a shared
    /// `AuthManager`. Used by embedders that seed the manager with an
    /// in-memory `CodexAuth` via `AuthManager::from_static_auth`
    /// (e.g. VM-provisioned credentials), and by tests.
    #[must_use]
    pub fn from_manager(manager: Arc<AuthManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl AuthProvider for OAuthAuthProvider {
    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let auth =
            self.manager
                .auth()
                .await
                .ok_or_else(|| ProviderError::AuthenticationFailed {
                    reason: "no OAuth token found; run the login flow or set up via the Codex CLI"
                        .to_string(),
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
        match self.manager.refresh_token_from_authority().await {
            Ok(()) => Ok(true),
            Err(RefreshTokenError::Permanent(failed)) => Err(ProviderError::AuthenticationFailed {
                reason: format!(
                    "OAuth refresh failed permanently: {failed}; please re-run the login flow"
                ),
            }),
            // A transient refresh failure is a network/server fault, not
            // a missing or dead credential: surface it as a retryable
            // connection failure with an accurate reason. (`Ok(false)`
            // is reserved for "no refresh path exists" — reporting it
            // here made the caller emit a non-retryable
            // `AuthenticationFailed { "no refresh available" }`, the
            // wrong class and a false message.)
            Err(RefreshTokenError::Transient(failed)) => Err(ProviderError::ConnectionFailed {
                reason: format!(
                    "OAuth token refresh failed transiently: {failed}; the stored refresh \
                     credential remains valid and the request may be retried"
                ),
            }),
        }
    }
}

/// API-key-backed [`AuthProvider`].
///
/// **Testing only.** Used by env-gated integration tests reading
/// `OPENAI_TEST_KEY`. Production code should use
/// [`OAuthAuthProvider`].
pub struct ApiKeyAuthProvider {
    key: SecretString,
}

impl std::fmt::Debug for ApiKeyAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyAuthProvider").finish_non_exhaustive()
    }
}

impl ApiKeyAuthProvider {
    /// Constructs a new API-key provider wrapping the given secret.
    #[must_use]
    pub const fn new(key: SecretString) -> Self {
        Self { key }
    }
}

#[async_trait]
impl AuthProvider for ApiKeyAuthProvider {
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
/// constructor (e.g. failure to resolve the codex home directory).
pub async fn build_from_auth_source(
    auth_source: &AuthSource,
) -> Result<Arc<dyn AuthProvider>, ProviderError> {
    match auth_source {
        AuthSource::OAuth { codex_home } => {
            let provider = OAuthAuthProvider::new(codex_home.clone()).await?;
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

/// Configuration for the [`login`] / [`logout`] flows.
#[derive(Clone, Debug, Default)]
pub struct LoginConfig {
    /// Optional override for the Codex home directory.
    pub codex_home: Option<PathBuf>,
    /// Whether to use the device code flow. Currently unsupported;
    /// setting this to `true` returns an error.
    pub device_code: bool,
}

/// Triggers the OAuth PKCE login flow.
///
/// Opens a browser, runs a local callback server, and persists tokens
/// to `auth.json` on success.
///
/// # Errors
///
/// Returns [`NornError::Config`] if `device_code` is set (not yet
/// supported). Returns [`NornError::Provider`] for
/// any underlying login-flow failure (browser failed to open, user
/// cancelled, callback server failed, etc.).
pub async fn login(config: LoginConfig) -> Result<(), NornError> {
    if config.device_code {
        return Err(NornError::Config(ConfigError::InvalidConfig {
            reason: "device code login is not yet supported; use the browser PKCE flow".to_string(),
        }));
    }
    let codex_home = resolve_codex_home(config.codex_home)?;
    let opts = ServerOptions::new(
        codex_home,
        CLIENT_ID.to_string(),
        None,
        AuthCredentialsStoreMode::File,
    );
    let server = super::openai_oauth::run_login_server(opts).map_err(|e| {
        NornError::Provider(ProviderError::AuthenticationFailed {
            reason: format!("login server start failed: {e}"),
        })
    })?;
    server.block_until_done().await.map_err(|e| {
        NornError::Provider(ProviderError::AuthenticationFailed {
            reason: format!("login flow failed: {e}"),
        })
    })?;
    Ok(())
}

/// Revokes any stored OAuth tokens and clears local auth storage.
///
/// # Errors
///
/// Returns [`NornError::Provider`] if the revoke or storage-delete
/// call fails.
pub async fn logout(config: LoginConfig) -> Result<(), NornError> {
    let codex_home = resolve_codex_home(config.codex_home)?;
    super::openai_oauth::logout_with_revoke(&codex_home, AuthCredentialsStoreMode::File)
        .await
        .map_err(|e| {
            NornError::Provider(ProviderError::AuthenticationFailed {
                reason: format!("logout failed: {e}"),
            })
        })?;
    Ok(())
}

fn resolve_codex_home(override_path: Option<PathBuf>) -> Result<PathBuf, ProviderError> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    if let Ok(env_path) = std::env::var("CODEX_HOME")
        && !env_path.is_empty()
    {
        return Ok(PathBuf::from(env_path));
    }
    let home = dirs::home_dir().ok_or_else(|| ProviderError::AuthenticationFailed {
        reason: "could not determine home directory for codex auth storage".to_string(),
    })?;
    Ok(home.join(".codex"))
}

/// Mock auth provider for tests.
///
/// Records call counts and consumes a configurable sequence of bearer
/// tokens (one per `apply_auth` call) and `on_unauthorized` responses
/// (one per `on_unauthorized` call). When a sequence is exhausted, the
/// last value is reused, except that `on_unauthorized` defaults to
/// `Ok(false)` when no responses are pre-configured.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockAuthProvider {
    token_seq: std::sync::Mutex<Vec<String>>,
    on_unauthorized_seq: std::sync::Mutex<Vec<Result<bool, ProviderError>>>,
    apply_count: std::sync::atomic::AtomicUsize,
    refresh_count: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockAuthProvider {
    /// Constructs a mock with sequences of bearer tokens and
    /// `on_unauthorized` responses. Successive calls consume the next
    /// element; once exhausted, the last element is reused.
    #[must_use]
    pub fn new(tokens: Vec<String>, on_unauthorized: Vec<Result<bool, ProviderError>>) -> Self {
        let mut tokens_reversed = tokens;
        tokens_reversed.reverse();
        let mut unauth_reversed = on_unauthorized;
        unauth_reversed.reverse();
        Self {
            token_seq: std::sync::Mutex::new(tokens_reversed),
            on_unauthorized_seq: std::sync::Mutex::new(unauth_reversed),
            apply_count: std::sync::atomic::AtomicUsize::new(0),
            refresh_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Constructs a mock that yields `token` on every `apply_auth`
    /// call and `Ok(false)` on every `on_unauthorized` call.
    #[must_use]
    pub fn single(token: impl Into<String>) -> Self {
        Self::new(vec![token.into()], Vec::new())
    }

    /// Constructs a mock with an explicit sequence of bearer tokens.
    /// Returns the next token from the sequence on each `apply_auth`
    /// call; once exhausted, reuses the last token.
    #[must_use]
    pub fn with_token_sequence(tokens: Vec<String>) -> Self {
        Self::new(tokens, Vec::new())
    }

    /// Returns a new mock with the same token sequence but the given
    /// `on_unauthorized` response sequence. Consumes `self`.
    #[must_use]
    pub fn with_unauthorized_responses(self, responses: Vec<Result<bool, ProviderError>>) -> Self {
        let tokens = match self.token_seq.lock() {
            Ok(guard) => {
                let mut tokens: Vec<String> = guard.clone();
                tokens.reverse();
                tokens
            }
            Err(poison) => {
                let mut tokens: Vec<String> = poison.into_inner().clone();
                tokens.reverse();
                tokens
            }
        };
        Self::new(tokens, responses)
    }

    /// Returns the number of times `apply_auth` has been called.
    pub fn apply_call_count(&self) -> usize {
        self.apply_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the number of times `on_unauthorized` has been called.
    pub fn refresh_call_count(&self) -> usize {
        self.refresh_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl std::fmt::Debug for MockAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockAuthProvider")
            .field("apply_count", &self.apply_call_count())
            .field("refresh_count", &self.refresh_call_count())
            .finish_non_exhaustive()
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl AuthProvider for MockAuthProvider {
    async fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        self.apply_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut tokens = self
            .token_seq
            .lock()
            .map_err(|e| ProviderError::StreamError {
                reason: format!("mock auth lock poisoned: {e}"),
            })?;
        let token = if tokens.len() <= 1 {
            tokens
                .last()
                .cloned()
                .unwrap_or_else(|| "mock-token".to_string())
        } else {
            tokens.pop().unwrap_or_else(|| "mock-token".to_string())
        };
        Ok(request.header("Authorization", format!("Bearer {token}")))
    }

    async fn on_unauthorized(&self) -> Result<bool, ProviderError> {
        self.refresh_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut seq = self
            .on_unauthorized_seq
            .lock()
            .map_err(|e| ProviderError::StreamError {
                reason: format!("mock auth lock poisoned: {e}"),
            })?;
        if seq.len() <= 1 {
            match seq.last() {
                Some(Ok(v)) => Ok(*v),
                Some(Err(e)) => Err(e.clone()),
                None => Ok(false),
            }
        } else {
            seq.pop().unwrap_or(Ok(false))
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::get_unwrap
)]
mod tests {
    use super::*;

    #[test]
    fn auth_source_default_is_oauth_with_none_codex_home() {
        let source = AuthSource::default();
        match source {
            AuthSource::OAuth { codex_home } => assert!(codex_home.is_none()),
            AuthSource::ApiKey { .. } => panic!("expected OAuth default"),
        }
    }

    #[test]
    fn auth_source_oauth_default_constructor() {
        let source = AuthSource::oauth_default();
        assert!(matches!(source, AuthSource::OAuth { codex_home: None }));
    }

    #[test]
    fn login_config_default_is_browser_pkce() {
        let config = LoginConfig::default();
        assert!(config.codex_home.is_none());
        assert!(!config.device_code);
    }

    #[test]
    fn auth_provider_is_object_safe() {
        let _provider: Arc<dyn AuthProvider> =
            Arc::new(ApiKeyAuthProvider::new(SecretString::new("k")));
    }

    #[tokio::test]
    async fn api_key_auth_provider_on_unauthorized_returns_false() {
        let provider = ApiKeyAuthProvider::new(SecretString::new("test-key"));
        let result = provider.on_unauthorized().await.expect("ok");
        assert!(!result, "ApiKey auth has no refresh path");
    }

    #[tokio::test]
    async fn api_key_auth_provider_sets_bearer_header() {
        let provider = ApiKeyAuthProvider::new(SecretString::new("test-key"));
        let client = reqwest::Client::new();
        let builder = client.get("http://example.invalid");
        let built = provider.apply_auth(builder).await.expect("apply");
        let req = built.build().expect("build");
        let header = req
            .headers()
            .get("Authorization")
            .expect("auth header present");
        assert_eq!(header.to_str().unwrap(), "Bearer test-key");
    }

    #[tokio::test]
    async fn build_from_auth_source_api_key_returns_api_key_provider() {
        let source = AuthSource::ApiKey {
            key: SecretString::new("k"),
        };
        let provider = build_from_auth_source(&source).await.expect("build");
        assert!(!provider.on_unauthorized().await.expect("call"));
    }

    #[tokio::test]
    async fn mock_auth_provider_applies_token_sequence() {
        let provider =
            MockAuthProvider::with_token_sequence(vec!["stale".to_string(), "fresh".to_string()]);
        let client = reqwest::Client::new();

        let first = provider
            .apply_auth(client.get("http://example.invalid"))
            .await
            .expect("apply")
            .build()
            .expect("build");
        assert_eq!(
            first
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer stale"
        );

        let second = provider
            .apply_auth(client.get("http://example.invalid"))
            .await
            .expect("apply")
            .build()
            .expect("build");
        assert_eq!(
            second
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer fresh"
        );
        assert_eq!(provider.apply_call_count(), 2);
    }

    #[tokio::test]
    async fn mock_auth_provider_consumes_on_unauthorized_sequence() {
        let provider =
            MockAuthProvider::single("t").with_unauthorized_responses(vec![Ok(true), Ok(false)]);
        assert!(provider.on_unauthorized().await.expect("ok"));
        assert!(!provider.on_unauthorized().await.expect("ok"));
        assert_eq!(provider.refresh_call_count(), 2);
    }

    #[test]
    fn resolve_codex_home_with_override() {
        let path = std::path::PathBuf::from("/tmp/custom-codex");
        let resolved = resolve_codex_home(Some(path.clone())).expect("resolve");
        assert_eq!(resolved, path);
    }

    #[tokio::test]
    async fn login_with_device_code_returns_config_error() {
        let config = LoginConfig {
            codex_home: Some(std::path::PathBuf::from("/tmp/codex-test-nx")),
            device_code: true,
        };
        let result = login(config).await;
        match result {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("device code"));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    /// R2 acceptance: mock `AuthManager` returns token, verify Bearer
    /// header set correctly; verify `chatgpt-account-id` set when
    /// account id is present.
    #[tokio::test]
    async fn oauth_provider_sets_bearer_and_account_id_headers() {
        // `create_dummy_chatgpt_auth_for_testing` produces a CodexAuth
        // with access_token = "Access Token" and
        // account_id = Some("account_id").
        let auth = super::super::openai_oauth::CodexAuth::create_dummy_chatgpt_auth_for_testing();
        let manager = super::super::openai_oauth::AuthManager::from_static_auth(auth);
        let provider = OAuthAuthProvider::from_manager(manager);

        let client = reqwest::Client::new();
        let builder = client.get("http://example.invalid");
        let built = provider.apply_auth(builder).await.expect("apply_auth");
        let req = built.build().expect("build request");

        // Verify Bearer header carries the seeded token.
        let auth_header = req
            .headers()
            .get("Authorization")
            .expect("Authorization header present");
        assert_eq!(
            auth_header.to_str().unwrap(),
            "Bearer Access Token",
            "Bearer header must carry the token from the seeded AuthManager"
        );

        // Verify chatgpt-account-id header is set when account_id is
        // present.
        let account_header = req
            .headers()
            .get("chatgpt-account-id")
            .expect("chatgpt-account-id header present");
        assert_eq!(
            account_header.to_str().unwrap(),
            "account_id",
            "chatgpt-account-id must match the seeded account id"
        );
    }

    /// Regression test (fix campaign Track V, finding 3): a *transient*
    /// OAuth refresh failure after a 401 previously surfaced as
    /// `Ok(false)`, which the caller reported as non-retryable
    /// `AuthenticationFailed { "no refresh available" }` — the wrong
    /// class AND a false message (a refresh WAS available; it
    /// transiently failed). It must surface as a retryable error with
    /// an accurate reason.
    #[tokio::test]
    async fn transient_refresh_failure_surfaces_as_retryable_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let auth = super::super::openai_oauth::CodexAuth::create_dummy_chatgpt_auth_for_testing();
        let manager = super::super::openai_oauth::AuthManager::from_static_auth_with_token_url(
            auth,
            server.uri(),
        );
        let provider = OAuthAuthProvider::from_manager(manager);

        let err = provider
            .on_unauthorized()
            .await
            .expect_err("transient refresh failure must surface as an error, not Ok(false)");
        match &err {
            ProviderError::ConnectionFailed { reason } => {
                assert!(
                    reason.contains("transiently"),
                    "reason must say the refresh failed transiently: {reason}"
                );
                assert!(
                    !reason.contains("no refresh available"),
                    "reason must not falsely claim no refresh was available: {reason}"
                );
            }
            other => panic!("expected retryable ConnectionFailed, got {other:?}"),
        }
        assert!(
            crate::r#loop::retry::RetryPolicy::default().classifies_as_retryable(&err),
            "transient refresh failure must classify as retryable: {err:?}"
        );
    }

    /// Finding 3 counterpart: a genuinely missing refresh credential
    /// (API-key auth has no refresh path) keeps the non-retryable
    /// authentication failure.
    #[tokio::test]
    async fn missing_refresh_credential_remains_non_retryable_auth_failure() {
        let auth = super::super::openai_oauth::CodexAuth::from_api_key("vm-injected-key");
        let manager = super::super::openai_oauth::AuthManager::from_static_auth(auth);
        let provider = OAuthAuthProvider::from_manager(manager);

        let err = provider
            .on_unauthorized()
            .await
            .expect_err("a credential with no refresh path must fail permanently");
        match &err {
            ProviderError::AuthenticationFailed { reason } => {
                assert!(
                    reason.contains("no refreshable OAuth credential"),
                    "reason must name the missing refresh credential: {reason}"
                );
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }
        assert!(
            !crate::r#loop::retry::RetryPolicy::default().classifies_as_retryable(&err),
            "a permanently missing refresh credential must not be retryable"
        );
    }

    /// Complementary to the above: when `account_id` is absent, the
    /// `chatgpt-account-id` header must not be set.
    #[tokio::test]
    async fn oauth_provider_omits_account_id_when_absent() {
        let auth = super::super::openai_oauth::CodexAuth::from_api_key("test-api-key-value");
        let manager = super::super::openai_oauth::AuthManager::from_static_auth(auth);
        let provider = OAuthAuthProvider::from_manager(manager);

        let client = reqwest::Client::new();
        let builder = client.get("http://example.invalid");
        let built = provider.apply_auth(builder).await.expect("apply_auth");
        let req = built.build().expect("build request");

        // Bearer header must still be set.
        let auth_header = req
            .headers()
            .get("Authorization")
            .expect("Authorization header present");
        assert_eq!(auth_header.to_str().unwrap(), "Bearer test-api-key-value");

        // chatgpt-account-id must NOT be present.
        assert!(
            req.headers().get("chatgpt-account-id").is_none(),
            "chatgpt-account-id must not be set when account_id is absent"
        );
    }
}
