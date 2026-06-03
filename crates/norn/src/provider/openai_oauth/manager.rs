//! Cached OAuth credential manager with proactive refresh.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration, Utc};
use tokio::sync::Mutex;

use super::jwt::parse_jwt_expiration;
use super::refresh::{RefreshError, refresh_auth};
use super::storage::{AuthCredentialsStoreMode, load_auth_dot_json, save_auth_dot_json};
use super::types::CodexAuth;

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
        tokio::task::yield_now().await;
        let auth = load_auth_dot_json(&codex_home, mode)
            .ok()
            .flatten()
            .map(|auth| CodexAuth::ChatGpt(Box::new(auth)));
        Arc::new(Self {
            codex_home: Some(codex_home),
            auth: Mutex::new(auth),
        })
    }

    /// Creates a manager seeded with known credentials for tests.
    #[must_use]
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Arc::new(Self {
            codex_home: None,
            auth: Mutex::new(Some(auth)),
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
    /// # Errors
    ///
    /// Returns permanent/transient classification for the failed refresh.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        let current = self.auth.lock().await.clone();
        let Some(CodexAuth::ChatGpt(auth)) = current else {
            return Err(RefreshTokenError::Permanent(
                "no refreshable OAuth credential".to_string(),
            ));
        };
        let refreshed = refresh_auth(&auth).await.map_err(map_refresh_error)?;
        if let Some(codex_home) = self.codex_home.as_ref() {
            save_auth_dot_json(codex_home, &refreshed)
                .map_err(|err| RefreshTokenError::Transient(err.to_string()))?;
        }
        *self.auth.lock().await = Some(CodexAuth::ChatGpt(Box::new(refreshed)));
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
