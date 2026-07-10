//! Browser OAuth PKCE login server.

use base64::Engine as _;
use rand::RngCore as _;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::oneshot;

use super::options::OAuthHttpOptions;
use super::pkce;
use super::storage::{AuthCredentialsStoreMode, save_auth_dot_json};
use super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::{AUTHORIZE_URL, OAUTH_SCOPES, TOKEN_URL};

const LOGIN_PORTS: [u16; 2] = [1455, 1457];

/// Path the OAuth authority redirects the browser to on this host.
const CALLBACK_PATH: &str = "/auth/callback";

/// Login server options.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    codex_home: PathBuf,
    client_id: String,
    mode: AuthCredentialsStoreMode,
    http: OAuthHttpOptions,
}

impl ServerOptions {
    /// Creates login-server options.
    ///
    /// `http` supplies the total callback wait and the authorization-code
    /// exchange deadline (see [`OAuthHttpOptions`]).
    #[must_use]
    pub fn new(
        codex_home: PathBuf,
        client_id: String,
        mode: AuthCredentialsStoreMode,
        http: OAuthHttpOptions,
    ) -> Self {
        Self {
            codex_home,
            client_id,
            mode,
            http,
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
/// Returns `LoginError` if no allowlisted port can be bound or browser launch
/// fails.
pub fn run_login_server(opts: ServerOptions) -> Result<LoginServer, LoginError> {
    let (server, port) = bind_allowed_port()?;
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
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
    /// Returns `LoginError` for callback, exchange, or storage failures.
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
    let code = wait_for_callback(server, state, opts.http.callback_timeout)?;
    let auth = exchange_code_blocking(
        &opts.client_id,
        redirect_uri,
        verifier,
        &code,
        opts.http.request_timeout,
    )?;
    match opts.mode {
        AuthCredentialsStoreMode::File => save_auth_dot_json(&opts.codex_home, &auth)
            .map_err(|err| LoginError::Storage(err.to_string()))?,
    }
    Ok(())
}

/// Serves the callback port until the OAuth redirect for *this* login
/// attempt arrives or `total_wait` elapses.
///
/// The port is a plain local HTTP listener, so browsers and other local
/// software routinely probe it (`/favicon.ico`, health checks, stray
/// tabs). Any request that is not a `/auth/callback` hit carrying this
/// attempt's `state` is answered `404` and the server keeps listening —
/// a single stray request must never consume the one-shot wait and abort
/// the login. Only the state-matching callback is processed: a provider
/// `error` parameter fails the flow, a missing `code` fails the flow,
/// and a `code` completes it.
fn wait_for_callback(
    server: &tiny_http::Server,
    expected_state: &str,
    total_wait: Duration,
) -> Result<String, LoginError> {
    // Computed from the elapsed time rather than `Instant + Duration`,
    // which panics on overflow for absurd configured waits.
    let started = std::time::Instant::now();
    loop {
        let remaining = total_wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(LoginError::Server(
                "timed out waiting for OAuth callback".to_string(),
            ));
        }
        let request = server
            .recv_timeout(remaining)
            .map_err(|err| LoginError::Server(err.to_string()))?
            .ok_or_else(|| {
                LoginError::Server("timed out waiting for OAuth callback".to_string())
            })?;

        let callback_url = format!("http://localhost{}", request.url());
        match classify_callback(&callback_url, expected_state) {
            CallbackDisposition::Foreign => {
                let response = tiny_http::Response::from_string("Not found.").with_status_code(404);
                if let Err(err) = request.respond(response) {
                    tracing::warn!(
                        error = %err,
                        "failed to answer non-callback request on the login port"
                    );
                }
            }
            CallbackDisposition::Ours(Ok(code)) => {
                let response = tiny_http::Response::from_string(
                    "Login complete. You can close this browser window and return to norn.",
                );
                request
                    .respond(response)
                    .map_err(|err| LoginError::Server(err.to_string()))?;
                return Ok(code);
            }
            CallbackDisposition::Ours(Err(flow_err)) => {
                let response =
                    tiny_http::Response::from_string("Login failed. Return to norn for details.")
                        .with_status_code(400);
                if let Err(err) = request.respond(response) {
                    tracing::warn!(
                        error = %err,
                        "failed to answer failed OAuth callback on the login port"
                    );
                }
                return Err(flow_err);
            }
        }
    }
}

/// How an inbound request on the login port relates to this login attempt.
enum CallbackDisposition {
    /// Not the OAuth redirect for this attempt (wrong path, unparseable
    /// URL, or non-matching `state`) — answer 404 and keep waiting. A
    /// mismatched `state` is treated as foreign rather than fatal so a
    /// forged or stale request cannot abort a legitimate in-flight login.
    Foreign,
    /// The state-matching `/auth/callback` redirect: either the
    /// authorization code or the flow-level failure it reported.
    Ours(Result<String, LoginError>),
}

fn classify_callback(callback_url: &str, expected_state: &str) -> CallbackDisposition {
    let Ok(url) = url::Url::parse(callback_url) else {
        return CallbackDisposition::Foreign;
    };
    if url.path() != CALLBACK_PATH {
        return CallbackDisposition::Foreign;
    }
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
    if state.as_deref() != Some(expected_state) {
        return CallbackDisposition::Foreign;
    }
    if let Some(error) = callback_error {
        return CallbackDisposition::Ours(Err(LoginError::Server(format!(
            "OAuth callback returned error: {error}"
        ))));
    }
    CallbackDisposition::Ours(
        code.filter(|value| !value.is_empty())
            .ok_or(LoginError::MissingCode),
    )
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
/// exchange — and with it the freshly minted refresh token. The
/// whole-request `timeout` comes from
/// [`OAuthHttpOptions::request_timeout`].
fn exchange_code_blocking(
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
    timeout: Duration,
) -> Result<AuthDotJson, LoginError> {
    let client = crate::provider::http_client::build_blocking_bounded_client(timeout)
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    // -- classify_callback --------------------------------------------------

    #[test]
    fn classify_matching_callback_yields_code() {
        let disposition =
            classify_callback("http://localhost/auth/callback?code=abc&state=s1", "s1");
        match disposition {
            CallbackDisposition::Ours(Ok(code)) => assert_eq!(code, "abc"),
            _ => panic!("expected Ours(Ok) for a state-matching callback"),
        }
    }

    #[test]
    fn classify_wrong_path_is_foreign() {
        assert!(matches!(
            classify_callback("http://localhost/favicon.ico", "s1"),
            CallbackDisposition::Foreign
        ));
    }

    #[test]
    fn classify_state_mismatch_is_foreign() {
        // A forged/stale request must not be able to abort the login.
        assert!(matches!(
            classify_callback("http://localhost/auth/callback?code=evil&state=other", "s1"),
            CallbackDisposition::Foreign
        ));
    }

    #[test]
    fn classify_missing_state_is_foreign() {
        assert!(matches!(
            classify_callback("http://localhost/auth/callback?code=abc", "s1"),
            CallbackDisposition::Foreign
        ));
    }

    #[test]
    fn classify_matching_error_callback_fails_flow() {
        match classify_callback(
            "http://localhost/auth/callback?error=access_denied&state=s1",
            "s1",
        ) {
            CallbackDisposition::Ours(Err(LoginError::Server(message))) => {
                assert!(message.contains("access_denied"), "message: {message}");
            }
            _ => panic!("expected Ours(Err) for a state-matching error callback"),
        }
    }

    #[test]
    fn classify_matching_callback_without_code_is_missing_code() {
        assert!(matches!(
            classify_callback("http://localhost/auth/callback?state=s1", "s1"),
            CallbackDisposition::Ours(Err(LoginError::MissingCode))
        ));
        assert!(matches!(
            classify_callback("http://localhost/auth/callback?code=&state=s1", "s1"),
            CallbackDisposition::Ours(Err(LoginError::MissingCode))
        ));
    }

    // -- wait_for_callback --------------------------------------------------

    /// Issues one HTTP request over a raw TCP socket and returns the raw
    /// response text.
    fn raw_get(port: u16, path: &str) -> String {
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        socket
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .expect("write request");
        let mut response = String::new();
        socket.read_to_string(&mut response).expect("read response");
        response
    }

    fn test_server() -> (tiny_http::Server, u16) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind test port");
        let port = server
            .server_addr()
            .to_ip()
            .expect("IP listen address")
            .port();
        (server, port)
    }

    /// Regression test (final-state hardening, T1 item 7): the callback
    /// server previously treated the FIRST request on the port as the
    /// OAuth callback, so a browser favicon probe or a stray request
    /// aborted the login. It must now answer foreign requests with 404
    /// and keep listening until the state-matching callback arrives.
    #[test]
    fn stray_requests_get_404_and_login_still_completes() {
        let (server, port) = test_server();

        let waiter = std::thread::spawn(move || {
            wait_for_callback(&server, "expected-state", Duration::from_secs(10))
        });

        let favicon = raw_get(port, "/favicon.ico");
        assert!(
            favicon.starts_with("HTTP/1.1 404"),
            "foreign path must get 404: {favicon}"
        );

        let forged = raw_get(port, "/auth/callback?code=evil&state=forged");
        assert!(
            forged.starts_with("HTTP/1.1 404"),
            "state-mismatched callback must get 404: {forged}"
        );

        let genuine = raw_get(port, "/auth/callback?code=real-code&state=expected-state");
        assert!(
            genuine.starts_with("HTTP/1.1 200"),
            "the genuine callback must get the success page: {genuine}"
        );

        let code = waiter
            .join()
            .expect("waiter thread")
            .expect("callback must succeed after stray requests");
        assert_eq!(code, "real-code");
    }

    #[test]
    fn matching_error_callback_fails_the_flow_with_a_400_page() {
        let (server, port) = test_server();

        let waiter = std::thread::spawn(move || {
            wait_for_callback(&server, "expected-state", Duration::from_secs(10))
        });

        let response = raw_get(
            port,
            "/auth/callback?error=access_denied&state=expected-state",
        );
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "a failed genuine callback must get the failure page: {response}"
        );

        let result = waiter.join().expect("waiter thread");
        match result {
            Err(LoginError::Server(message)) => {
                assert!(message.contains("access_denied"), "message: {message}");
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    /// The overall wait is a total budget: stray requests must not extend
    /// it, and with no genuine callback the wait ends in the timeout
    /// error.
    #[test]
    fn wait_times_out_when_no_matching_callback_arrives() {
        let (server, port) = test_server();

        // Generous budget so the stray request below reliably lands while
        // the waiter is still listening, even on a loaded machine.
        let waiter = std::thread::spawn(move || {
            wait_for_callback(&server, "expected-state", Duration::from_secs(2))
        });

        // A stray request part-way through must not reset the deadline.
        let stray = raw_get(port, "/not-the-callback");
        assert!(stray.starts_with("HTTP/1.1 404"));

        let result = waiter.join().expect("waiter thread");
        match result {
            Err(LoginError::Server(message)) => {
                assert!(
                    message.contains("timed out waiting for OAuth callback"),
                    "message: {message}"
                );
            }
            other => panic!("expected timeout Server error, got {other:?}"),
        }
    }
}
