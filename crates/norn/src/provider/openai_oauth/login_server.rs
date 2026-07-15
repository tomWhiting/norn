//! Browser OAuth PKCE login server.

use base64::Engine as _;
use rand::RngCore as _;
use std::net::TcpListener;
#[cfg(test)]
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use super::auth_root::NornAuthRoot;
use super::credential_transaction::{
    CredentialRevision, CredentialTransaction, CredentialTransactionError,
};
use super::endpoints::{AUTHORIZE_URL, OAUTH_SCOPES};
use super::options::OAuthHttpOptions;
use super::pkce;
use super::storage::AuthCredentialsStoreMode;
use super::types::AuthDotJson;

#[path = "login_callback.rs"]
mod callback_protocol;
#[path = "login_callback_worker.rs"]
mod callback_worker;

use callback_protocol::CALLBACK_PATH;
#[cfg(test)]
use callback_protocol::{CallbackDisposition, classify_callback, configure_accepted_stream};
use callback_worker::{CallbackServerArgs, run_callback_worker};
#[cfg(test)]
use callback_worker::{complete_prepared_callback, wait_for_callback};

const LOGIN_PORTS: [u16; 2] = [1455, 1457];
const CALLBACK_DESCRIPTOR_WEIGHT: u32 = 2;
const CALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const LOGIN_WAITING: u8 = 0;
const LOGIN_CANCELED: u8 = 1;
const LOGIN_CALLBACK_CLAIMED: u8 = 2;

/// Login server options.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    auth_root: NornAuthRoot,
    client_id: String,
    mode: AuthCredentialsStoreMode,
    http: OAuthHttpOptions,
}

impl ServerOptions {
    /// Creates login-server options from an already validated auth root.
    ///
    /// `http` supplies the total callback wait and the authorization-code
    /// exchange deadline (see [`OAuthHttpOptions`]).
    #[must_use]
    pub fn new(
        auth_root: NornAuthRoot,
        client_id: String,
        mode: AuthCredentialsStoreMode,
        http: OAuthHttpOptions,
    ) -> Self {
        Self {
            auth_root,
            client_id,
            mode,
            http,
        }
    }
}

/// Running OAuth login flow.
#[derive(Debug)]
pub struct LoginServer {
    prepared: oneshot::Receiver<Result<AuthDotJson, LoginError>>,
    acknowledgement: Option<oneshot::Sender<CommitAcknowledgement>>,
    finished: oneshot::Receiver<()>,
    auth_root: NornAuthRoot,
    expected_revision: Option<CredentialRevision>,
    mode: AuthCredentialsStoreMode,
    credential_lock_timeout: Duration,
    lifecycle: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitAcknowledgement {
    Committed,
    Canceled,
}

/// Typed local-storage failure encountered while committing a browser login.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginStorageFailureKind {
    /// Another credential writer changed the observed credential lineage.
    Conflict,
    /// Credential bytes may be visible but their durability was not confirmed.
    Undurable,
    /// Locking or private filesystem coordination failed.
    Coordination,
}

impl std::fmt::Display for LoginStorageFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Conflict => "conflict",
            Self::Undurable => "undurable",
            Self::Coordination => "coordination",
        })
    }
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
    Browser(&'static str),
    /// Callback server failed.
    #[error("callback server failed: {0}")]
    Server(String),
    /// OAuth callback was missing the authorization code.
    #[error("OAuth callback did not include an authorization code")]
    MissingCode,
    /// OAuth authorization ended without returning a code.
    #[error("OAuth authorization failed before returning a code")]
    AuthorizationFailed,
    /// Token exchange failed.
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
    /// Auth storage failed with a structural lifecycle classification.
    #[error("auth storage failed ({kind}): {reason}")]
    Storage {
        /// Stable lifecycle category used at the public provider boundary.
        kind: LoginStorageFailureKind,
        /// Non-disclosing local failure detail.
        reason: String,
    },
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
    let ServerOptions {
        auth_root,
        client_id,
        mode,
        http,
    } = opts;
    let expected_revision = CredentialTransaction::inspect(&auth_root)
        .map_err(map_credential_transaction_error)?
        .revision;
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
    let authorize_url = build_authorize_url(&client_id, &redirect_uri, &pkce.challenge, &state)?;
    let browser_launch =
        super::browser::open_authorization_url(&authorize_url).map_err(map_browser_launch_error)?;

    let (prepared_tx, prepared_rx) = oneshot::channel();
    let (acknowledgement_tx, acknowledgement_rx) = oneshot::channel();
    let (finished_tx, finished_rx) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_WAITING));
    let worker_lifecycle = Arc::clone(&lifecycle);
    let credential_lock_timeout = http.credential_lock_timeout;
    std::thread::Builder::new()
        .name("norn-openai-oauth-login".to_string())
        .spawn(move || {
            run_callback_worker(
                CallbackServerArgs {
                    listener,
                    listener_permit,
                    accepted_permit: callback_permits,
                    client_id: &client_id,
                    http,
                    redirect_uri: &redirect_uri,
                    verifier: &pkce.verifier,
                    state: &state,
                    browser_launch,
                    lifecycle: &worker_lifecycle,
                },
                prepared_tx,
                acknowledgement_rx,
            );
            if finished_tx.send(()).is_err() {
                tracing::trace!("OAuth login owner dropped before worker completion");
            }
        })
        .map_err(|err| LoginError::Server(err.to_string()))?;
    Ok(LoginServer {
        prepared: prepared_rx,
        acknowledgement: Some(acknowledgement_tx),
        finished: finished_rx,
        auth_root,
        expected_revision,
        mode,
        credential_lock_timeout,
        lifecycle,
    })
}

