//! Human retry surfaces for headless runs (retry-forever DESIGN D8,
//! commit C6).
//!
//! The engine retries transient provider failures and announces every wait
//! as a typed `AgentStreamRetry` event. `-f stream-json` already carries
//! that event; the human formats did not carry anything at all, so a
//! headless run under an unbounded policy looked like a hang. This test
//! drives the BUILT binary against a mock provider that fails its first
//! request and succeeds on the next, and asserts:
//!
//! - `-f text`: one concise retry line per notice, on STDERR — stdout
//!   still carries only the model's output (D9's stdout discipline holds
//!   for the human format too),
//! - `-q`: the same run says nothing on stderr about the retry,
//! - `-f json`: the retry activity is rolled up into the envelope's
//!   `diagnostics` array (attempt count, total backoff, classes seen)
//!   with stdout still exactly one JSON line.
//!
//! The provider is the same raw TCP responder shape `stdout_purity.rs`
//! uses, with a request counter so the first attempt fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

/// The minimal openai-compatible SSE stream: one content delta, a
/// `finish_reason` chunk carrying usage, then the terminal sentinel.
const SSE_BODY: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
      \"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

/// The 5xx body the first request is answered with — a server error is a
/// transient class, so the loop's retry brain replays the request.
const ERROR_BODY: &str =
    "{\"error\":{\"message\":\"upstream unavailable\",\"type\":\"server_error\"}}";

/// A local mock provider that fails its first `fail_first` requests with
/// HTTP 500 and answers everything after that with [`SSE_BODY`].
fn spawn_flaky_provider(fail_first: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let seen = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let seen = Arc::clone(&seen);
                    std::thread::spawn(move || {
                        let index = seen.fetch_add(1, Ordering::SeqCst);
                        serve_connection(stream, index < fail_first);
                    });
                }
                Err(err) => {
                    eprintln!("mock provider accept failed: {err}");
                    return;
                }
            }
        }
    });
    port
}

/// Read one HTTP/1.1 request (headers + `Content-Length` body) and answer
/// with either the 500 or the SSE stream, then close.
fn serve_connection(mut stream: TcpStream, fail: bool) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone mock socket"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read request line");
        if read == 0 {
            return;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
            .and_then(|value| value.parse::<usize>().ok())
        {
            content_length = value;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read request body");

    let response = if fail {
        format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\
             Content-Length: {len}\r\nConnection: close\r\n\r\n{ERROR_BODY}",
            len = ERROR_BODY.len(),
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\n\
             Connection: close\r\n\r\n{SSE_BODY}",
            len = SSE_BODY.len(),
        )
    };
    stream
        .write_all(response.as_bytes())
        .expect("write mock response");
    stream.flush().expect("flush mock response");
}

/// Run the built binary in print mode against the mock provider with a
/// bounded, jitter-free retry policy so the announced wait is exactly the
/// configured one. `max_retries=0` disables the transport's own bounded
/// retry, so the 500 reaches the loop's retry brain — the layer under
/// test.
fn run_against_mock(port: u16, extra_args: &[&str]) -> Output {
    let home = tempfile::tempdir().expect("temp NORN_HOME");
    let base_url = format!("http://127.0.0.1:{port}/v1");
    let mut cmd = Command::new(norn_bin());
    cmd.args([
        "-p",
        "--provider",
        "openai-compatible",
        "-c",
        &format!("base_url={base_url}"),
        "-c",
        "max_retries=0",
        "-c",
        "retry_max=3",
        "-c",
        "retry_base_delay=200ms",
        "-c",
        "retry_jitter=false",
        "--no-session",
    ])
    .args(extra_args)
    .arg("say hi")
    .env("NORN_HOME", home.path())
    .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd.output().expect("run norn")
}

/// Text mode: the wait is announced on stderr, once, naming the attempt,
/// the delay, the budget and the taxonomy class — while stdout still
/// carries the model output and nothing else.
#[test]
fn text_mode_reports_each_retry_wait_on_stderr() {
    let port = spawn_flaky_provider(1);
    let out = run_against_mock(port, &[]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the retried run must succeed — stdout: {stdout:?}, stderr: {stderr:?}"
    );
    assert_eq!(
        stdout, "hello\n",
        "stdout carries the model output only: {stdout:?}"
    );

    // The engine also logs its own `tracing` WARN for the same event;
    // that is the debug channel. The operator surface under test is the
    // `norn: `-prefixed line every headless operator message carries.
    let retry_lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("norn: ") && line.contains("retrying"))
        .collect();
    assert_eq!(
        retry_lines.len(),
        1,
        "exactly one line per retry notice: {stderr:?}"
    );
    let line = retry_lines[0];
    assert!(line.starts_with("norn: "), "line: {line:?}");
    assert!(line.contains("server_error"), "line: {line:?}");
    assert!(line.contains("attempt 2 of 3"), "line: {line:?}");
    assert!(line.contains("200ms"), "line: {line:?}");
}

/// `--quiet` suppresses progress on stderr; the retry line is progress.
#[test]
fn quiet_suppresses_the_retry_line() {
    let port = spawn_flaky_provider(1);
    let out = run_against_mock(port, &["-q"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(out.status.code(), Some(0), "stderr: {stderr:?}");
    assert_eq!(stdout, "hello\n");
    assert!(
        !stderr
            .lines()
            .any(|line| line.starts_with("norn: ") && line.contains("retrying")),
        "--quiet must silence the retry progress line: {stderr:?}"
    );
}

/// JSON mode: the retry activity rides the envelope's `diagnostics` array
/// (DESIGN D8: a rollup, with zero envelope schema change), and stdout is
/// still exactly one JSON line.
#[test]
fn json_mode_rolls_retry_activity_into_the_envelope_diagnostics() {
    let port = spawn_flaky_provider(2);
    let out = run_against_mock(port, &["-f", "json"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the retried run must succeed — stdout: {stdout:?}, stderr: {stderr:?}"
    );
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one envelope line only: {stdout:?}");
    let envelope: Value = serde_json::from_str(lines[0]).expect("the envelope parses");
    assert_eq!(envelope["stop"]["reason"], "completed");

    let diagnostics = envelope["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    let rollup = diagnostics
        .iter()
        .find(|diag| diag["code"] == "provider-retry")
        .unwrap_or_else(|| panic!("no provider-retry rollup in {diagnostics:?}"));
    let message = rollup["message"].as_str().expect("a message");
    assert!(
        message.contains('2'),
        "the rollup must count the retries: {message:?}"
    );
    // Two waits under the configured jitter-free policy: the 200ms base
    // and its doubling — 600ms of backoff in total.
    assert!(
        message.contains("600ms"),
        "the rollup must total the backoff waited: {message:?}"
    );
    assert!(
        message.contains("server_error"),
        "the rollup must name the classes seen: {message:?}"
    );
}

/// A run that never retries must not add a rollup: the envelope carries
/// diagnostics about what happened, not about what did not.
#[test]
fn json_mode_omits_the_rollup_when_nothing_was_retried() {
    let port = spawn_flaky_provider(0);
    let out = run_against_mock(port, &["-f", "json"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(out.status.code(), Some(0), "stderr: {stderr:?}");
    let envelope: Value =
        serde_json::from_str(stdout.trim_end()).expect("the envelope parses: {stdout:?}");
    let diagnostics = envelope["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        !diagnostics
            .iter()
            .any(|diag| diag["code"] == "provider-retry"),
        "a clean run has no retry rollup: {diagnostics:?}"
    );
}
