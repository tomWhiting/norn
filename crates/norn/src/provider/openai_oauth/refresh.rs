//! OAuth refresh-token exchange.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::CLIENT_ID;
use super::credential_validation::{
    CredentialField, CredentialValueError, validate_credential_value,
};
use super::jwt::JwtError;
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
    /// The authority accepted the refresh request, but the returned lineage
    /// could not be safely accepted. Retrying the old token is unsafe.
    #[error("indeterminate rotated OAuth lineage: {0}")]
    Indeterminate(String),
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
/// Returns [`RefreshError::Permanent`] for dead credentials,
/// [`RefreshError::Transient`] for failures known to precede acceptance, and
/// [`RefreshError::Indeterminate`] when a successful authority response cannot
/// be accepted safely and the old refresh-token lineage must not be retried.
pub async fn refresh_auth(
    auth: &AuthDotJson,
    token_url: &str,
    client: &reqwest::Client,
) -> Result<AuthDotJson, RefreshError> {
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| RefreshError::Permanent("missing OAuth tokens".to_string()))?;
    validate_refresh_preconditions(tokens)?;
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| RefreshError::Transient(error.to_string()))?;
    let _permit = governor
        .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
        .map_err(|error| RefreshError::Transient(error.to_string()))?;
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
        RefreshError::Indeterminate(
            "token endpoint returned a malformed success response".to_owned(),
        )
    })?;
    let new_tokens = refreshed_tokens(refreshed, tokens)?;
    let mut updated = auth.clone();
    updated.tokens = Some(new_tokens);
    updated.last_refresh = Some(chrono::Utc::now());
    Ok(updated)
}

fn refreshed_tokens(
    refreshed: RefreshResponse,
    current: &ChatGptTokens,
) -> Result<ChatGptTokens, RefreshError> {
    let id_token = IdTokenInfo::from_raw_jwt(refreshed.id_token)
        .map_err(|error| map_post_success_id_token_error(&error))?;
    let issued_account_id = id_token
        .reconcile_account_id(refreshed.account_id)
        .map_err(|error| map_post_success_id_token_error(&error))?;
    let prior_account_id = current
        .id_token
        .reconcile_account_id(current.account_id.clone())
        .map_err(|error| map_post_success_id_token_error(&error))?;
    if issued_account_id != prior_account_id {
        return Err(RefreshError::Indeterminate(
            "token endpoint returned conflicting account identity metadata".to_owned(),
        ));
    }
    if id_token.chatgpt_user_id.as_deref() != current.id_token.chatgpt_user_id.as_deref() {
        return Err(RefreshError::Indeterminate(
            "token endpoint returned conflicting user identity metadata".to_owned(),
        ));
    }
    let refresh_token = match refreshed.refresh_token {
        Some(refresh_token) => refresh_token,
        // The current Codex auth manager treats refresh responses as partial
        // updates and retains the prior refresh token when this field is absent.
        None => current.refresh_token.clone(),
    };

    let mut tokens = ChatGptTokens::validated(
        id_token,
        refreshed.access_token,
        refresh_token,
        issued_account_id,
    )
    .map_err(map_post_success_credential_error)?;
    tokens
        .additional_fields
        .clone_from(&current.additional_fields);
    Ok(tokens)
}

fn validate_refresh_preconditions(tokens: &ChatGptTokens) -> Result<(), RefreshError> {
    if validate_credential_value(CredentialField::RefreshToken, &tokens.refresh_token).is_err() {
        return Err(RefreshError::Permanent(
            "missing usable OAuth refresh token".to_owned(),
        ));
    }
    tokens
        .id_token
        .reconcile_account_id(tokens.account_id.clone())
        .map_err(|error| map_current_identity_error(&error))?;
    Ok(())
}

fn map_current_identity_error(error: &JwtError) -> RefreshError {
    let reason = match error {
        JwtError::MissingAccountId => "stored OAuth credential has no account identity metadata",
        JwtError::InvalidAccountId => {
            "stored OAuth credential has invalid account identity metadata"
        }
        JwtError::ConflictingAccountIds => {
            "stored OAuth credential has conflicting account identity metadata"
        }
        JwtError::MissingClaims
        | JwtError::InvalidStructure
        | JwtError::Base64(_)
        | JwtError::Json(_)
        | JwtError::InvalidExpiration
        | JwtError::InvalidUserId
        | JwtError::ConflictingUserIds => "stored OAuth credential has malformed identity metadata",
    };
    RefreshError::Permanent(reason.to_owned())
}

fn map_post_success_id_token_error(error: &JwtError) -> RefreshError {
    let reason = match error {
        JwtError::ConflictingAccountIds => {
            "token endpoint returned conflicting account identity metadata"
        }
        JwtError::MissingAccountId => "token endpoint returned no account identity metadata",
        JwtError::InvalidAccountId => "token endpoint returned invalid account identity metadata",
        JwtError::MissingClaims
        | JwtError::InvalidStructure
        | JwtError::Base64(_)
        | JwtError::Json(_)
        | JwtError::InvalidExpiration
        | JwtError::InvalidUserId
        | JwtError::ConflictingUserIds => "token endpoint returned malformed id-token claims",
    };
    RefreshError::Indeterminate(reason.to_owned())
}