fn map_browser_launch_error(error: super::browser::BrowserLaunchError) -> LoginError {
    match error {
        super::browser::BrowserLaunchError::DescriptorAdmission(error) => {
            LoginError::DescriptorAdmission(Box::new(error))
        }
        super::browser::BrowserLaunchError::Structural(reason) => LoginError::Browser(reason),
    }
}

fn map_credential_transaction_error(error: CredentialTransactionError) -> LoginError {
    let reason = error.to_string();
    let kind = match error {
        CredentialTransactionError::DescriptorAdmission(error) => {
            return LoginError::DescriptorAdmission(error);
        }
        CredentialTransactionError::Conflict
        | CredentialTransactionError::VerificationConflict
        | CredentialTransactionError::RecoveryIncomplete(_) => LoginStorageFailureKind::Conflict,
        CredentialTransactionError::PublishedButUndurable { .. }
        | CredentialTransactionError::DeletedButUndurable(_) => LoginStorageFailureKind::Undurable,
        CredentialTransactionError::OpenRoot(_)
        | CredentialTransactionError::OpenLock(_)
        | CredentialTransactionError::LockTimeout { .. }
        | CredentialTransactionError::Lock(_)
        | CredentialTransactionError::Storage(_) => LoginStorageFailureKind::Coordination,
    };
    LoginError::Storage { kind, reason }
}

impl LoginServer {
    /// Blocks until the browser login flow completes.
    ///
    /// This future owns the credential commit. Dropping it before commit sends
    /// an explicit cancellation to the callback worker, which closes the
    /// browser request without writing credentials. A successful browser page
    /// is released only after the durable save returns successfully.
    ///
    /// # Errors
    ///
    /// Returns `LoginError` for callback, exchange, or storage failures.
    pub async fn block_until_done(mut self) -> Result<(), LoginError> {
        let prepared = match (&mut self.prepared).await {
            Ok(prepared) => prepared,
            Err(closed) => {
                tracing::trace!(%closed, "OAuth login worker closed before preparing credentials");
                return Err(LoginError::Canceled);
            }
        };
        let auth = match prepared {
            Ok(auth) => auth,
            Err(error) => {
                self.wait_for_worker().await?;
                return Err(error);
            }
        };

        let root = self.auth_root.clone();
        let deadline = self.credential_lock_timeout;
        let transaction =
            tokio::task::spawn_blocking(move || CredentialTransaction::acquire(&root, deadline))
                .await
                .map_err(|error| LoginError::Storage {
                    kind: LoginStorageFailureKind::Coordination,
                    reason: format!("credential transaction task failed: {error}"),
                })
                .and_then(|result| result.map_err(map_credential_transaction_error));

        // Keep the durable write and acknowledgement in one synchronous poll.
        // Dropping this future before this point sends Canceled; once this poll
        // starts, the credential is durable before the worker can render success.
        let stored = match (self.mode, transaction) {
            (AuthCredentialsStoreMode::File, Ok(transaction)) => transaction
                .save_if_revision(self.expected_revision.as_ref(), &auth)
                .map(drop)
                .map_err(map_credential_transaction_error),
            (AuthCredentialsStoreMode::File, Err(error)) => Err(error),
        };
        let acknowledgement = if stored.is_ok() {
            CommitAcknowledgement::Committed
        } else {
            CommitAcknowledgement::Canceled
        };
        let acknowledged = self.acknowledge_worker(acknowledgement);
        let finished = self.wait_for_worker().await;

        stored?;
        acknowledged?;
        finished
    }

    fn acknowledge_worker(
        &mut self,
        acknowledgement: CommitAcknowledgement,
    ) -> Result<(), LoginError> {
        let sender = self.acknowledgement.take().ok_or_else(|| {
            LoginError::Server("login acknowledgement channel is unavailable".to_owned())
        })?;
        sender.send(acknowledgement).map_err(|returned| {
            tracing::trace!(
                ?returned,
                "OAuth login worker closed before acknowledgement"
            );
            LoginError::Canceled
        })
    }

    async fn wait_for_worker(&mut self) -> Result<(), LoginError> {
        match (&mut self.finished).await {
            Ok(()) => Ok(()),
            Err(closed) => {
                tracing::trace!(%closed, "OAuth login worker closed before completion");
                Err(LoginError::Canceled)
            }
        }
    }
}

impl Drop for LoginServer {
    fn drop(&mut self) {
        cancel_waiting_login(&self.lifecycle);
        if let Some(sender) = self.acknowledgement.take()
            && sender.send(CommitAcknowledgement::Canceled).is_err()
        {
            tracing::trace!("OAuth login worker closed before cancellation acknowledgement");
        }
    }
}

fn cancel_waiting_login(lifecycle: &AtomicU8) {
    match lifecycle.compare_exchange(
        LOGIN_WAITING,
        LOGIN_CANCELED,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) | Err(_) => {}
    }
}

fn claim_callback(lifecycle: &AtomicU8) -> Result<(), LoginError> {
    match lifecycle.compare_exchange(
        LOGIN_WAITING,
        LOGIN_CALLBACK_CLAIMED,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(LOGIN_WAITING) => Ok(()),
        Err(LOGIN_CANCELED) => Err(LoginError::Canceled),
        Ok(_) | Err(_) => Err(LoginError::Server(
            "OAuth callback lifecycle changed unexpectedly".to_owned(),
        )),
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
) -> Result<url::Url, LoginError> {
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
    Ok(url)
}

fn generate_state() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
#[path = "login_server_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "login_server_commit_tests.rs"]
mod commit_tests;
