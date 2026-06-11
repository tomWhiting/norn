//! OAuth token revocation and logout.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::storage::{AuthCredentialsStoreMode, delete_auth_dot_json, load_auth_dot_json};
use super::{CLIENT_ID, REVOKE_URL};

/// Errors from logout/revoke.
#[derive(Debug, thiserror::Error)]
pub enum LogoutError {
    /// Storage I/O failed.
    #[error("auth storage failed: {0}")]
    Storage(#[from] std::io::Error),
    /// HTTP client construction or request failed.
    #[error("token revoke failed: {0}")]
    Revoke(String),
}

#[derive(Serialize)]
struct RevokeRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    client_id: &'a str,
}

/// Best-effort revokes the stored refresh token and deletes `auth.json` on
/// success.
///
/// # Errors
///
/// Returns an error if revocation fails or the auth file cannot be deleted.
pub async fn logout_with_revoke(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
) -> Result<(), LogoutError> {
    let Some(auth) = load_auth_dot_json(codex_home, mode)? else {
        return Ok(());
    };
    if let Some(tokens) = auth.tokens.as_ref() {
        revoke_refresh_token(&tokens.refresh_token).await?;
    }
    delete_auth_dot_json(codex_home)?;
    Ok(())
}

/// Revokes the refresh token at the compiled revoke endpoint.
///
/// The endpoint is deliberately not configurable (no environment
/// override): the request body carries the live refresh token, so an
/// environment-redirectable endpoint would be an exfiltration vector.
async fn revoke_refresh_token(refresh_token: &str) -> Result<(), LogoutError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| LogoutError::Revoke(err.to_string()))?;
    let response = client
        .post(REVOKE_URL)
        .json(&RevokeRequest {
            token: refresh_token,
            token_type_hint: "refresh_token",
            client_id: CLIENT_ID,
        })
        .send()
        .await
        .map_err(|err| LogoutError::Revoke(err.to_string()))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(LogoutError::Revoke(format!(
            "revoke endpoint returned {}",
            response.status()
        )))
    }
}
