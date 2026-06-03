//! OAuth refresh-token exchange.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::{CLIENT_ID, TOKEN_URL};

/// Errors from token refresh.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// Network or protocol issue that may succeed later.
    #[error("transient refresh failure: {0}")]
    Transient(String),
    /// Credential is permanently invalid and the user must log in again.
    #[error("permanent refresh failure: {0}")]
    Permanent(String),
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: String,
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

/// Refreshes an auth document using its refresh token.
///
/// # Errors
///
/// Returns [`RefreshError::Permanent`] for dead credentials and
/// [`RefreshError::Transient`] for retryable failures.
pub async fn refresh_auth(auth: &AuthDotJson) -> Result<AuthDotJson, RefreshError> {
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| RefreshError::Permanent("missing OAuth tokens".to_string()))?;
    let url =
        std::env::var("CODEX_REFRESH_TOKEN_URL_OVERRIDE").unwrap_or_else(|_| TOKEN_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| RefreshError::Transient(err.to_string()))?;
    let response = client
        .post(url)
        .json(&RefreshRequest {
            client_id: CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: &tokens.refresh_token,
        })
        .send()
        .await
        .map_err(|err| RefreshError::Transient(err.to_string()))?;

    if response.status() == StatusCode::UNAUTHORIZED {
        let body = response
            .json::<ErrorResponse>()
            .await
            .map_err(|err| RefreshError::Permanent(format!("401 Unauthorized ({err})")))?;
        return Err(classify_401(body));
    }
    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|err| format!("<failed to read error body: {err}>"));
        return Err(RefreshError::Transient(format!(
            "token endpoint returned {status}: {text}"
        )));
    }

    let refreshed = response
        .json::<RefreshResponse>()
        .await
        .map_err(|err| RefreshError::Transient(err.to_string()))?;
    let new_tokens = ChatGptTokens {
        id_token: IdTokenInfo::from_raw_jwt(refreshed.id_token),
        access_token: refreshed.access_token,
        refresh_token: refreshed
            .refresh_token
            .unwrap_or_else(|| tokens.refresh_token.clone()),
        account_id: refreshed.account_id.or_else(|| tokens.account_id.clone()),
    };
    let mut updated = AuthDotJson::from_tokens(new_tokens);
    updated.agent_identity.clone_from(&auth.agent_identity);
    Ok(updated)
}

fn classify_401(body: ErrorResponse) -> RefreshError {
    let code = body.error.code.as_deref().unwrap_or("other");
    let message = body.error.message.unwrap_or_else(|| code.to_string());
    match code {
        "refresh_token_expired" | "expired" => {
            RefreshError::Permanent(format!("refresh token expired: {message}"))
        }
        "refresh_token_exhausted" | "exhausted" => {
            RefreshError::Permanent(format!("refresh token exhausted: {message}"))
        }
        "refresh_token_revoked" | "revoked" => {
            RefreshError::Permanent(format!("refresh token revoked: {message}"))
        }
        _ => RefreshError::Permanent(format!("refresh token rejected: {message}")),
    }
}
