use std::io::{Read as _, Write as _};

use super::super::types::CodexAuth;
use super::browser_prompt::PreparedBrowserLaunch;
use super::*;

const CALLBACK_ERROR_SECRET: &str = "callback-secret-must-not-escape";

#[test]
fn server_options_retain_validated_auth_root() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let expected = NornAuthRoot::try_from(directory.path())?;
    let options = ServerOptions::new(
        expected.clone(),
        "test-client".to_owned(),
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default(),
    );

    assert_eq!(options.auth_root, expected);
    Ok(())
}

// -- classify_callback --------------------------------------------------

#[test]
fn classify_matching_callback_yields_code() -> Result<(), std::io::Error> {
    let disposition = classify_callback("http://localhost/auth/callback?code=abc&state=s1", "s1");
    let CallbackDisposition::Ours(Ok(code)) = disposition else {
        return Err(std::io::Error::other(
            "expected a successful state-matching callback",
        ));
    };
    assert_eq!(code, "abc");
    Ok(())
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
fn classify_matching_error_callback_fails_without_disclosure()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "callback-secret-must-not-escape";
    let callback =
        format!("http://localhost/auth/callback?error={SECRET}%0Aforged-log-line&state=s1");
    let CallbackDisposition::Ours(Err(error)) = classify_callback(&callback, "s1") else {
        return Err(std::io::Error::other(
            "expected Ours(Err) for a state-matching error callback",
        )
        .into());
    };
    assert!(matches!(error, LoginError::AuthorizationFailed));
    let rendered = error.to_string();
    assert!(!rendered.contains(SECRET), "rendered error: {rendered}");
    assert!(
        !rendered.contains("forged-log-line"),
        "rendered error: {rendered}"
    );
    Ok(())
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
fn raw_get(port: u16, path: &str) -> Result<String, std::io::Error> {
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port))?;
    socket.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    read_http_response(&mut socket)
}

fn read_http_response(socket: &mut TcpStream) -> Result<String, std::io::Error> {
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        socket.read_exact(&mut byte)?;
        response.push(byte[0]);
    }
    let headers = std::str::from_utf8(&response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .ok_or_else(|| std::io::Error::other("HTTP response omitted Content-Length"))?
        .parse::<usize>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let body_start = response.len();
    response.resize(body_start.saturating_add(content_length), 0);
    socket.read_exact(&mut response[body_start..])?;
    String::from_utf8(response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
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

fn waiting_lifecycle() -> Arc<AtomicU8> {
    Arc::new(AtomicU8::new(LOGIN_WAITING))
}

fn no_browser_launch() -> PreparedBrowserLaunch {
    PreparedBrowserLaunch {
        browser_launch: None,
        manual_fallback: false,
    }
}

fn test_auth_document() -> Result<AuthDotJson, std::io::Error> {
    match CodexAuth::create_dummy_chatgpt_auth_for_testing() {
        CodexAuth::ChatGpt(auth) => Ok(*auth),
        CodexAuth::ApiKey(_) => Err(std::io::Error::other(
            "dummy ChatGPT credential returned an API key",
        )),
    }
}

#[test]
fn cancellation_wins_callback_lifecycle_when_it_claims_waiting_first() {
    let lifecycle = AtomicU8::new(LOGIN_WAITING);
    cancel_waiting_login(&lifecycle);
    assert!(matches!(
        claim_callback(&lifecycle),
        Err(LoginError::Canceled)
    ));
    assert_eq!(lifecycle.load(Ordering::Acquire), LOGIN_CANCELED);
}

#[test]
fn callback_claim_wins_lifecycle_before_later_cancellation() {
    let lifecycle = AtomicU8::new(LOGIN_WAITING);
    assert!(claim_callback(&lifecycle).is_ok());
    cancel_waiting_login(&lifecycle);
    assert_eq!(lifecycle.load(Ordering::Acquire), LOGIN_CALLBACK_CLAIMED);
}

/// Regression test (final-state hardening, T1 item 7): the callback
/// server previously treated the FIRST request on the port as the
/// OAuth callback, so a browser favicon probe or a stray request
/// aborted the login. It must now answer foreign requests with 404
/// and keep listening until the state-matching callback arrives.
#[test]
fn stray_requests_get_404_and_login_still_completes() -> Result<(), Box<dyn std::error::Error>> {
    let (listener, port, listener_permit, accepted_permit) = test_server()?;

    let waiter = std::thread::spawn(move || {
        let lifecycle = waiting_lifecycle();
        wait_for_callback(
            listener,
            listener_permit,
            accepted_permit,
            "expected-state",
            Duration::from_secs(10),
            no_browser_launch(),
            &lifecycle,
        )
    });

    let favicon = raw_get(port, "/favicon.ico")?;
    assert!(
        favicon.starts_with("HTTP/1.1 404"),
        "foreign path must get 404: {favicon}"
    );

    let forged = raw_get(port, "/auth/callback?code=evil&state=forged")?;
    assert!(
        forged.starts_with("HTTP/1.1 404"),
        "state-mismatched callback must get 404: {forged}"
    );

    let genuine = std::thread::spawn(move || {
        raw_get(port, "/auth/callback?code=real-code&state=expected-state")
    });
    let callback = waiter
        .join()
        .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))??;
    assert_eq!(callback.code, "real-code");
    let (prepared_sender, prepared_receiver) = oneshot::channel();
    let (acknowledgement_sender, acknowledgement_receiver) = oneshot::channel();
    let auth = test_auth_document()?;
    let finisher = std::thread::spawn(move || {
        complete_prepared_callback(auth, callback, prepared_sender, acknowledgement_receiver);
    });
    let _prepared_auth = prepared_receiver.blocking_recv()??;
    assert!(
        !genuine.is_finished(),
        "the browser response must wait for exchange and storage"
    );
    acknowledgement_sender
        .send(CommitAcknowledgement::Committed)
        .map_err(|_acknowledgement| {
            std::io::Error::other("callback acknowledgement receiver closed")
        })?;
    finisher
        .join()
        .map_err(|error| std::io::Error::other(format!("callback finisher panicked: {error:?}")))?;
    let genuine = genuine.join().map_err(|error| {
        std::io::Error::other(format!("genuine request thread panicked: {error:?}"))
    })??;
    assert!(
        genuine.starts_with("HTTP/1.1 200"),
        "the genuine callback must get the success page: {genuine}"
    );
    Ok(())
}

