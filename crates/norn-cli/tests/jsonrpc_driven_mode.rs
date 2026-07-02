//! End-to-end integration tests for the driven JSON-RPC mode
//! (`docs/design/norn-cli/DRIVEN-PROTOCOL.md`).
//!
//! Every test spawns the real `norn` binary with `--protocol jsonrpc` and
//! drives it over its actual stdin/stdout, proving the full wiring — CLI
//! flag → `detect_mode` (unaffected) → print dispatch → the driven duplex
//! loop — at the process boundary, including the single serializing writer
//! and the newline-delimited framing. The agent-facing tests drive the
//! `openai-compatible` provider against a local hand-rolled HTTP stub that
//! answers Chat Completions SSE, so no live model or network is involved.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

/// Upper bound on any single child interaction; a hung child is killed so
/// the suite fails with an assertion instead of hanging forever.
const WATCHDOG: Duration = Duration::from_mins(2);

/// A Chat Completions SSE body that streams "hello" and completes.
const SSE_COMPLETION: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

/// A minimal hand-rolled HTTP stub standing in for an OpenAI-compatible
/// Chat Completions endpoint. Serves every connection the same SSE
/// completion; when a `gate` is supplied, the FIRST connection blocks
/// after reading the request until the gate fires — holding the run
/// in-flight so a test can interleave mid-run JSON-RPC traffic
/// deterministically.
struct SseStub {
    base_url: String,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SseStub {
    fn spawn(gate: Option<mpsc::Receiver<()>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
        let addr = listener.local_addr().expect("stub addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut gate = gate;
            for stream in listener.incoming() {
                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { return };
                stream
                    .set_read_timeout(Some(WATCHDOG))
                    .expect("stub read timeout");
                read_http_request(&mut stream);
                // Only the first connection is gated: it holds the run
                // in-flight until the test releases it.
                if let Some(rx) = gate.take() {
                    rx.recv().expect("gate release");
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                    SSE_COMPLETION.len(),
                    SSE_COMPLETION,
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("stub write response");
                stream.flush().expect("stub flush");
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            stop,
            addr,
            thread: Some(thread),
        }
    }

    /// Stop the accept loop: raise the flag, poke a dummy connection to
    /// unblock `accept`, and join the thread so nothing leaks across tests.
    fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        drop(TcpStream::connect(self.addr));
        if let Some(thread) = self.thread.take() {
            thread.join().expect("stub thread joins cleanly");
        }
    }
}

/// Read one HTTP request (headers + content-length body) off `stream`.
fn read_http_request(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("stub read header line");
        assert!(n > 0, "peer closed mid-headers");
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().expect("content-length value");
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("stub read body");
}

/// Spawn `norn --protocol jsonrpc` with an isolated `NORN_HOME`, extra
/// args, and a watchdog that kills the child if a test wedges.
struct DrivenChild {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    reader: BufReader<std::process::ChildStdout>,
    watchdog_disarm: mpsc::Sender<()>,
    _home: tempfile::TempDir,
}

impl DrivenChild {
    fn spawn(extra_args: &[&str]) -> Self {
        let home = tempfile::tempdir().expect("temp NORN_HOME");
        let mut child = Command::new(norn_bin())
            .arg("--protocol")
            .arg("jsonrpc")
            .args(extra_args)
            .env("NORN_HOME", home.path())
            .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn norn --protocol jsonrpc");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        // Drain stderr in the background so a chatty child can never block
        // on a full pipe; surface it for post-mortem readability.
        let stderr = child.stderr.take().expect("child stderr");
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { return };
                eprintln!("norn-child stderr: {line}");
            }
        });
        // Watchdog: kill the child if the test has not disarmed in time,
        // so a protocol bug fails the suite instead of hanging it.
        let (disarm_tx, disarm_rx) = mpsc::channel::<()>();
        let pid = child.id();
        std::thread::spawn(move || {
            if disarm_rx.recv_timeout(WATCHDOG).is_err() {
                eprintln!("watchdog: killing wedged norn child {pid}");
                // Best-effort: the child may have exited already.
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            reader: BufReader::new(stdout),
            watchdog_disarm: disarm_tx,
            _home: home,
        }
    }

    fn send(&mut self, frame: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        let mut line = frame.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).expect("write frame");
        stdin.flush().expect("flush frame");
    }

