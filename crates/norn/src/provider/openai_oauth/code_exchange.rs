//! Bounded authorization-code exchange for browser OAuth login.

use std::time::Duration;

use serde::Deserialize;

use super::endpoints::TOKEN_URL;
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
    let id_token = IdTokenInfo::from_raw_jwt(token_response.id_token);
    let account_id = token_response
        .account_id
        .or_else(|| id_token.chatgpt_account_id.clone());
    Ok(AuthDotJson::from_tokens(ChatGptTokens {
        id_token,
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        account_id,
    }))
}