#[test]
fn matching_error_callback_fails_the_flow_with_a_400_page() -> Result<(), Box<dyn std::error::Error>>
{
    let (listener, port, listener_permit, accepted_permit) = test_server()?;

    let waiter = std::thread::spawn(move || {
        let lifecycle = waiting_lifecycle();
        wait_for_callback(
            listener,
            listener_permit,
            accepted_permit,
            "expected-state",
            Duration::from_secs(10),
            no_browser_launch(),
            &lifecycle,
        )
    });

    let response = raw_get(
        port,
        &format!(
            "/auth/callback?error={CALLBACK_ERROR_SECRET}%0Aforged-log-line&state=expected-state"
        ),
    )?;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "a failed genuine callback must get the failure page: {response}"
    );

    let result = waiter
        .join()
        .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
    let Err(error @ LoginError::AuthorizationFailed) = result else {
        return Err(std::io::Error::other("expected authorization failure").into());
    };
    let rendered = error.to_string();
    assert!(
        !rendered.contains(CALLBACK_ERROR_SECRET),
        "rendered error: {rendered}"
    );
    assert!(
        !rendered.contains("forged-log-line"),
        "rendered error: {rendered}"
    );
    Ok(())
}

#[test]
fn accepted_connection_waits_for_delayed_request_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let (listener, port, listener_permit, accepted_permit) = test_server()?;

    let waiter = std::thread::spawn(move || {
        let lifecycle = waiting_lifecycle();
        wait_for_callback(
            listener,
            listener_permit,
            accepted_permit,
            "expected-state",
            Duration::from_secs(2),
            no_browser_launch(),
            &lifecycle,
        )
    });

    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port))?;
    std::thread::sleep(Duration::from_millis(100));
    socket.write_all(
        b"GET /auth/callback?error=callback-secret-must-not-escape&state=expected-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )?;
    let response = read_http_response(&mut socket)?;

    assert!(
        response.starts_with("HTTP/1.1 400"),
        "a delayed genuine callback must get the failure page: {response}"
    );
    let result = waiter
        .join()
        .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
    let Err(error @ LoginError::AuthorizationFailed) = result else {
        return Err(std::io::Error::other("expected authorization failure").into());
    };
    assert!(
        !error
            .to_string()
            .contains("callback-secret-must-not-escape"),
        "callback-controlled error text must not be rendered"
    );
    Ok(())
}