    /// Read frames until the id-matched Response arrives, collecting any
    /// interleaved `event/*` notifications. Panics on EOF or a foreign
    /// response — every response on this channel must be accounted for.
    fn read_response(&mut self, id: &Value) -> (Value, Vec<Value>) {
        let mut notifications = Vec::new();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read frame");
            assert!(n > 0, "unexpected EOF while waiting for response {id}");
            if line.trim().is_empty() {
                continue;
            }
            let parsed: Value = serde_json::from_str(line.trim()).expect("valid JSON frame");
            assert_eq!(parsed["jsonrpc"], "2.0");
            if parsed.get("method").is_some() {
                assert!(
                    parsed.get("id").is_none(),
                    "a notification must not carry an id: {parsed}"
                );
                notifications.push(parsed);
                continue;
            }
            assert_eq!(
                parsed["id"], *id,
                "responses must arrive in request order: {parsed}"
            );
            return (parsed, notifications);
        }
    }

    fn initialize(&mut self) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": "init", "method": "initialize"}));
        let (response, notes) = self.read_response(&json!("init"));
        assert!(notes.is_empty(), "no events before a run");
        response
    }

    /// Close stdin and reap the child, disarming the watchdog.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let status = self.child.wait().expect("norn exits");
        let _ = self.watchdog_disarm.send(());
        status
    }
}

#[test]
fn driven_mode_answers_initialize_over_real_stdio() {
    let mut child = DrivenChild::spawn(&[]);
    let parsed = child.initialize();

    // (a) initialize returns capabilities, id-matched, never a notification.
    assert_eq!(parsed["id"], "init");
    assert!(
        parsed.get("method").is_none(),
        "a response is never a method"
    );
    // The driven contract version consumers gate on — distinct from the
    // JSON-RPC "2.0" envelope tag.
    assert_eq!(parsed["result"]["protocol"], "norn-driven/1");
    assert_eq!(
        parsed["result"]["capabilities"]["runLifecycle"], "one_shot",
        "the one-shot run lifecycle is advertised"
    );
    let interventions = parsed["result"]["capabilities"]["interventions"]
        .as_array()
        .expect("interventions array");
    let names: Vec<&str> = interventions.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"inject_message"));
    assert!(names.contains(&"cancel"));

    // Close stdin: with no run/execute, the channel closes and the process
    // exits cleanly (the terminal shutdown handshake).
    let status = child.finish();
    assert!(
        status.success(),
        "driven mode must exit 0 when the channel closes before run/execute"
    );
}

/// Full run/execute round trip against a local Chat Completions stub:
/// events stream live as notifications, and the terminal id-matched
/// Response carries the versioned typed stop envelope.
#[test]
fn run_execute_round_trips_typed_stop_envelope() {
    let stub = SseStub::spawn(None);
    let base_url_arg = format!("base_url={}", stub.base_url);
    let mut child = DrivenChild::spawn(&[
        "--provider",
        "openai-compatible",
        "-c",
        &base_url_arg,
        "--no-session",
    ]);
    child.initialize();

    child.send(&json!({
        "jsonrpc": "2.0",
        "id": "run-1",
        "method": "run/execute",
        "params": {"prompt": "Say hello"},
    }));
    let (response, notifications) = child.read_response(&json!("run-1"));

    // Live event/* notifications preceded the terminal response.
    assert!(
        !notifications.is_empty(),
        "the run must stream event/* notifications before the result"
    );
    let methods: Vec<&str> = notifications
        .iter()
        .filter_map(|n| n["method"].as_str())
        .collect();
    // The openai-compatible provider streams text as deltas
    // (`event/progress` — the transport always forwards deltas) and closes
    // with `Done` (`event/stop`).
    assert!(methods.contains(&"event/progress"), "methods: {methods:?}");
    assert!(methods.contains(&"event/stop"), "methods: {methods:?}");
    for note in &notifications {
        assert!(note["params"]["agent_id"].is_string());
        assert!(note["params"]["agent_role"].is_string());
    }

    // The terminal result is the versioned typed stop envelope.
    let result = &response["result"];
    assert_eq!(result["envelope_version"], 1);
    assert_eq!(result["stop"]["reason"], "completed");
    assert_eq!(result["output"], "hello");
    assert_eq!(result["usage"]["input_tokens"], 7);
    assert_eq!(result["usage"]["output_tokens"], 2);
    assert!(
        result["stop"].get("retryable").is_none(),
        "retryability is the caller's judgment — never on the wire"
    );

    let status = child.finish();
    assert!(status.success(), "a completed run exits 0");
    stub.shutdown();
}

