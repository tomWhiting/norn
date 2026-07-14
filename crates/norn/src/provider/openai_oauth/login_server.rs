//! Browser OAuth PKCE login server.

use base64::Engine as _;
use rand::RngCore as _;
use std::io::{Read as _, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
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
const CALLBACK_DESCRIPTOR_WEIGHT: u32 = 2;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Safe descriptor capacity could not admit the callback listener.
    #[error(transparent)]
    DescriptorAdmission(Box<crate::resource::DescriptorAdmissionError>),
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
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))?;
    let mut callback_permits = governor
        .try_acquire(CALLBACK_DESCRIPTOR_WEIGHT)
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))?;
    let listener_permit = callback_permits
        .split(1)
        .ok_or_else(|| LoginError::Server("callback listener admission split failed".to_owned()))?;
    let (listener, port) = bind_allowed_port()?;
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
            let result = run_callback_server(
                listener,
                listener_permit,
                callback_permits,
                &opts,
                &redirect_uri,
                &pkce.verifier,
                &state,
            );
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

fn bind_allowed_port() -> Result<(TcpListener, u16), LoginError> {
    for port in LOGIN_PORTS {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            listener
                .set_nonblocking(true)
                .map_err(|error| LoginError::Server(error.to_string()))?;
            return Ok((listener, port));
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
    listener: TcpListener,
    listener_permit: crate::resource::DescriptorPermit,
    accepted_permit: crate::resource::DescriptorPermit,
    opts: &ServerOptions,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
) -> Result<(), LoginError> {
    let mut callback = wait_for_callback(
        listener,
        listener_permit,
        accepted_permit,
        state,
        opts.http.callback_timeout,
    )?;
    let auth = match exchange_code_blocking(
        &opts.client_id,
        redirect_uri,
        verifier,
        &callback.code,
        opts.http.request_timeout,
    ) {
        Ok(auth) => auth,
        Err(error) => {
            callback.respond_failure();
            return Err(error);
        }
    };
    let stored = match opts.mode {
        AuthCredentialsStoreMode::File => save_auth_dot_json(&opts.codex_home, &auth)
            .map_err(|err| LoginError::Storage(err.to_string())),
    };
    if let Err(error) = stored {
        callback.respond_failure();
        return Err(error);
    }
    callback.respond_success();
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
    listener: TcpListener,
    listener_permit: crate::resource::DescriptorPermit,
    accepted_permit: crate::resource::DescriptorPermit,
    expected_state: &str,
    total_wait: Duration,
) -> Result<PendingCallback, LoginError> {
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
        let (mut stream, _peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10).min(remaining));
                continue;
            }
            Err(error) => return Err(LoginError::Server(error.to_string())),
        };
        // Darwin can propagate the listener's nonblocking state to accepted
        // sockets. The request reader relies on a bounded blocking timeout.
        stream
            .set_nonblocking(false)
            .map_err(|error| LoginError::Server(error.to_string()))?;
        let Some(target) = read_request_target(&mut stream, remaining)? else {
            continue;
        };
        let callback_url = format!("http://localhost{target}");
        match classify_callback(&callback_url, expected_state) {
            CallbackDisposition::Foreign => {
                if let Err(err) = write_response(&mut stream, 404, "Not found.") {
                    tracing::warn!(
                        error = %err,
                        "failed to answer non-callback request on the login port"
                    );
                }
            }
            CallbackDisposition::Ours(Ok(code)) => {
                drop(listener);
                drop(listener_permit);
                return Ok(PendingCallback {
                    stream,
                    code,
                    _permit: accepted_permit,
                });
            }
            CallbackDisposition::Ours(Err(flow_err)) => {
                if let Err(err) = write_response(
                    &mut stream,
                    400,
                    "Login failed. Return to norn for details.",
                ) {
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

struct PendingCallback {
    stream: TcpStream,
    code: String,
    _permit: crate::resource::DescriptorPermit,
}

impl PendingCallback {
    fn respond_success(&mut self) {
        if let Err(error) = write_response(
            &mut self.stream,
            200,
            "Login complete. You can close this browser window and return to norn.",
        ) {
            tracing::warn!(%error, "failed to send the completed OAuth login page");
        }
    }

    fn respond_failure(&mut self) {
        if let Err(error) = write_response(
            &mut self.stream,
            400,
            "Login failed. Return to norn for details.",
        ) {
            tracing::warn!(%error, "failed to send the failed OAuth login page");
        }
    }
}

fn read_request_target(
    stream: &mut TcpStream,
    remaining: Duration,
) -> Result<Option<String>, LoginError> {
    let read_timeout = remaining.min(IDLE_CONNECTION_TIMEOUT);
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|error| LoginError::Server(error.to_string()))?;
    let mut header = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while header.len() < MAX_REQUEST_HEADER_BYTES {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read) => {
                header.extend_from_slice(&chunk[..read]);
                if header.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(LoginError::Server(error.to_string())),
        }
    }
    if !header.windows(4).any(|window| window == b"\r\n\r\n") {
        return Ok(None);
    }
    let first_line = header
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let line = std::str::from_utf8(first_line)
        .unwrap_or_default()
        .trim_end();
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(target), Some(version), None) if version.starts_with("HTTP/1.") => {
            Ok(Some(target.to_owned()))
        }
        _ => Ok(None),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)
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
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| LoginError::TokenExchange(error.to_string()))?;
    let _permit = governor
        .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
        .map_err(|error| LoginError::TokenExchange(error.to_string()))?;
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

    fn test_server() -> Result<
        (
            TcpListener,
            u16,
            crate::resource::DescriptorPermit,
            crate::resource::DescriptorPermit,
        ),
        Box<dyn std::error::Error>,
    > {
        let governor = crate::resource::DescriptorGovernor::global()?;
        let mut permits = governor.try_acquire(2)?;
        let listener_permit = permits
            .split(1)
            .ok_or_else(|| std::io::Error::other("listener permit split failed"))?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok((listener, port, listener_permit, permits))
    }

    /// Regression test (final-state hardening, T1 item 7): the callback
    /// server previously treated the FIRST request on the port as the
    /// OAuth callback, so a browser favicon probe or a stray request
    /// aborted the login. It must now answer foreign requests with 404
    /// and keep listening until the state-matching callback arrives.
    #[test]
    fn stray_requests_get_404_and_login_still_completes() -> Result<(), Box<dyn std::error::Error>>
    {
        let (listener, port, listener_permit, accepted_permit) = test_server()?;

        let waiter = std::thread::spawn(move || {
            wait_for_callback(
                listener,
                listener_permit,
                accepted_permit,
                "expected-state",
                Duration::from_secs(10),
            )
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

        let genuine = std::thread::spawn(move || {
            raw_get(port, "/auth/callback?code=real-code&state=expected-state")
        });
        let mut callback = waiter.join().map_err(|error| {
            std::io::Error::other(format!("waiter thread panicked: {error:?}"))
        })??;
        assert_eq!(callback.code, "real-code");
        assert!(
            !genuine.is_finished(),
            "the browser response must wait for exchange and storage"
        );
        callback.respond_success();
        let genuine = genuine.join().map_err(|error| {
            std::io::Error::other(format!("genuine request thread panicked: {error:?}"))
        })?;
        assert!(
            genuine.starts_with("HTTP/1.1 200"),
            "the genuine callback must get the success page: {genuine}"
        );
        Ok(())
    }

    #[test]
    fn matching_error_callback_fails_the_flow_with_a_400_page()
    -> Result<(), Box<dyn std::error::Error>> {
        let (listener, port, listener_permit, accepted_permit) = test_server()?;

        let waiter = std::thread::spawn(move || {
            wait_for_callback(
                listener,
                listener_permit,
                accepted_permit,
                "expected-state",
                Duration::from_secs(10),
            )
        });

        let response = raw_get(
            port,
            "/auth/callback?error=access_denied&state=expected-state",
        );
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "a failed genuine callback must get the failure page: {response}"
        );

        let result = waiter
            .join()
            .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
        let Err(LoginError::Server(message)) = result else {
            return Err(std::io::Error::other("expected callback Server error").into());
        };
        assert!(message.contains("access_denied"), "message: {message}");
        Ok(())
    }

    #[test]
    fn accepted_connection_waits_for_delayed_request_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (listener, port, listener_permit, accepted_permit) = test_server()?;

        let waiter = std::thread::spawn(move || {
            wait_for_callback(
                listener,
                listener_permit,
                accepted_permit,
                "expected-state",
                Duration::from_secs(2),
            )
        });

        let mut socket = std::net::TcpStream::connect(("127.0.0.1", port))?;
        std::thread::sleep(Duration::from_millis(100));
        socket.write_all(
            b"GET /auth/callback?error=access_denied&state=expected-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )?;
        let mut response = String::new();
        socket.read_to_string(&mut response)?;

        assert!(
            response.starts_with("HTTP/1.1 400"),
            "a delayed genuine callback must get the failure page: {response}"
        );
        let result = waiter
            .join()
            .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
        let Err(LoginError::Server(message)) = result else {
            return Err(std::io::Error::other("expected callback Server error").into());
        };
        assert!(message.contains("access_denied"), "message: {message}");
        Ok(())
    }

    /// The overall wait is a total budget: stray requests must not extend
    /// it, and with no genuine callback the wait ends in the timeout
    /// error.
    #[test]
    fn wait_times_out_when_no_matching_callback_arrives() -> Result<(), Box<dyn std::error::Error>>
    {
        let (listener, port, listener_permit, accepted_permit) = test_server()?;

        // Generous budget so the stray request below reliably lands while
        // the waiter is still listening, even on a loaded machine.
        let waiter = std::thread::spawn(move || {
            wait_for_callback(
                listener,
                listener_permit,
                accepted_permit,
                "expected-state",
                Duration::from_secs(2),
            )
        });

        // A stray request part-way through must not reset the deadline.
        let stray = raw_get(port, "/not-the-callback");
        assert!(stray.starts_with("HTTP/1.1 404"));

        let result = waiter
            .join()
            .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
        let Err(LoginError::Server(message)) = result else {
            return Err(std::io::Error::other("expected callback timeout error").into());
        };
        assert!(
            message.contains("timed out waiting for OAuth callback"),
            "message: {message}"
        );
        Ok(())
    }
}
