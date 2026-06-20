//! Cached OAuth credential manager with proactive refresh.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use tokio::sync::Mutex;

use super::TOKEN_URL;
use super::jwt::parse_jwt_expiration;
use super::refresh::{RefreshError, refresh_auth};
use super::storage::{AuthCredentialsStoreMode, load_auth_dot_json, save_auth_dot_json};
use super::types::CodexAuth;
use crate::provider::startup_trace;

/// Refresh result classification expected by auth.rs.
#[derive(Debug, thiserror::Error)]
pub enum RefreshTokenError {
    /// Credential is dead; user must log in again.
    #[error("{0}")]
    Permanent(String),
    /// Network/server issue; caller may retry later.
    #[error("{0}")]
    Transient(String),
}

/// Small credential manager compatible with norn's OAuth auth-provider use.
#[derive(Debug)]
pub struct AuthManager {
    codex_home: Option<PathBuf>,
    auth: Mutex<Option<CodexAuth>>,
    /// Single-flight gate held across the token-authority exchange.
    ///
    /// `auth` cannot serve as the gate: holding it across the network
    /// call would block every `apply_auth` reader for the duration. A
    /// dedicated mutex serializes refreshes only.
    refresh_gate: Mutex<()>,
    /// Monotonic count of *completed* refreshes. A caller that observed
    /// epoch `N` before waiting on the gate and sees `> N` afterwards
    /// knows another caller already refreshed the credential it wanted
    /// refreshed, and must not spend the (possibly rotated) refresh
    /// token again.
    refresh_epoch: AtomicU64,
    /// Token-endpoint URL the refresh token is exchanged against.
    ///
    /// Fixed to the compiled authority ([`TOKEN_URL`]) by every
    /// production constructor — the endpoint is intrinsic to the
    /// credential's authority, and making it configurable (e.g. via an
    /// environment variable) would create a refresh-token exfiltration
    /// vector. Tests inject a mock server URL through the test-gated
    /// constructors instead.
    token_url: String,
}

impl AuthManager {
    /// Creates a shared manager and loads cached credentials from disk.
    #[must_use]
    pub async fn shared(
        codex_home: PathBuf,
        _enable_codex_api_key_env: bool,
        mode: AuthCredentialsStoreMode,
        _chatgpt_base_url: Option<String>,
    ) -> Arc<Self> {
        Self::shared_with_token_url(codex_home, mode, TOKEN_URL.to_string()).await
    }