/// Regression: a post-acceptance assembly failure (bad `--output-schema`)
/// must be answered as the id-matched ERROR Response — the peer must never
/// see EOF in place of a Response. Pre-fix, `parse_output_schema` ran
/// outside the error funnel and the process died silently.
#[test]
fn assembly_failure_answers_run_execute_with_id_matched_error() {
    let mut child = DrivenChild::spawn(&["--output-schema", "{invalid-json"]);
    child.initialize();

    child.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "run/execute",
        "params": {"prompt": "go"},
    }));
    let (response, notifications) = child.read_response(&json!(5));
    assert!(notifications.is_empty(), "no run ever started");
    assert_eq!(response["error"]["code"], -32603);
    let message = response["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("argument error"),
        "the typed CLI error rides the response: {message}"
    );
    assert!(response.get("result").is_none());

    let status = child.finish();
    // The CLI exit contract still holds alongside the wire answer:
    // argument errors exit 2.
    assert_eq!(status.code(), Some(2));
}

/// Mid-run traffic at the process boundary: while the run is held in
/// flight by the gated stub, (a) an intervene/injectMessage is acked
/// id-matched, and (b) a second run/execute gets the typed -32000
/// invalid-state error (one-shot lifecycle) — then the run completes and
/// the terminal envelope still arrives last.
#[test]
fn intervene_and_second_run_are_served_mid_run() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let stub = SseStub::spawn(Some(gate_rx));
    let base_url_arg = format!("base_url={}", stub.base_url);
    let mut child = DrivenChild::spawn(&[
        "--provider",
        "openai-compatible",
        "-c",
        &base_url_arg,
        "--no-session",
    ]);
    child.initialize();

    child.send(&json!({
        "jsonrpc": "2.0",
        "id": "run-9",
        "method": "run/execute",
        "params": {"prompt": "long task"},
    }));

    // The provider call is now blocked on the gate — the run is in flight.
    // An operator injection must be dispatched and acked NOW, not after
    // the run.
    child.send(&json!({
        "jsonrpc": "2.0",
        "id": "iv-1",
        "method": "intervene/injectMessage",
        "params": {"text": "extra context", "priority": "normal"},
    }));
    let (ack, _notes) = child.read_response(&json!("iv-1"));
    assert_eq!(ack["result"]["status"], "injected");

    // A second run/execute mid-run: typed busy error, not method-not-found.
    child.send(&json!({
        "jsonrpc": "2.0",
        "id": "run-dup",
        "method": "run/execute",
        "params": {"prompt": "again"},
    }));
    let (busy, _notes) = child.read_response(&json!("run-dup"));
    assert_eq!(busy["error"]["code"], -32000);
    assert!(
        busy["error"]["message"]
            .as_str()
            .expect("busy message")
            .contains("one_shot"),
    );

    // Release the run. The injected queued turn triggers one more provider
    // iteration (the stub serves every connection), then the run completes.
    gate_tx.send(()).expect("release gate");

    let (response, notifications) = child.read_response(&json!("run-9"));
    assert!(
        !notifications.is_empty(),
        "events must have streamed during the run"
    );
    assert_eq!(response["result"]["envelope_version"], 1);
    assert_eq!(response["result"]["stop"]["reason"], "completed");
    assert_eq!(response["result"]["output"], "hello");

    let status = child.finish();
    assert!(status.success());
    stub.shutdown();
}