fn map_post_success_credential_error(error: CredentialValueError) -> RefreshError {
    let reason = match error.field() {
        CredentialField::AccessToken => "token endpoint returned an unusable access token",
        CredentialField::RefreshToken => "token endpoint returned an unusable refresh token",
        CredentialField::AccountId => "token endpoint returned an unusable account identifier",
    };
    RefreshError::Indeterminate(reason.to_owned())
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

    use base64::Engine as _;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn fixture_jwt(claims: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{claims}.")
    }

    fn current_tokens(account_id: &str) -> ChatGptTokens {
        ChatGptTokens {
            id_token: IdTokenInfo {
                raw_jwt: fixture_jwt(&serde_json::json!({
                    "https://api.openai.com/auth": {
                        "chatgpt_account_id": account_id
                    }
                })),
                email: None,
                chatgpt_plan_type: None,
                chatgpt_user_id: None,
                chatgpt_account_id: Some(account_id.to_owned()),
            },
            access_token: "not-an-access-token".to_owned(),
            refresh_token: "not-a-refresh-token".to_owned(),
            account_id: Some(account_id.to_owned()),
            additional_fields: std::collections::BTreeMap::new(),
        }
    }

    fn auth() -> AuthDotJson {
        let mut tokens = current_tokens("account-secret");
        tokens.access_token = "access-token-secret".to_owned();
        tokens.refresh_token = "refresh-token-secret".to_owned();
        AuthDotJson::from_tokens(tokens)
    }

    fn response(id_token: String, account_id: Option<&str>) -> RefreshResponse {
        RefreshResponse {
            id_token,
            access_token: "not-a-refreshed-access-token".to_owned(),
            refresh_token: Some("not-a-rotated-refresh-token".to_owned()),
            account_id: account_id.map(str::to_owned),
        }
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

    #[test]
    fn namespaced_refresh_claim_preserves_account_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let current = current_tokens("account-fixture");
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture"
            }
        }));

        let refreshed = refreshed_tokens(response(id_token, None), &current)?;

        assert_eq!(refreshed.account_id.as_deref(), Some("account-fixture"));
        Ok(())
    }

    #[test]
    fn omitted_refresh_token_retains_the_current_lineage() -> Result<(), Box<dyn std::error::Error>>
    {
        let current = current_tokens("account-fixture");
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture"
            }
        }));
        let mut partial = response(id_token, None);
        partial.refresh_token = None;

        let refreshed = refreshed_tokens(partial, &current)?;

        assert_eq!(refreshed.refresh_token, current.refresh_token);
        Ok(())
    }

    #[test]
    fn response_and_claim_account_conflict_is_rejected_without_disclosure()
    -> Result<(), std::io::Error> {
        let current = current_tokens("claim-account-secret");
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "claim-account-secret"
            }
        }));

        let result = refreshed_tokens(
            response(id_token, Some("response-account-secret")),
            &current,
        );
        assert_identity_conflict(result)
    }

    #[test]
    fn refreshed_account_cannot_replace_prior_identity() -> Result<(), std::io::Error> {
        let current = current_tokens("prior-account-secret");
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "refreshed-account-secret"
            }
        }));

        let result = refreshed_tokens(
            response(id_token, Some("refreshed-account-secret")),
            &current,
        );
        assert_identity_conflict(result)
    }

    #[test]
    fn malformed_refresh_id_token_is_rejected_without_disclosure() -> Result<(), std::io::Error> {
        let current = current_tokens("account-fixture");
        let result = refreshed_tokens(
            response(
                "malformed-id-token-secret".to_owned(),
                Some("account-fixture"),
            ),
            &current,
        );
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "malformed refresh id token unexpectedly succeeded",
            ));
        };
        let rendered = error.to_string();

        assert!(matches!(error, RefreshError::Indeterminate(_)));
        assert!(rendered.contains("malformed id-token claims"));
        assert!(!rendered.contains("malformed-id-token-secret"));
        Ok(())
    }

    fn assert_identity_conflict(
        result: Result<ChatGptTokens, RefreshError>,
    ) -> Result<(), std::io::Error> {
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "conflicting account identities unexpectedly succeeded",
            ));
        };
        let rendered = error.to_string();

        assert!(matches!(error, RefreshError::Indeterminate(_)));
        assert!(rendered.contains("conflicting account identity metadata"));
        assert!(!rendered.contains("claim-account-secret"));
        assert!(!rendered.contains("response-account-secret"));
        assert!(!rendered.contains("prior-account-secret"));
        assert!(!rendered.contains("refreshed-account-secret"));
        Ok(())
    }
}

#[cfg(test)]
#[path = "refresh_validation_tests.rs"]
mod validation_tests;
