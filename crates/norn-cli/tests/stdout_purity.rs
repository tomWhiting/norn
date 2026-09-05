//! stdout purity for the machine output formats (retry-forever DESIGN D9,
//! commit C1).
//!
//! The owner acceptance criterion for C1 is an end-to-end run of the BUILT
//! binary, with `RUST_LOG=trace` so the engine's tracing is voluminous,
//! against a local mock provider:
//!
//! - `-f json`: stdout is exactly one JSON envelope line, nothing else.
//! - `-f stream-json`: every stdout line is one NDJSON object.
//! - in both modes the trace output appears ONLY on stderr — a single
//!   non-JSON byte on stdout fails the test regardless of which mechanism
//!   produced it (M1 subscriber routing, M3 stream splitting, a stray
//!   `println!`, …).
//! - `-f stream-json -o PATH` (M3): the WHOLE stream — streamed events,
//!   diagnostics and the terminal `completed` envelope — lands in the file
//!   in order, and stdout stays empty. Pre-fix the streamed events went to
//!   terminal stdout while only the envelope reached the file.
//!
//! The provider is a raw TCP responder serving the minimal
//! openai-compatible chat-completions SSE stream (imitating the wiremock
//! fixtures in `norn/src/provider/openai_compatible/execute.rs`), reached
//! through the existing `-c base_url=` config surface — no new dependency
//! and no invented configuration.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};

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

/// A local mock provider: accepts connections for the lifetime of the test
/// process and answers every request with [`SSE_BODY`].
fn spawn_mock_provider() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    std::thread::spawn(move || serve_connection(stream));
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
/// with the SSE stream, then close.
fn serve_connection(mut stream: TcpStream) {
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

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\n\
         Connection: close\r\n\r\n{SSE_BODY}",
        len = SSE_BODY.len(),
    );
    stream
        .write_all(response.as_bytes())
        .expect("write mock response");
    stream.flush().expect("flush mock response");
}

/// Run the built binary in print mode against the mock provider with
/// `RUST_LOG=trace`, an isolated `NORN_HOME`, and no session persistence.
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
        // Explicit Sol fixture budget: the mock route has no model catalogue.
        "-c",
        "context_window=272000",
        "-c",
        "max_retries=0",
        "--no-session",
    ])
    .args(extra_args)
    .arg("say hi")
    .env("NORN_HOME", home.path())
    .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
    .env("RUST_LOG", "trace")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd.output().expect("run norn")
}

/// Every non-empty stdout line must parse as a JSON object; the raw text is
/// returned for further assertions.
fn assert_pure_json_lines(stdout: &str, context: &str) -> Vec<Value> {
    assert!(
        !stdout.trim().is_empty(),
        "{context}: stdout must carry the requested format, got nothing"
    );
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
                panic!("{context}: non-JSON byte(s) on stdout ({err}): {line:?}")
            })
        })
        .collect()
}

/// `RUST_LOG=trace` guarantees a large volume of engine tracing; it must be
/// on stderr and nowhere else.
fn assert_trace_on_stderr(stderr: &str, context: &str) {
    assert!(
        stderr.contains("TRACE") || stderr.contains("DEBUG"),
        "{context}: RUST_LOG=trace must produce engine diagnostics on stderr: {stderr:?}"
    );
}

#[test]
fn json_mode_stdout_carries_only_the_envelope_under_trace_logging() {
    let port = spawn_mock_provider();
    let out = run_against_mock(port, &["-f", "json"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the mock run must succeed — stdout: {stdout:?}, stderr: {stderr:?}"
    );
    let lines = assert_pure_json_lines(&stdout, "-f json");
    assert_eq!(lines.len(), 1, "one envelope line only: {stdout:?}");
    assert_eq!(lines[0]["stop"]["reason"], "completed");
    assert_eq!(lines[0]["output"], "hello");
    assert_trace_on_stderr(&stderr, "-f json");
}

#[test]
fn stream_json_mode_stdout_carries_only_ndjson_under_trace_logging() {
    let port = spawn_mock_provider();
    let out = run_against_mock(port, &["-f", "stream-json"]);
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the mock run must succeed — stdout: {stdout:?}, stderr: {stderr:?}"
    );
    let lines = assert_pure_json_lines(&stdout, "-f stream-json");
    for line in &lines {
        assert!(
            line["type"].is_string(),
            "every NDJSON line is a typed event: {line}"
        );
    }
    let last = lines.last().expect("at least the completed event");
    assert_eq!(last["type"], "completed");
    assert_eq!(last["stop"]["reason"], "completed");
    assert_trace_on_stderr(&stderr, "-f stream-json");
}

/// M3: `-o PATH` must redirect the WHOLE stream. Pre-fix the streamed
/// events went to terminal stdout and only the terminal envelope reached
/// the file.
#[test]
fn stream_json_output_file_receives_events_and_envelope_with_empty_stdout() {
    let port = spawn_mock_provider();
    let dir = tempfile::tempdir().expect("temp output dir");
    let path = dir.path().join("stream.ndjson");
    let out = run_against_mock(
        port,
        &[
            "-f",
            "stream-json",
            "-o",
            path.to_str().expect("utf-8 path"),
        ],
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf-8");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the mock run must succeed — stdout: {stdout:?}, stderr: {stderr:?}"
    );
    assert_eq!(
        stdout, "",
        "-o redirects the whole stream; stdout must stay empty: {stdout:?}"
    );

    let raw = std::fs::read_to_string(&path).expect("output file written");
    let lines: Vec<Value> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("non-JSON line in the output file ({err}): {line:?}"))
        })
        .collect();
    assert!(
        lines.len() >= 2,
        "the file carries the streamed events AND the terminal envelope: {raw:?}"
    );
    let last = lines.last().expect("terminal envelope");
    assert_eq!(
        last["type"], "completed",
        "the terminal envelope is appended after the events: {raw:?}"
    );
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .any(|line| line["type"] == "done"),
        "the streamed provider events must be in the file, not on the terminal: {raw:?}"
    );
    assert_trace_on_stderr(&stderr, "-f stream-json -o PATH");
}
