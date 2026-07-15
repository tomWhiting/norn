//! Bounded authorization-code exchange for browser OAuth login.

use std::time::Duration;

use serde::Deserialize;

use super::credential_validation::{CredentialField, CredentialValueError};
use super::endpoints::TOKEN_URL;
use super::jwt::JwtError;
use super::login_server::LoginError;
use super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};

#[derive(Deserialize)]
struct CodeExchangeResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
}

/// Exchanges the authorization code at the compiled token endpoint.
///
/// The endpoint is deliberately not configurable: an environment-redirectable
/// endpoint could receive the freshly minted refresh token. The whole-request
/// `timeout` comes from [`super::OAuthHttpOptions::request_timeout`].
pub(super) fn exchange_code_blocking(
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
    timeout: Duration,
) -> Result<AuthDotJson, LoginError> {
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))?;
    let _permit = governor
        .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))?;
    let client = crate::provider::http_client::build_blocking_bounded_client(timeout)
        .map_err(|error| LoginError::TokenExchange(error.to_string()))?;
    let response = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|error| LoginError::TokenExchange(error.to_string()))?;
    if !response.status().is_success() {
        return Err(LoginError::TokenExchange(format!(
            "token endpoint returned {}",
            response.status()
        )));
    }
    let token_response = response
        .json::<CodeExchangeResponse>()
        .map_err(|error| LoginError::TokenExchange(error.to_string()))?;
    auth_from_response(token_response)
}

fn auth_from_response(token_response: CodeExchangeResponse) -> Result<AuthDotJson, LoginError> {
    let id_token = IdTokenInfo::from_raw_jwt(token_response.id_token)
        .map_err(|error| map_id_token_error(&error))?;
    let account_id = id_token
        .reconcile_account_id(token_response.account_id)
        .map_err(|error| map_id_token_error(&error))?;
    let tokens = ChatGptTokens::validated(
        id_token,
        token_response.access_token,
        token_response.refresh_token,
        account_id,
    )
    .map_err(map_credential_error)?;
    Ok(AuthDotJson::from_tokens(tokens))
}

fn map_id_token_error(error: &JwtError) -> LoginError {
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
    LoginError::TokenExchange(reason.to_owned())
}

fn map_credential_error(error: CredentialValueError) -> LoginError {
    let reason = match error.field() {
        CredentialField::AccessToken => "token endpoint returned an unusable access token",
        CredentialField::RefreshToken => "token endpoint returned an unusable refresh token",
        CredentialField::AccountId => "token endpoint returned an unusable account identifier",
    };
    LoginError::TokenExchange(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::*;

    fn fixture_jwt(claims: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{claims}.")
    }

    fn response(id_token: String, account_id: Option<&str>) -> CodeExchangeResponse {
        CodeExchangeResponse {
            id_token,
            access_token: "not-an-access-token".to_owned(),
            refresh_token: "not-a-refresh-token".to_owned(),
            account_id: account_id.map(str::to_owned),
        }
    }

    #[test]
    fn namespaced_claim_becomes_persisted_account_id() -> Result<(), Box<dyn std::error::Error>> {
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture"
            }
        }));

        let auth = auth_from_response(response(id_token, None))?;
        let Some(tokens) = auth.tokens else {
            return Err(std::io::Error::other("token exchange omitted credentials").into());
        };

        assert_eq!(tokens.account_id.as_deref(), Some("account-fixture"));
        Ok(())
    }

    #[test]
    fn response_and_claim_account_conflict_is_rejected_without_disclosure()
    -> Result<(), std::io::Error> {
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "claim-account-secret"
            }
        }));

        let result = auth_from_response(response(id_token, Some("response-account-secret")));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "conflicting account identities unexpectedly succeeded",
            ));
        };
        let rendered = error.to_string();

        assert!(rendered.contains("conflicting account identity metadata"));
        assert!(!rendered.contains("claim-account-secret"));
        assert!(!rendered.contains("response-account-secret"));
        Ok(())
    }

    #[test]
    fn malformed_id_token_is_rejected_without_disclosure() -> Result<(), std::io::Error> {
        let result = auth_from_response(response(
            "malformed-id-token-secret".to_owned(),
            Some("account-fixture"),
        ));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "malformed id token unexpectedly succeeded",
            ));
        };
        let rendered = error.to_string();

        assert!(rendered.contains("malformed id-token claims"));
        assert!(!rendered.contains("malformed-id-token-secret"));
        Ok(())
    }

    #[test]
    fn missing_account_id_is_rejected_as_identity_failure() -> Result<(), std::io::Error> {
        let id_token = fixture_jwt(&serde_json::json!({"scope": "openid"}));
        let result = auth_from_response(response(id_token, None));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "accountless token exchange unexpectedly succeeded",
            ));
        };

        assert!(error.to_string().contains("no account identity metadata"));
        Ok(())
    }

    #[test]
    fn unsafe_response_fields_are_rejected_without_disclosure() -> Result<(), std::io::Error> {
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture"
            }
        }));
        for (access_token, refresh_token, account_id, expected) in [
            ("", "refresh", None, "unusable access token"),
            ("access\rsecret", "refresh", None, "unusable access token"),
            ("access", "", None, "unusable refresh token"),
            ("access", " refresh", None, "unusable refresh token"),
            ("access", "refresh\0secret", None, "unusable refresh token"),
            (
                "access",
                "refresh",
                Some(""),
                "invalid account identity metadata",
            ),
            (
                "access",
                "refresh",
                Some("account\nsecret"),
                "invalid account identity metadata",
            ),
        ] {
            let result = auth_from_response(CodeExchangeResponse {
                id_token: id_token.clone(),
                access_token: access_token.to_owned(),
                refresh_token: refresh_token.to_owned(),
                account_id: account_id.map(str::to_owned),
            });
            let Err(error) = result else {
                return Err(std::io::Error::other(
                    "unsafe token response unexpectedly succeeded",
                ));
            };
            let rendered = error.to_string();

            assert!(rendered.contains(expected));
            assert!(!rendered.contains("secret"));
        }
        Ok(())
    }
}