    /// [`Self::shared`] with an injected token-endpoint URL.
    ///
    /// Test-gated so production builds physically cannot redirect the
    /// refresh exchange away from the compiled authority.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub async fn shared_for_tests(
        codex_home: PathBuf,
        mode: AuthCredentialsStoreMode,
        token_url: String,
    ) -> Arc<Self> {
        Self::shared_with_token_url(codex_home, mode, token_url).await
    }

    async fn shared_with_token_url(
        codex_home: PathBuf,
        mode: AuthCredentialsStoreMode,
        token_url: String,
    ) -> Arc<Self> {
        let shared_started = startup_trace::start("oauth_auth_manager_shared_with_token_url_start");
        tokio::task::yield_now().await;
        startup_trace::elapsed("oauth_auth_manager_initial_yield_done", shared_started);

        let load_started = startup_trace::start("oauth_auth_manager_load_auth_start");
        let auth = load_auth_dot_json(&codex_home, mode)
            .ok()
            .flatten()
            .map(|auth| CodexAuth::ChatGpt(Box::new(auth)));
        startup_trace::auth_manager_load_done(load_started, auth.is_some());
        startup_trace::elapsed(
            "oauth_auth_manager_shared_with_token_url_done",
            shared_started,
        );
        Arc::new(Self {
            codex_home: Some(codex_home),
            auth: Mutex::new(auth),
            refresh_gate: Mutex::new(()),
            refresh_epoch: AtomicU64::new(0),
            token_url,
        })
    }

    /// Creates a shared manager seeded with an in-memory credential.
    ///
    /// Production constructor for embedders that inject credentials
    /// directly (for example, VM-provisioned tokens handed to the agent
    /// at startup) instead of loading them from `$CODEX_HOME/auth.json`.
    /// The manager has no backing store: refreshed tokens are kept in
    /// memory only and nothing is persisted to disk.
    #[must_use]
    pub fn from_static_auth(auth: CodexAuth) -> Arc<Self> {
        Self::static_auth_with_token_url(auth, TOKEN_URL.to_string())
    }

    /// [`Self::from_static_auth`] with an injected token-endpoint URL.
    ///
    /// Test-gated so production builds physically cannot redirect the
    /// refresh exchange away from the compiled authority.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn from_static_auth_with_token_url(auth: CodexAuth, token_url: String) -> Arc<Self> {
        Self::static_auth_with_token_url(auth, token_url)
    }

    fn static_auth_with_token_url(auth: CodexAuth, token_url: String) -> Arc<Self> {
        Arc::new(Self {
            codex_home: None,
            auth: Mutex::new(Some(auth)),
            refresh_gate: Mutex::new(()),
            refresh_epoch: AtomicU64::new(0),
            token_url,
        })
    }

    /// Returns cached credentials, proactively refreshing expired `ChatGPT`
    /// access tokens before returning them.
    #[must_use]
    pub async fn auth(&self) -> Option<CodexAuth> {
        if self.should_refresh().await {
            match self.refresh_token_from_authority().await {
                Err(RefreshTokenError::Permanent(_)) => return None,
                Ok(()) | Err(RefreshTokenError::Transient(_)) => {}
            }
        }
        self.auth.lock().await.clone()
    }

    /// Forces a refresh through the token authority.
    ///
    /// Refreshes are single-flight: concurrent callers wait for the
    /// in-flight exchange and adopt its result instead of re-spending
    /// the same refresh token (which, under token rotation, would
    /// invalidate the winner's credential and force a spurious logout).
    /// A failed refresh releases the gate without bumping the epoch, so
    /// subsequent attempts proceed normally.
    ///
    /// # Errors
    ///
    /// Returns permanent/transient classification for the failed refresh.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        let observed_epoch = self.refresh_epoch.load(Ordering::Acquire);
        let _gate = self.refresh_gate.lock().await;
        if self.refresh_epoch.load(Ordering::Acquire) != observed_epoch {
            // Another caller completed a refresh while we waited on the
            // gate; its result is already cached in `self.auth`.
            return Ok(());
        }

        let current = self.auth.lock().await.clone();
        let Some(CodexAuth::ChatGpt(auth)) = current else {
            return Err(RefreshTokenError::Permanent(
                "no refreshable OAuth credential".to_string(),
            ));
        };
        let refreshed = refresh_auth(&auth, &self.token_url)
            .await
            .map_err(map_refresh_error)?;

        // Cache the new credential before attempting persistence: a
        // persist failure must never discard a successful refresh.
        *self.auth.lock().await = Some(CodexAuth::ChatGpt(Box::new(refreshed.clone())));
        self.refresh_epoch.fetch_add(1, Ordering::Release);

        if let Some(codex_home) = self.codex_home.as_ref()
            && let Err(err) = save_auth_dot_json(codex_home, &refreshed)
        {
            tracing::warn!(
                codex_home = %codex_home.display(),
                error = %err,
                "refreshed OAuth credential could not be persisted; keeping it in memory"
            );
        }
        Ok(())
    }

    async fn should_refresh(&self) -> bool {
        let auth = self.auth.lock().await.clone();
        let Some(CodexAuth::ChatGpt(auth_dot_json)) = auth else {
            return false;
        };
        let Some(tokens) = auth_dot_json.tokens.as_ref() else {
            return false;
        };
        match parse_jwt_expiration(&tokens.access_token) {
            Ok(Some(expiry)) => expiry <= Utc::now(),
            Ok(None) | Err(_) => auth_dot_json
                .last_refresh
                .is_none_or(|last| Utc::now() >= last + Duration::days(8)),
        }
    }
}

