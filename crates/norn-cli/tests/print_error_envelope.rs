//! Typed error envelope on plain print-mode failures (owner rulings
//! 2026-07-06, `docs/reviews/2026-07-05-context-window-incident.md`
//! "Second bug").
//!
//! Pre-fix, `norn -p -f json` / `-f stream-json` emitted NOTHING on
//! stdout when the run failed — machine consumers saw "unparseable
//! output" instead of a typed stop. These tests spawn the real binary
//! and prove, per failure class:
//!
//! - R2: every post-argument-parsing failure emits the error envelope on
//!   the machine formats, INCLUDING auth (exit 3) and pre-assembly
//!   failures; argument errors (exit 2) stay stderr-only (clap parity).
//! - R3: the payload is minimal — `output: null`, zeroed usage, no
//!   events.
//! - R5: `--output PATH` receives the same envelope.
//! - Exit codes are unchanged in every case — the envelope is additive.
//! - Driven mode does NOT double-emit: its stdout stays pure JSON-RPC
//!   and the failure rides the id-matched error response.
//!
//! The agent-path failures dial an unroutable local port with all retry
//! budgets zeroed, so no live network or model is involved and each run
//! fails fast and deterministically.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

/// Spawn `norn` in plain print mode with an isolated `NORN_HOME`, no
/// stdin, and the given args; collect the full output. `with_key`
/// controls whether the openai-compatible API key env var is present —
/// it is force-removed otherwise so a developer machine's environment
/// cannot leak in.
fn run_print(args: &[&str], with_key: bool) -> Output {
    let home = tempfile::tempdir().expect("temp NORN_HOME");
    let mut cmd = Command::new(norn_bin());
    cmd.args(args)
        .env("NORN_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if with_key {
        cmd.env("NORN_OPENAI_COMPAT_API_KEY", "test-key");
    } else {
        cmd.env_remove("NORN_OPENAI_COMPAT_API_KEY");
    }
    cmd.output().expect("run norn")
}

/// Args that make the agent step fail fast and deterministically: an
/// unroutable local endpoint (port 9, discard) with the HTTP and loop
/// retry budgets zeroed.
const AGENT_FAILURE_ARGS: &[&str] = &[
    "-p",
    "--provider",
    "openai-compatible",
    "-c",
    "base_url=http://127.0.0.1:9/v1",
    "-c",
    "max_retries=0",
    "-c",
    "retry_max=0",
    "--no-session",
];

/// Assert the minimal error-envelope contract shared by every class
/// (R3): version 1, `stop.reason == "error"`, the expected class, null
/// output, zeroed usage.
fn assert_error_envelope(envelope: &Value, class: &str) {
    assert_eq!(envelope["envelope_version"], json!(1), "{envelope}");
    assert_eq!(envelope["stop"]["reason"], json!("error"), "{envelope}");
    assert_eq!(envelope["stop"]["class"], json!(class), "{envelope}");
    assert!(
        envelope["stop"]["message"].is_string(),
        "the stop carries the failure text: {envelope}"
    );
    assert!(envelope["output"].is_null(), "{envelope}");
    assert_eq!(envelope["usage"]["input_tokens"], json!(0), "{envelope}");
    assert_eq!(envelope["usage"]["output_tokens"], json!(0), "{envelope}");
}

/// An agent-path failure (provider unreachable during the step) in
/// `-f json`: one parseable envelope on stdout with `class: "agent"`,
/// the resolved model carried, exit code 1 unchanged, stderr line intact.
#[test]
fn agent_failure_emits_error_envelope_in_json_mode() {
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    args.extend_from_slice(&["-f", "json", "say hi"]);
    let out = run_print(&args, true);

    assert_eq!(out.status.code(), Some(1), "agent failures still exit 1");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope ({err}): {stdout:?}"));
    assert_error_envelope(&envelope, "agent");
    assert!(
        envelope["model"].is_string(),
        "assembly succeeded, so the resolved model is carried: {envelope}"
    );
    assert!(
        envelope["session_id"].is_null(),
        "--no-session keeps session_id null: {envelope}"
    );
    assert_eq!(envelope["events"], json!([]), "minimal payload (R3)");
    // The stderr surface is unchanged — the envelope is additive.
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("norn: agent error:"),
        "stderr line stays: {stderr:?}"
    );
}

