//! Headless signal handling, end-to-end against the BUILT binary
//! (retry-forever DESIGN D5, commit C3).
//!
//! Before this, `norn --print` had no signal handling at all: SIGINT was
//! instant process death — no `Cancelled` envelope, no tool-result repair,
//! no checkpoint. With the loop's retry policy unbounded by default, the
//! signal is also the only way to stop a run that is retrying a transient
//! provider failure, so it is now a first-class, tested surface.
//!
//! The provider here deliberately STALLS: it accepts the connection, reads
//! the request in full, tells the test the turn is genuinely in flight, and
//! then never answers. That makes the mid-turn window deterministic — the
//! signal cannot race a completed turn.
//!
//! Second-signal escalation (`128 + signo`) is pinned at unit level in
//! `print::signals` rather than here: the graceful wind-down after the
//! first signal is fast and has no configurable stall point, so a second
//! signal delivered end-to-end would race the process's own exit. A
//! coalescing-tolerant version of that race is not a deterministic test.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use serde_json::Value;

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

/// A local mock provider that never answers: it reads each request in
/// full, reports that the turn is in flight, and holds the connection open
/// until the client goes away.
fn spawn_stalling_provider() -> (u16, std::sync::mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    // Bounded so the sender cannot outrun a test that only needs the first
    // in-flight report.
    let (tx, rx) = sync_channel::<()>(8);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    std::thread::spawn(move || stall_connection(stream, &tx));
                }
                Err(err) => {
                    eprintln!("mock provider accept failed: {err}");
                    return;
                }
            }
        }
    });
    (port, rx)
}

/// Read one HTTP/1.1 request (headers + `Content-Length` body), announce
/// it, then hold the socket open without responding.
fn stall_connection(stream: TcpStream, in_flight: &SyncSender<()>) {
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

    // The turn is now genuinely in flight inside the child process.
    let _ = in_flight.send(());

    // Never answer. Park on the socket until the client closes it, keeping
    // `stream` alive so the connection is not reset.
    let mut sink = Vec::new();
    let _ = reader.read_to_end(&mut sink);
    drop(stream);
}

/// Launch the built binary in print mode against the stalling provider.
fn spawn_norn(port: u16, extra_args: &[&str]) -> (Child, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("temp NORN_HOME");
    let base_url = format!("http://127.0.0.1:{port}/v1");
    let child = Command::new(norn_bin())
        .args([
            "-p",
            "--provider",
            "openai-compatible",
            "-c",
            &format!("base_url={base_url}"),
            // Explicit Sol fixture budget: the mock route has no model catalogue.
            "-c",
            "context_window=272000",
            "--no-session",
        ])
        .args(extra_args)
        .arg("say hi")
        .env("NORN_HOME", home.path())
        .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn norn");
    (child, home)
}

/// Send one signal to a live child process. Uses `kill(1)` so the test
/// needs no libc binding.
fn signal_child(child: &Child, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} failed");
}

/// Wait for the turn to be in flight, then signal. The receive is bounded
/// so a wiring regression fails the test instead of hanging the suite.
fn wait_for_in_flight(in_flight: &std::sync::mpsc::Receiver<()>) {
    in_flight
        .recv_timeout(Duration::from_secs(60))
        .expect("the mock provider never saw a request; the turn was never in flight");
}

/// D5, first rung: a SIGINT delivered mid-turn produces the ordinary
/// graceful cancellation — the `Cancelled` envelope in the requested
/// format on stdout, and the existing `Cancelled` exit mapping
/// (`ExitCode::AgentError`, 1). Pre-fix the process died instantly with no
/// envelope at all.
#[test]
fn sigint_mid_turn_yields_a_cancelled_envelope_and_the_cancelled_exit_code() {
    let (port, in_flight) = spawn_stalling_provider();
    let (child, _home) = spawn_norn(port, &["-f", "json"]);

    wait_for_in_flight(&in_flight);
    signal_child(&child, "INT");

    let output = child
        .wait_with_output()
        .expect("the signalled run must terminate");
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "a cancelled run keeps the existing Cancelled exit mapping — \
         stdout: {stdout:?}, stderr: {stderr:?}",
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one envelope line only: {stdout:?}");
    let envelope: Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|err| panic!("stdout must be the JSON envelope ({err}): {stdout:?}"));
    assert_eq!(
        envelope["stop"]["reason"], "cancelled",
        "the signal must produce the graceful Cancelled path: {envelope}",
    );
    assert!(
        stderr.contains("SIGINT received"),
        "the operator must be told which signal cancelled the run: {stderr:?}",
    );
}

/// SIGTERM — what an orchestrator, container runtime or `systemd` sends —
/// takes the same graceful first rung as an operator's Ctrl-C, rather than
/// killing the process before it can checkpoint.
#[test]
fn sigterm_mid_turn_takes_the_same_graceful_path() {
    let (port, in_flight) = spawn_stalling_provider();
    let (child, _home) = spawn_norn(port, &["-f", "json"]);

    wait_for_in_flight(&in_flight);
    signal_child(&child, "TERM");

    let output = child
        .wait_with_output()
        .expect("the signalled run must terminate");
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "SIGTERM must exit through the Cancelled mapping, not die by signal — \
         stdout: {stdout:?}, stderr: {stderr:?}",
    );
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one envelope line only: {stdout:?}");
    let envelope: Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|err| panic!("stdout must be the JSON envelope ({err}): {stdout:?}"));
    assert_eq!(envelope["stop"]["reason"], "cancelled", "{envelope}");
    assert!(
        stderr.contains("SIGTERM received"),
        "the operator must be told which signal cancelled the run: {stderr:?}",
    );
}

/// The streamed format gets the same treatment: the terminal event on the
/// NDJSON stream carries the cancellation, so a streaming consumer sees a
/// well-formed end instead of a truncated stream.
#[test]
fn sigint_mid_turn_terminates_the_stream_json_output_cleanly() {
    let (port, in_flight) = spawn_stalling_provider();
    let (child, _home) = spawn_norn(port, &["-f", "stream-json"]);

    wait_for_in_flight(&in_flight);
    signal_child(&child, "INT");

    let output = child
        .wait_with_output()
        .expect("the signalled run must terminate");
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {stdout:?}, stderr: {stderr:?}",
    );
    let events: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("non-JSON line on the stream ({err}): {line:?}"))
        })
        .collect();
    let last = events
        .last()
        .expect("the stream must carry a terminal event");
    assert_eq!(
        last["stop"]["reason"], "cancelled",
        "the stream's terminal event must report the cancellation: {last}",
    );
}