fn map_refresh_error(error: RefreshError) -> RefreshTokenError {
    match error {
        RefreshError::Transient(message) => RefreshTokenError::Transient(message),
        RefreshError::Permanent(message) => RefreshTokenError::Permanent(message),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::storage::{AuthCredentialsStoreMode, save_auth_dot_json};
    use super::super::types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};
    use super::*;

    /// Builds a manager whose refresh exchange targets the given mock
    /// server, injected through the test-gated constructor — no
    /// environment mutation, so tests run in parallel safely.
    fn seeded_manager(server: &MockServer) -> Arc<AuthManager> {
        AuthManager::from_static_auth_with_token_url(
            CodexAuth::ChatGpt(Box::new(seed_auth("seed-refresh"))),
            server.uri(),
        )
    }

    fn seed_auth(refresh_token: &str) -> AuthDotJson {
        AuthDotJson::from_tokens(ChatGptTokens {
            id_token: IdTokenInfo::from_raw_jwt("seed-id-token".to_string()),
            access_token: "seed-access-token".to_string(),
            refresh_token: refresh_token.to_string(),
            account_id: Some("seed-account".to_string()),
        })
    }

    fn refresh_response_body() -> serde_json::Value {
        serde_json::json!({
            "id_token": "new-id-token",
            "access_token": "new-access-token",
            "refresh_token": "rotated-refresh-token",
            "account_id": "seed-account",
        })
    }

    async fn access_token(manager: &AuthManager) -> String {
        let auth = manager.auth().await.expect("credential present");
        auth.get_token().expect("token present").to_string()
    }

    /// Regression test for REVIEW.md H6: concurrent refreshes of the
    /// same credential must collapse into a single token-authority
    /// exchange. Pre-fix, both callers spent the same refresh token —
    /// under rotation the loser's exchange returns 401, classifies
    /// Permanent, and forces a spurious logout.
    #[tokio::test]
    async fn concurrent_refreshes_collapse_into_single_exchange() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(150))
                    .set_body_json(refresh_response_body()),
            )
            .mount(&server)
            .await;

        let manager = seeded_manager(&server);

        let (first, second) = tokio::join!(
            manager.refresh_token_from_authority(),
            manager.refresh_token_from_authority(),
        );
        assert!(first.is_ok(), "first refresh failed: {first:?}");
        assert!(second.is_ok(), "second refresh failed: {second:?}");

        let requests = server
            .received_requests()
            .await
            .expect("request recording enabled");
        assert_eq!(
            requests.len(),
            1,
            "concurrent refreshes must share one exchange, saw {}",
            requests.len()
        );
        assert_eq!(access_token(&manager).await, "new-access-token");
    }

    /// A failed refresh must not poison the gate: the next attempt
    /// proceeds and can succeed.
    #[tokio::test]
    async fn failed_refresh_does_not_poison_subsequent_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
            .with_priority(2)
            .mount(&server)
            .await;

        let manager = seeded_manager(&server);

        let first = manager.refresh_token_from_authority().await;
        assert!(
            matches!(first, Err(RefreshTokenError::Transient(_))),
            "expected transient failure from HTTP 500, got {first:?}"
        );

        let second = manager.refresh_token_from_authority().await;
        assert!(second.is_ok(), "retry after failure failed: {second:?}");
        assert_eq!(access_token(&manager).await, "new-access-token");
    }

    /// Sequential refresh calls each perform their own exchange — the
    /// epoch check only suppresses callers that overlapped an in-flight
    /// refresh, not later forced refreshes (e.g. distinct 401s).
    #[tokio::test]
    async fn sequential_refreshes_each_exchange() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
            .mount(&server)
            .await;

        let manager = seeded_manager(&server);

        manager
            .refresh_token_from_authority()
            .await
            .expect("first refresh");
        manager
            .refresh_token_from_authority()
            .await
            .expect("second refresh");

        let requests = server
            .received_requests()
            .await
            .expect("request recording enabled");
        assert_eq!(requests.len(), 2, "sequential refreshes must not dedupe");
    }

    /// Regression test for REVIEW.md medium `openai_oauth/manager.rs:87`:
    /// a successful refresh must be kept in memory even when persisting
    /// `auth.json` fails. Pre-fix the persist error failed the whole
    /// refresh and the new credential was lost.
    #[cfg(unix)]
    #[tokio::test]
    async fn persist_failure_keeps_refreshed_credential_in_memory() {
        use std::os::unix::fs::PermissionsExt as _;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        std::fs::create_dir(&codex_home).expect("create codex home");
        save_auth_dot_json(&codex_home, &seed_auth("seed-refresh")).expect("seed auth.json");

        let manager = AuthManager::shared_for_tests(
            codex_home.clone(),
            AuthCredentialsStoreMode::File,
            server.uri(),
        )
        .await;

        // Make the directory unwritable so persistence fails.
        std::fs::set_permissions(&codex_home, std::fs::Permissions::from_mode(0o500))
            .expect("make codex home read-only");

        let result = manager.refresh_token_from_authority().await;

        // Restore permissions so the tempdir can be cleaned up.
        std::fs::set_permissions(&codex_home, std::fs::Permissions::from_mode(0o700))
            .expect("restore codex home permissions");

        assert!(
            result.is_ok(),
            "refresh must succeed despite persist failure, got {result:?}"
        );
        assert_eq!(
            access_token(&manager).await,
            "new-access-token",
            "refreshed credential must be served from memory"
        );
    }

    /// `from_static_auth` managers have no backing store and never
    /// touch the filesystem on refresh.
    #[tokio::test]
    async fn static_auth_refreshes_in_memory_only() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
            .mount(&server)
            .await;

        let manager = seeded_manager(&server);
        manager
            .refresh_token_from_authority()
            .await
            .expect("refresh");
        assert_eq!(access_token(&manager).await, "new-access-token");
    }

    /// `from_static_auth` with an API-key credential serves it as-is
    /// and reports refresh as permanently unavailable.
    #[tokio::test]
    async fn static_auth_api_key_has_no_refresh_path() {
        let manager = AuthManager::from_static_auth(CodexAuth::from_api_key("vm-injected-key"));
        assert_eq!(access_token(&manager).await, "vm-injected-key");

        let result = manager.refresh_token_from_authority().await;
        assert!(
            matches!(result, Err(RefreshTokenError::Permanent(_))),
            "API-key credentials are not refreshable, got {result:?}"
        );
    }

    const _: fn() = || {
        fn check<T: Send + Sync>() {}
        check::<AuthManager>();
        check::<Arc<AuthManager>>();
    };
}