/// The same agent-path failure in `-f stream-json`: every stdout line is
/// valid NDJSON and the LAST line is the terminal `completed` event
/// carrying the error stop — a stream consumer sees a typed stop, never
/// a silently truncated stream.
#[test]
fn agent_failure_emits_terminal_error_event_in_stream_json_mode() {
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    args.extend_from_slice(&["-f", "stream-json", "say hi"]);
    let out = run_print(&args, true);

    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "the terminal error event must be present"
    );
    let parsed: Vec<Value> = lines
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("non-JSON stdout line ({err}): {line:?}"))
        })
        .collect();
    let last = parsed.last().unwrap();
    assert_eq!(last["type"], json!("completed"), "{last}");
    assert_error_envelope(last, "agent");
}

/// A pre-assembly auth failure (missing API key env) in `-f json`: the
/// envelope is emitted with `class: "auth"`, `model: null` (the failure
/// precedes model/provider assembly), and the exit code stays 3.
#[test]
fn pre_assembly_auth_failure_emits_envelope_with_auth_class_and_exit_3() {
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    args.extend_from_slice(&["-f", "json", "say hi"]);
    let out = run_print(&args, false);

    assert_eq!(out.status.code(), Some(3), "auth failures still exit 3");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope ({err}): {stdout:?}"));
    assert_error_envelope(&envelope, "auth");
    assert!(
        envelope["model"].is_null(),
        "pre-assembly failure: no resolved model to carry: {envelope}"
    );
    assert!(envelope["session_id"].is_null(), "{envelope}");
}

/// The same pre-assembly auth failure in `-f stream-json`: a single
/// terminal `completed` event with the auth-classed error stop.
#[test]
fn pre_assembly_auth_failure_emits_terminal_event_in_stream_json_mode() {
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    args.extend_from_slice(&["-f", "stream-json", "say hi"]);
    let out = run_print(&args, false);

    assert_eq!(out.status.code(), Some(3));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "no run ever started — exactly the terminal event: {stdout:?}"
    );
    let event: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["type"], json!("completed"), "{event}");
    assert_error_envelope(&event, "auth");
}

/// R2, subcommand spelling: `norn -p -f json session resume <stale-id>`
/// fails at resolve BEFORE the forward to the agent path — the same
/// operation as `--resume <stale-id>`, so it emits the same
/// `session`-classed envelope with `model`/`session_id` null, while the
/// stderr line and exit code stay byte-identical to before.
#[test]
fn session_resume_stale_id_emits_session_envelope_in_json_mode() {
    let out = run_print(&["-p", "-f", "json", "session", "resume", "deadbeef"], true);

    assert_eq!(out.status.code(), Some(1), "resolve failures still exit 1");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope ({err}): {stdout:?}"));
    assert_error_envelope(&envelope, "session");
    assert!(
        envelope["model"].is_null(),
        "the failure precedes assembly — no resolved model: {envelope}"
    );
    assert!(envelope["session_id"].is_null(), "{envelope}");
    assert_eq!(envelope["events"], json!([]), "minimal payload (R3)");
    // The stderr surface is byte-identical to before — the envelope is
    // strictly additive.
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("Session not found: deadbeef"),
        "stderr line stays: {stderr:?}"
    );
}

/// The `session fork <stale-id>` spelling in `-f stream-json`: exactly
/// one terminal `completed` event carrying the session-classed error
/// stop — never an empty stream.
#[test]
fn session_fork_stale_id_emits_terminal_event_in_stream_json_mode() {
    let out = run_print(
        &["-p", "-f", "stream-json", "session", "fork", "deadbeef"],
        true,
    );

    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "no run ever started — exactly the terminal event: {stdout:?}"
    );
    let event: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["type"], json!("completed"), "{event}");
    assert_error_envelope(&event, "session");
}

/// The same resolve failure in text mode stays stderr-only: the human
/// format never gets an envelope, on the subcommand spelling too.
#[test]
fn session_resume_stale_id_text_mode_emits_nothing_on_stdout() {
    let out = run_print(&["-p", "session", "resume", "deadbeef"], true);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "text mode never gets an envelope: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("Session not found: deadbeef"), "{stderr:?}");
}