#[test]
fn cancellation_releases_callback_listener_without_waiting_for_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let (listener, port, listener_permit, accepted_permit) = test_server()?;
    let lifecycle = waiting_lifecycle();
    let worker_lifecycle = Arc::clone(&lifecycle);
    let waiter = std::thread::spawn(move || {
        wait_for_callback(
            listener,
            listener_permit,
            accepted_permit,
            "expected-state",
            Duration::from_secs(10),
            no_browser_launch(),
            &worker_lifecycle,
        )
    });

    cancel_waiting_login(&lifecycle);
    let result = waiter
        .join()
        .map_err(|error| std::io::Error::other(format!("waiter thread panicked: {error:?}")))?;
    assert!(matches!(result, Err(LoginError::Canceled)));
    let rebound = TcpListener::bind(("127.0.0.1", port))?;
    drop(rebound);
    Ok(())
}

#[test]
fn cancellation_with_partial_callback_request_returns_failure_page()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let mut browser = TcpStream::connect(("127.0.0.1", port))?;
    let (mut accepted, _peer) = listener.accept()?;
    configure_accepted_stream(&accepted)?;
    let lifecycle = waiting_lifecycle();
    let worker_lifecycle = Arc::clone(&lifecycle);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        ready_tx.send(()).map_err(|error| {
            LoginError::Server(format!("callback reader barrier closed: {error}"))
        })?;
        super::callback_worker::read_accepted_request_target(
            &mut accepted,
            Duration::from_secs(10),
            &worker_lifecycle,
        )
    });

    ready_rx.recv()?;
    browser.write_all(b"GET /auth/callback")?;
    cancel_waiting_login(&lifecycle);
    let result = reader
        .join()
        .map_err(|error| std::io::Error::other(format!("reader thread panicked: {error:?}")))?;
    assert!(matches!(result, Err(LoginError::Canceled)));

    let response = read_http_response(&mut browser)?;
    assert!(response.starts_with("HTTP/1.1 400"), "response: {response}");
    assert!(response.contains("Login failed. Return to norn for details."));
    assert!(!response.contains("Login complete"));
    Ok(())
}

#[test]
fn cancellation_between_classification_and_claim_returns_failure_page()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let mut browser = TcpStream::connect(("127.0.0.1", port))?;
    let (mut accepted, _peer) = listener.accept()?;
    let lifecycle = AtomicU8::new(LOGIN_CANCELED);

    let result = super::callback_worker::claim_accepted_callback(&lifecycle, &mut accepted);
    assert!(matches!(result, Err(LoginError::Canceled)));
    drop(accepted);

    let response = read_http_response(&mut browser)?;
    assert!(response.starts_with("HTTP/1.1 400"), "response: {response}");
    assert!(response.contains("Login failed. Return to norn for details."));
    assert!(!response.contains("Login complete"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn accepted_stream_is_normalized_to_blocking_mode() -> Result<(), Box<dyn std::error::Error>> {
    use rustix::fs::{OFlags, fcntl_getfl};

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let connector = std::thread::spawn(move || TcpStream::connect(("127.0.0.1", port)));
    let (stream, _peer) = listener.accept()?;
    let client = connector
        .join()
        .map_err(|error| std::io::Error::other(format!("connector panicked: {error:?}")))??;

    stream.set_nonblocking(true)?;
    assert!(fcntl_getfl(&stream)?.contains(OFlags::NONBLOCK));
    configure_accepted_stream(&stream)?;
    assert!(!fcntl_getfl(&stream)?.contains(OFlags::NONBLOCK));
    drop(client);
    Ok(())
}

/// The overall wait is a total budget: stray requests must not extend
/// it, and with no genuine callback the wait ends in the timeout
/// error.
#[test]
fn wait_times_out_when_no_matching_callback_arrives() -> Result<(), Box<dyn std::error::Error>> {
    let (listener, port, listener_permit, accepted_permit) = test_server()?;

    // Generous budget so the stray request below reliably lands while
    // the waiter is still listening, even on a loaded machine.
    let waiter = std::thread::spawn(move || {
        let lifecycle = waiting_lifecycle();
        wait_for_callback(
            listener,
            listener_permit,
            accepted_permit,
            "expected-state",
            Duration::from_secs(2),
            no_browser_launch(),
            &lifecycle,
        )
    });

    // A stray request part-way through must not reset the deadline.
    let stray = raw_get(port, "/not-the-callback")?;
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
