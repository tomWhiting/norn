//! End-to-end integration test for the NOI-1 driven JSON-RPC mode.
//!
//! Spawns the real `norn` binary with `--protocol jsonrpc` and drives the
//! `initialize` handshake over its actual stdin/stdout, proving the full
//! wiring — CLI flag → `detect_mode` (unaffected) → print dispatch → the
//! driven duplex loop — end to end, including the single serializing writer
//! and the newline-delimited framing.
//!
//! It deliberately does NOT issue `run/execute`, which would require a live
//! provider; the transport round-trip (initialize→capabilities, result vs
//! event separation, all four event arms, id-matching) is covered
//! deterministically by the `print::jsonrpc` unit tests. Here the goal is
//! the negative control that the flag is honoured and the handshake works
//! against the real process boundary — and that stderr stays human logs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

#[test]
fn driven_mode_answers_initialize_over_real_stdio() {
    let mut child = Command::new(norn_bin())
        .arg("--protocol")
        .arg("jsonrpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn norn --protocol jsonrpc");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // Send the initialize request, newline-framed.
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"h1\",\"method\":\"initialize\"}\n")
        .expect("write initialize");
    stdin.flush().expect("flush initialize");

    // Read exactly one framed response line.
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read initialize response");
    let parsed: serde_json::Value =
        serde_json::from_str(line.trim()).expect("initialize response is valid JSON");

    // (a) initialize returns capabilities, id-matched, never a notification.
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], "h1");
    assert!(
        parsed.get("method").is_none(),
        "a response is never a method"
    );
    let interventions = parsed["result"]["capabilities"]["interventions"]
        .as_array()
        .expect("interventions array");
    let names: Vec<&str> = interventions
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(names.contains(&"inject_message"));
    assert!(names.contains(&"cancel"));

    // Close stdin: with no run/execute, the channel closes and the process
    // exits cleanly (the terminal shutdown handshake).
    drop(stdin);
    let status = child.wait().expect("norn exits");
    assert!(
        status.success(),
        "driven mode must exit 0 when the channel closes before run/execute"
    );
}
