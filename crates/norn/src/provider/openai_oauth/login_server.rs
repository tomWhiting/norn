//! Browser OAuth PKCE login server.

use base64::Engine as _;
use rand::RngCore as _;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::oneshot;

use super::pkce;
use super::storage::{AuthCredentialsStoreMode, save_auth_dot_json};
use super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::{AUTHORIZE_URL, OAUTH_SCOPES, TOKEN_URL};

const LOGIN_PORTS: [u16; 2] = [1455, 1457];

/// Login server options compatible with the previous call site.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    codex_home: PathBuf,
    client_id: String,
    mode: AuthCredentialsStoreMode,
}

impl ServerOptions {
    /// Creates login-server options.
    #[must_use]
    pub fn new(
        codex_home: PathBuf,
        client_id: String,
        _chatgpt_base_url: Option<String>,
        mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            codex_home,
            client_id,
            mode,
        }
    }
}

/// Running OAuth login flow.
#[derive(Debug)]
pub struct LoginServer {
    done: oneshot::Receiver<Result<(), LoginError>>,
}

/// Login flow errors.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// Could not bind any allowed callback port.
    #[error("failed to bind local callback server on ports 1455 or 1457")]
    Bind,
    /// Browser launch failed.
    #[error("failed to open browser: {0}")]
    Browser(String),
    /// Callback server failed.
    #[error("callback server failed: {0}")]
    Server(String),
    /// OAuth callback was missing the authorization code.
    #[error("OAuth callback did not include an authorization code")]
    MissingCode,
    /// Token exchange failed.
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
    /// Auth storage failed.
    #[error("auth storage failed: {0}")]
    Storage(String),
    /// Login task ended before reporting a result.
    #[error("login task ended unexpectedly")]
    Canceled,
}

/// Starts the local callback server and opens the browser.
///
/// # Errors
///
/// Returns [`LoginError`] if no allowlisted port can be bound or browser launch
/// fails.
pub fn run_login_server(opts: ServerOptions) -> Result<LoginServer, LoginError> {
    let (server, port) = bind_allowed_port()?;
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let pkce = pkce::generate();
    let state = generate_state();
    let authorize_url =
        build_authorize_url(&opts.client_id, &redirect_uri, &pkce.challenge, &state)?;
    webbrowser::open(&authorize_url).map_err(|err| LoginError::Browser(err.to_string()))?;

    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("norn-openai-oauth-login".to_string())
        .spawn(move || {
            let result = run_callback_server(&server, &opts, &redirect_uri, &pkce.verifier, &state);
            let _ignored = tx.send(result);
        })
        .map_err(|err| LoginError::Server(err.to_string()))?;
    Ok(LoginServer { done: rx })
}

impl LoginServer {
    /// Blocks until the browser login flow completes.
    ///
    /// # Errors
    ///
    /// Returns [`LoginError`] for callback, exchange, or storage failures.
    pub async fn block_until_done(self) -> Result<(), LoginError> {
        self.done.await.map_err(|_closed| LoginError::Canceled)?
    }
}

fn bind_allowed_port() -> Result<(tiny_http::Server, u16), LoginError> {
    for port in LOGIN_PORTS {
        let address = format!("127.0.0.1:{port}");
        if let Ok(server) = tiny_http::Server::http(address) {
            return Ok((server, port));
        }
    }
    Err(LoginError::Bind)
}

fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<String, LoginError> {
    let mut url =
        url::Url::parse(AUTHORIZE_URL).map_err(|err| LoginError::Server(err.to_string()))?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", OAUTH_SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", "codex_cli_rs");
    Ok(url.into())
}

fn run_callback_server(
    server: &tiny_http::Server,
    opts: &ServerOptions,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
) -> Result<(), LoginError> {
    let request = server
        .recv_timeout(Duration::from_mins(5))
        .map_err(|err| LoginError::Server(err.to_string()))?
        .ok_or_else(|| LoginError::Server("timed out waiting for OAuth callback".to_string()))?;
    let callback_url = format!("http://localhost{}", request.url());
    let code = extract_code(&callback_url, state)?;
    let response = tiny_http::Response::from_string(
        "Login complete. You can close this browser window and return to norn.",
    );
    request
        .respond(response)
        .map_err(|err| LoginError::Server(err.to_string()))?;
    let auth = exchange_code_blocking(&opts.client_id, redirect_uri, verifier, &code)?;
    match opts.mode {
        AuthCredentialsStoreMode::File => save_auth_dot_json(&opts.codex_home, &auth)
            .map_err(|err| LoginError::Storage(err.to_string()))?,
    }
    Ok(())
}

fn extract_code(callback_url: &str, expected_state: &str) -> Result<String, LoginError> {
    let url = url::Url::parse(callback_url).map_err(|err| LoginError::Server(err.to_string()))?;
    let mut code = None;
    let mut state = None;
    let mut callback_error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => callback_error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = callback_error {
        return Err(LoginError::Server(format!(
            "OAuth callback returned error: {error}"
        )));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(LoginError::Server(
            "OAuth callback state mismatch".to_string(),
        ));
    }
    code.filter(|value| !value.is_empty())
        .ok_or(LoginError::MissingCode)
}

fn generate_state() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Deserialize)]
struct CodeExchangeResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
}

/// Exchanges the authorization code at the compiled token endpoint.
///
/// The endpoint is deliberately not configurable (no environment
/// override): an environment-redirectable token endpoint would let any
/// process that can set an env var receive the authorization-code
/// exchange — and with it the freshly minted refresh token.
fn exchange_code_blocking(
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
) -> Result<AuthDotJson, LoginError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| LoginError::TokenExchange(err.to_string()))?;
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
        .map_err(|err| LoginError::TokenExchange(err.to_string()))?;
    if !response.status().is_success() {
        return Err(LoginError::TokenExchange(format!(
            "token endpoint returned {}",
            response.status()
        )));
    }
    let token_response = response
        .json::<CodeExchangeResponse>()
        .map_err(|err| LoginError::TokenExchange(err.to_string()))?;
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
