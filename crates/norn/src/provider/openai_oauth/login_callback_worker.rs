//! Callback worker lifecycle for browser OAuth login.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use super::super::options::OAuthHttpOptions;
use super::super::types::AuthDotJson;
use super::callback_protocol::{
    CallbackDisposition, classify_callback, configure_accepted_stream, read_request_target,
    write_response,
};
use super::{
    CALLBACK_POLL_INTERVAL, CommitAcknowledgement, LOGIN_CANCELED, LoginError, claim_callback,
    map_browser_launch_error,
};

const LOGIN_FAILURE_BODY: &str = "Login failed. Return to norn for details.";

pub(super) fn run_callback_worker(
    args: CallbackServerArgs<'_>,
    prepared: oneshot::Sender<Result<AuthDotJson, LoginError>>,
    acknowledgement: oneshot::Receiver<CommitAcknowledgement>,
) {
    match prepare_callback(args) {
        Ok((auth, callback)) => {
            complete_prepared_callback(auth, callback, prepared, acknowledgement);
        }
        Err(error) => {
            drop(prepared.send(Err(error)));
        }
    }
}

fn prepare_callback(
    args: CallbackServerArgs<'_>,
) -> Result<(AuthDotJson, PendingCallback), LoginError> {
    let CallbackServerArgs {
        listener,
        listener_permit,
        accepted_permit,
        client_id,
        http,
        redirect_uri,
        verifier,
        state,
        browser_launch,
        lifecycle,
    } = args;
    let mut callback = wait_for_callback(
        listener,
        listener_permit,
        accepted_permit,
        state,
        http.callback_timeout,
        Some(browser_launch),
        lifecycle,
    )?;
    let auth = match super::super::code_exchange::exchange_code_blocking(
        client_id,
        redirect_uri,
        verifier,
        &callback.code,
        http.request_timeout,
    ) {
        Ok(auth) => auth,
        Err(error) => {
            callback.respond_failure();
            return Err(error);
        }
    };
    Ok((auth, callback))
}

pub(super) fn complete_prepared_callback(
    auth: AuthDotJson,
    mut callback: PendingCallback,
    prepared: oneshot::Sender<Result<AuthDotJson, LoginError>>,
    acknowledgement: oneshot::Receiver<CommitAcknowledgement>,
) {
    drop(prepared.send(Ok(auth)));
    match acknowledgement.blocking_recv() {
        Ok(CommitAcknowledgement::Committed) => callback.respond_success(),
        Ok(CommitAcknowledgement::Canceled) | Err(_) => callback.respond_failure(),
    }
}

pub(super) struct CallbackServerArgs<'a> {
    pub(super) listener: TcpListener,
    pub(super) listener_permit: crate::resource::DescriptorPermit,
    pub(super) accepted_permit: crate::resource::DescriptorPermit,
    pub(super) client_id: &'a str,
    pub(super) http: OAuthHttpOptions,
    pub(super) redirect_uri: &'a str,
    pub(super) verifier: &'a str,
    pub(super) state: &'a str,
    pub(super) browser_launch: super::super::browser::BrowserLaunch,
    pub(super) lifecycle: &'a AtomicU8,
}

/// Serves the callback port until the OAuth redirect for *this* login
/// attempt arrives or `total_wait` elapses.
///
/// The port is a plain local HTTP listener, so browsers and other local
/// software routinely probe it (`/favicon.ico`, health checks, stray
/// tabs). Any request that is not a `/auth/callback` hit carrying this
/// attempt's `state` is answered `404` and the server keeps listening -
/// a single stray request must never consume the one-shot wait and abort
/// the login. Only the state-matching callback is processed: a provider
/// `error` parameter fails the flow, a missing `code` fails the flow,
/// and a `code` completes it.
pub(super) fn wait_for_callback(
    listener: TcpListener,
    listener_permit: crate::resource::DescriptorPermit,
    accepted_permit: crate::resource::DescriptorPermit,
    expected_state: &str,
    total_wait: Duration,
    mut browser_launch: Option<super::super::browser::BrowserLaunch>,
    lifecycle: &AtomicU8,
) -> Result<PendingCallback, LoginError> {
    // Computed from the elapsed time rather than `Instant + Duration`,
    // which panics on overflow for absurd configured waits.
    let started = std::time::Instant::now();
    loop {
        if lifecycle.load(Ordering::Acquire) == LOGIN_CANCELED {
            return Err(LoginError::Canceled);
        }
        if let Some(launch) = browser_launch.as_mut() {
            launch.check().map_err(map_browser_launch_error)?;
        }
        let remaining = total_wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(LoginError::Server(
                "timed out waiting for OAuth callback".to_string(),
            ));
        }
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(CALLBACK_POLL_INTERVAL.min(remaining));
                continue;
            }
            Err(error) => return Err(LoginError::Server(error.to_string())),
        };
        configure_accepted_stream(&stream)?;
        let Some(target) = read_accepted_request_target(&mut stream, remaining, lifecycle)? else {
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
                claim_accepted_callback(lifecycle, &mut stream)?;
                drop(listener);
                drop(listener_permit);
                return Ok(PendingCallback {
                    stream,
                    code,
                    permit: accepted_permit,
                });
            }
            CallbackDisposition::Ours(Err(flow_err)) => {
                claim_accepted_callback(lifecycle, &mut stream)?;
                write_failure_page(&mut stream);
                return Err(flow_err);
            }
        }
    }
}

pub(super) fn read_accepted_request_target(
    stream: &mut TcpStream,
    remaining: Duration,
    lifecycle: &AtomicU8,
) -> Result<Option<String>, LoginError> {
    let result = read_request_target(stream, remaining, lifecycle);
    respond_to_cancellation(result, stream)
}

pub(super) fn claim_accepted_callback(
    lifecycle: &AtomicU8,
    stream: &mut TcpStream,
) -> Result<(), LoginError> {
    let result = claim_callback(lifecycle);
    respond_to_cancellation(result, stream)
}

fn respond_to_cancellation<T>(
    result: Result<T, LoginError>,
    stream: &mut TcpStream,
) -> Result<T, LoginError> {
    if matches!(&result, Err(LoginError::Canceled)) {
        write_failure_page(stream);
    }
    result
}

fn write_failure_page(stream: &mut TcpStream) {
    if let Err(error) = write_response(stream, 400, LOGIN_FAILURE_BODY) {
        tracing::warn!(%error, "failed to send the failed OAuth login page");
    }
}

pub(super) struct PendingCallback {
    stream: TcpStream,
    pub(super) code: String,
    permit: crate::resource::DescriptorPermit,
}

impl PendingCallback {
    fn respond_success(&mut self) {
        tracing::trace!(
            descriptor_weight = self.permit.weight(),
            "responding to committed OAuth callback"
        );
        if let Err(error) = write_response(
            &mut self.stream,
            200,
            "Login complete. You can close this browser window and return to norn.",
        ) {
            tracing::warn!(%error, "failed to send the completed OAuth login page");
        }
    }

    fn respond_failure(&mut self) {
        tracing::trace!(
            descriptor_weight = self.permit.weight(),
            "responding to canceled OAuth callback"
        );
        write_failure_page(&mut self.stream);
    }
}
