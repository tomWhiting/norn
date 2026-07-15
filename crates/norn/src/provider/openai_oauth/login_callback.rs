//! Local HTTP protocol handling for the browser OAuth callback.

use std::io::{Read as _, Write as _};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use super::{CALLBACK_POLL_INTERVAL, LOGIN_CANCELED, LoginError};

const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Path the OAuth authority redirects the browser to on this host.
pub(super) const CALLBACK_PATH: &str = "/auth/callback";

pub(super) fn configure_accepted_stream(stream: &TcpStream) -> Result<(), LoginError> {
    // Accept flag inheritance is platform-dependent. Normalize the stream
    // because the request reader relies on a bounded blocking timeout.
    stream
        .set_nonblocking(false)
        .map_err(|error| LoginError::Server(error.to_string()))
}

pub(super) fn read_request_target(
    stream: &mut TcpStream,
    remaining: Duration,
    lifecycle: &AtomicU8,
) -> Result<Option<String>, LoginError> {
    let read_window = remaining.min(IDLE_CONNECTION_TIMEOUT);
    stream
        .set_read_timeout(Some(CALLBACK_POLL_INTERVAL.min(read_window)))
        .map_err(|error| LoginError::Server(error.to_string()))?;
    let started = std::time::Instant::now();
    let mut header = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while header.len() < MAX_REQUEST_HEADER_BYTES {
        if lifecycle.load(Ordering::Acquire) == LOGIN_CANCELED {
            return Err(LoginError::Canceled);
        }
        if started.elapsed() >= read_window {
            return Ok(None);
        }
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
                ) => {}
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

pub(super) fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
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
pub(super) enum CallbackDisposition {
    /// Not the OAuth redirect for this attempt (wrong path, unparseable URL, or
    /// non-matching `state`). The listener answers 404 and keeps waiting.
    Foreign,
    /// The state-matching redirect and its authorization result.
    Ours(Result<String, LoginError>),
}

pub(super) fn classify_callback(callback_url: &str, expected_state: &str) -> CallbackDisposition {
    let Ok(url) = url::Url::parse(callback_url) else {
        return CallbackDisposition::Foreign;
    };
    if url.path() != CALLBACK_PATH {
        return CallbackDisposition::Foreign;
    }
    let mut code = None;
    let mut state = None;
    let mut callback_failed = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => callback_failed = true,
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return CallbackDisposition::Foreign;
    }
    if callback_failed {
        return CallbackDisposition::Ours(Err(LoginError::AuthorizationFailed));
    }
    CallbackDisposition::Ours(
        code.filter(|value| !value.is_empty())
            .ok_or(LoginError::MissingCode),
    )
}
