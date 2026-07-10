//! OAuth refresh-token exchange.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::CLIENT_ID;
use super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};

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
}

/// Refreshes an auth document using its refresh token.
///
/// `token_url` is the token-endpoint URL the refresh token is sent to.
/// It is threaded explicitly from the caller ([`AuthManager`]) rather
/// than read from the environment: an environment-redirectable token
/// endpoint would let any process that can set an env var exfiltrate
/// refresh tokens. Production callers pass the compiled authority
/// constant; tests inject their mock server URL through the API.
///
/// `client` is the manager's shared HTTP client, built once with the
/// configured [`OAuthHttpOptions::request_timeout`] and reused across
/// every refresh instead of constructing a throwaway client per call.
///
/// [`AuthManager`]: super::manager::AuthManager
/// [`OAuthHttpOptions::request_timeout`]: super::options::OAuthHttpOptions::request_timeout
///
/// # Errors
///
/// Returns [`RefreshError::Permanent`] for dead credentials and
/// [`RefreshError::Transient`] for retryable failures.
pub async fn refresh_auth(
    auth: &AuthDotJson,
    token_url: &str,
    client: &reqwest::Client,
) -> Result<AuthDotJson, RefreshError> {
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| RefreshError::Permanent("missing OAuth tokens".to_string()))?;
    let response = client
        .post(token_url)
        .json(&RefreshRequest {
            client_id: CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: &tokens.refresh_token,
        })
        .send()
        .await
        .map_err(|err| RefreshError::Transient(err.to_string()))?;

    if response.status() == StatusCode::UNAUTHORIZED {
        let body = response.json::<ErrorResponse>().await.map_err(|error| {
            tracing::debug!(
                error = %error,
                "token authority returned a malformed 401 response"
            );
            RefreshError::Permanent(
                "refresh token rejected with a malformed authority response".to_owned(),
            )
        })?;
        return Err(classify_401(&body));
    }
    if !response.status().is_success() {
        let status = response.status();
        return Err(RefreshError::Transient(format!(
            "token endpoint returned HTTP {status}"
        )));
    }

    let refreshed = response.json::<RefreshResponse>().await.map_err(|error| {
        tracing::debug!(
            error = %error,
            "token authority returned a malformed success response"
        );
        RefreshError::Transient("token endpoint returned a malformed success response".to_owned())
    })?;
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

fn classify_401(body: &ErrorResponse) -> RefreshError {
    let code = body.error.code.as_deref().unwrap_or("other");
    match code {
        "refresh_token_expired" | "expired" => {
            RefreshError::Permanent("refresh token expired".to_owned())
        }
        "refresh_token_exhausted" | "exhausted" => {
            RefreshError::Permanent("refresh token exhausted".to_owned())
        }
        "refresh_token_revoked" | "revoked" => {
            RefreshError::Permanent("refresh token revoked".to_owned())
        }
        _ => RefreshError::Permanent("refresh token rejected".to_owned()),
    }
}

#[cfg(test)]
mod security_tests {
    use std::time::Duration;

    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn auth() -> AuthDotJson {
        AuthDotJson::from_tokens(ChatGptTokens {
            id_token: IdTokenInfo::from_raw_jwt("id-token".to_owned()),
            access_token: "access-token-secret".to_owned(),
            refresh_token: "refresh-token-secret".to_owned(),
            account_id: Some("account-secret".to_owned()),
        })
    }

    #[tokio::test]
    async fn authority_error_bodies_are_not_propagated() -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "code": "refresh_token_expired",
                    "message": "echoed-refresh-token-secret"
                }
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("echoed-account-secret"))
            .with_priority(2)
            .mount(&server)
            .await;
        let client = crate::provider::http_client::build_bounded_client(Duration::from_secs(5))?;

        let first = refresh_auth(&auth(), &server.uri(), &client).await;
        let Err(first_error) = first else {
            return Err(std::io::Error::other("401 refresh unexpectedly succeeded").into());
        };
        let second = refresh_auth(&auth(), &server.uri(), &client).await;
        let Err(second_error) = second else {
            return Err(std::io::Error::other("500 refresh unexpectedly succeeded").into());
        };
        let rendered = format!("{first_error} {second_error}");

        assert!(!rendered.contains("echoed-refresh-token-secret"));
        assert!(!rendered.contains("echoed-account-secret"));
        assert!(rendered.contains("refresh token expired"));
        assert!(rendered.contains("500"));
        Ok(())
    }
}