/// R5: `--output PATH` receives the error envelope; stdout stays empty
/// (the file is the selected surface), and the exit code is unchanged.
#[test]
fn output_file_receives_error_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    let path_str = path.to_str().unwrap();
    args.extend_from_slice(&["-f", "json", "-o", path_str, "say hi"]);
    let out = run_print(&args, true);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "-o routes the envelope to the file, not stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let raw = std::fs::read_to_string(&path).expect("the error envelope file must exist (R5)");
    let envelope: Value = serde_json::from_str(raw.trim()).unwrap();
    assert_error_envelope(&envelope, "agent");
}

/// Text mode is the human surface: no envelope on stdout, the stderr
/// line and exit code carry the failure exactly as before.
#[test]
fn text_mode_failure_emits_nothing_on_stdout() {
    let mut args = AGENT_FAILURE_ARGS.to_vec();
    args.push("say hi");
    let out = run_print(&args, true);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "text mode never gets an envelope: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("norn: agent error:"), "{stderr:?}");
}

/// R2 boundary: a post-clap argument error (invalid `--output-schema`)
/// exits 2 with NOTHING on stdout — argument errors keep clap parity on
/// every format.
#[test]
fn argument_error_emits_nothing_on_stdout() {
    for format in ["json", "stream-json"] {
        let out = run_print(
            &[
                "-p",
                "-f",
                format,
                "--output-schema",
                "{invalid-json",
                "say hi",
            ],
            true,
        );
        assert_eq!(out.status.code(), Some(2), "argument errors still exit 2");
        assert!(
            out.stdout.is_empty(),
            "-f {format}: argument errors are stderr-only: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains("argument error"), "{stderr:?}");
    }
}

/// Clap-level usage errors (exit 2) are equally envelope-free — the
/// boundary is argument parsing, wherever it fails.
#[test]
fn clap_usage_error_emits_nothing_on_stdout() {
    let out = run_print(&["-p", "-f", "json", "--definitely-not-a-flag"], true);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "usage errors are stderr-only: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Driven mode must NOT double-emit: a failing accepted `run/execute` is
/// answered as the id-matched JSON-RPC error response, stdout carries
/// ONLY JSON-RPC frames (no bare error envelope, no `completed` event),
/// and the process exit code still reflects the failure class.
#[test]
fn driven_mode_failure_answers_error_response_without_envelope() {
    let home = tempfile::tempdir().expect("temp NORN_HOME");
    let mut child = Command::new(norn_bin())
        .args([
            "--protocol",
            "jsonrpc",
            "--provider",
            "openai-compatible",
            "-c",
            "base_url=http://127.0.0.1:9/v1",
            "-c",
            "max_retries=0",
            "-c",
            "retry_max=0",
            "--no-session",
        ])
        .env("NORN_HOME", home.path())
        .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn norn --protocol jsonrpc");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n\
                  {\"jsonrpc\":\"2.0\",\"id\":\"run-1\",\"method\":\"run/execute\",\"params\":{\"prompt\":\"go\"}}\n",
            )
            .expect("write frames");
        // Drop closes stdin; mid-run EOF only stops the intervene reader
        // — the run itself proceeds to its (failing) terminal response.
    }

    let out = child.wait_with_output().expect("norn exits");
    assert_eq!(
        out.status.code(),
        Some(1),
        "the CLI exit contract holds alongside the wire answer"
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut error_response = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let frame: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("non-JSON-RPC stdout line ({err}): {line:?}"));
        assert_eq!(
            frame["jsonrpc"],
            json!("2.0"),
            "driven stdout carries ONLY JSON-RPC frames: {frame}"
        );
        assert!(
            frame.get("envelope_version").is_none(),
            "no bare print envelope on the driven channel: {frame}"
        );
        assert!(
            frame.get("type").is_none(),
            "no stream-json event on the driven channel: {frame}"
        );
        if frame["id"] == json!("run-1") {
            error_response = Some(frame);
        }
    }
    let response = error_response.expect("the accepted run/execute is answered id-matched");
    assert_eq!(response["error"]["code"], json!(-32603), "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("agent error"),
        "the typed failure rides the response: {response}"
    );
    assert!(response.get("result").is_none(), "{response}");
}
