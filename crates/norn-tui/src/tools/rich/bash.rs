//! `bash` tool renderer.
//!
//! Renders `bash` tool calls: `$ {command}` headers with streaming
//! ANSI-passthrough bodies, signal-name decoding on the failure path,
//! and a duration suffix. The JSON shapes consumed here are produced by
//! `crates/norn/src/tools/bash.rs`. Field access is defensive — a
//! missing or mistyped field degrades gracefully rather than panicking.

use std::fmt::Write as _;

use serde_json::Value;

use crate::terminal::caps::TerminalCaps;
use crate::tools::helpers::{
    RED, SPINNER, dim, fg, fg_reset, format_duration_ms, partial_field, reset, truncate_preview,
};
use crate::tools::renderer::ToolRenderer;

/// POSIX signal-termination convention: a subprocess killed by signal
/// `signum` exits with code `128 + signum`. Returns the common
/// signal name for recognised codes, `None` otherwise. Only the
/// handful of signals a user is likely to see attributed to a shell
/// command are listed — extending this requires a real reason.
fn signal_name(exit_code: i64) -> Option<&'static str> {
    match exit_code {
        130 => Some("SIGINT"),
        134 => Some("SIGABRT"),
        137 => Some("SIGKILL"),
        139 => Some("SIGSEGV"),
        143 => Some("SIGTERM"),
        _ => None,
    }
}

/// Renders `bash` tool calls: `$ {command}` headers with streaming
/// ANSI-passthrough bodies, exit code, and duration.
pub struct BashRenderer;

impl ToolRenderer for BashRenderer {
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        duration_ms: u64,
        caps: &TerminalCaps,
    ) -> String {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("");
        let preview = truncate_preview(command);
        let exit_code = result.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
        let timed_out = result
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let header = format!("$ {preview}  ({})", format_duration_ms(duration_ms));
        if exit_code != 0 || timed_out {
            format!("{}{header}{}", fg(RED, caps), fg_reset())
        } else {
            header
        }
    }

    fn body(&self, _args: &Value, result: &Value, caps: &TerminalCaps) -> Option<String> {
        let stdout = result.get("stdout").and_then(Value::as_str).unwrap_or("");
        let stderr = result.get("stderr").and_then(Value::as_str).unwrap_or("");
        let exit_code = result.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
        let timed_out = result
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // ANSI passthrough: subprocess output is written verbatim so
        // colours emitted by the child process are preserved.
        let mut out = String::from(stdout);
        if !stderr.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{}── stderr ──{}", dim(), reset());
            out.push_str(stderr);
        }
        // Failure footer carries the exit code (with signal-name decoding
        // for the common signal-termination codes) since the header no
        // longer surfaces it. Silent on the success path so green output
        // is not noisier than it needs to be.
        if exit_code != 0 {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            match signal_name(exit_code) {
                Some(name) => {
                    let _ = write!(
                        out,
                        "{}exit: {exit_code} ({name}){}",
                        fg(RED, caps),
                        fg_reset(),
                    );
                }
                None => {
                    let _ = write!(out, "{}exit: {exit_code}{}", fg(RED, caps), fg_reset());
                }
            }
        }
        if timed_out {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            let _ = write!(out, "{}[timed out]{}", fg(RED, caps), fg_reset());
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn streaming_header(&self, _name: &str, partial_args: &str, _caps: &TerminalCaps) -> String {
        match partial_field(partial_args, "command") {
            Some(command) => format!("$ {}  {SPINNER}", truncate_preview(&command)),
            None => format!("$ {SPINNER}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    fn true_caps() -> TerminalCaps {
        let mut c = TerminalCaps::baseline();
        c.true_colour = true;
        c
    }

    #[test]
    fn bash_header_shows_command_and_duration_without_exit_code() {
        let header = BashRenderer.header_line(
            &json!({ "command": "echo hi" }),
            &json!({ "exit_code": 0, "stdout": "hi\n", "stderr": "", "timed_out": false }),
            420,
            &caps(),
        );
        assert!(header.contains("$ echo hi"));
        assert!(header.contains("0.42s"));
        assert!(
            !header.contains("exit="),
            "exit code must not appear in the header: {header:?}",
        );
    }

    #[test]
    fn bash_non_zero_exit_renders_red() {
        let header = BashRenderer.header_line(
            &json!({ "command": "false" }),
            &json!({ "exit_code": 7, "stdout": "", "stderr": "", "timed_out": false }),
            10,
            &caps(),
        );
        assert!(
            !header.contains("exit="),
            "exit code must not appear in the header: {header:?}",
        );
        // Baseline caps → 256-colour palette escape.
        assert!(
            header.contains("38;5;"),
            "expected palette red SGR: {header:?}"
        );

        let header_true = BashRenderer.header_line(
            &json!({ "command": "false" }),
            &json!({ "exit_code": 7, "stdout": "", "stderr": "", "timed_out": false }),
            10,
            &true_caps(),
        );
        assert!(
            header_true.contains("38;2;200;80;80"),
            "expected truecolor red SGR: {header_true:?}",
        );
    }

    #[test]
    fn bash_body_failure_appends_exit_line_in_red() {
        let body = BashRenderer
            .body(
                &json!({ "command": "x" }),
                &json!({
                    "exit_code": 1,
                    "stdout": "",
                    "stderr": "boom\n",
                    "timed_out": false,
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("exit: 1"), "expected exit line: {body:?}");
        assert!(
            body.contains("38;5;"),
            "exit line must render in red: {body:?}",
        );
    }

    #[test]
    fn bash_body_decodes_signal_termination_codes() {
        // 128 + signum convention: 130 SIGINT, 134 SIGABRT, 137 SIGKILL,
        // 139 SIGSEGV, 143 SIGTERM. Each must show name and code.
        for (code, name) in [
            (130, "SIGINT"),
            (134, "SIGABRT"),
            (137, "SIGKILL"),
            (139, "SIGSEGV"),
            (143, "SIGTERM"),
        ] {
            let body = BashRenderer
                .body(
                    &json!({ "command": "x" }),
                    &json!({
                        "exit_code": code,
                        "stdout": "",
                        "stderr": "",
                        "timed_out": false,
                    }),
                    &caps(),
                )
                .unwrap();
            assert!(
                body.contains(&format!("exit: {code} ({name})")),
                "expected signal-decoded line for exit {code} ({name}): {body:?}",
            );
        }
    }

    #[test]
    fn bash_body_success_omits_exit_line() {
        // Body returns None when there's nothing to show on the success
        // path. The exit line is failure-only.
        let body = BashRenderer.body(
            &json!({ "command": "true" }),
            &json!({ "exit_code": 0, "stdout": "", "stderr": "", "timed_out": false }),
            &caps(),
        );
        assert!(
            body.is_none(),
            "success path must produce no body: {body:?}"
        );

        // When stdout is present but exit_code is 0, body has the
        // stdout but no exit line.
        let body = BashRenderer
            .body(
                &json!({ "command": "echo hi" }),
                &json!({
                    "exit_code": 0,
                    "stdout": "hi\n",
                    "stderr": "",
                    "timed_out": false,
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("hi"));
        assert!(
            !body.contains("exit:"),
            "exit line must not appear when exit_code is 0: {body:?}",
        );
    }

    #[test]
    fn bash_body_unknown_exit_code_omits_signal_name() {
        let body = BashRenderer
            .body(
                &json!({ "command": "x" }),
                &json!({
                    "exit_code": 42,
                    "stdout": "",
                    "stderr": "",
                    "timed_out": false,
                }),
                &caps(),
            )
            .unwrap();
        // Plain `exit: 42` — no parenthesised signal name.
        assert!(body.contains("exit: 42"), "got: {body:?}");
        assert!(
            !body.contains('('),
            "no signal name should be appended for exit 42: {body:?}",
        );
    }

    #[test]
    fn bash_body_timed_out_keeps_timed_out_marker() {
        let body = BashRenderer
            .body(
                &json!({ "command": "sleep" }),
                &json!({
                    "exit_code": 124,
                    "stdout": "",
                    "stderr": "",
                    "timed_out": true,
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("exit: 124"), "got: {body:?}");
        assert!(body.contains("[timed out]"), "got: {body:?}");
    }

    #[test]
    fn bash_timed_out_renders_red_without_exit_text() {
        let header = BashRenderer.header_line(
            &json!({ "command": "sleep 99" }),
            &json!({ "exit_code": 0, "stdout": "", "stderr": "", "timed_out": true }),
            5_000,
            &caps(),
        );
        assert!(
            !header.contains("exit="),
            "exit code must not appear in the header: {header:?}",
        );
        assert!(
            header.contains("38;5;"),
            "timed-out header must render red: {header:?}",
        );
    }

    #[test]
    fn bash_body_passes_through_and_appends_stderr() {
        let body = BashRenderer
            .body(
                &json!({ "command": "x" }),
                &json!({
                    "exit_code": 1,
                    "stdout": "out line\n",
                    "stderr": "err line\n",
                    "timed_out": false,
                }),
                &caps(),
            )
            .unwrap();
        assert!(body.contains("out line"));
        assert!(body.contains("── stderr ──"));
        assert!(body.contains("err line"));
    }

    #[test]
    fn bash_streaming_header_shows_spinner() {
        assert!(
            BashRenderer
                .streaming_header("bash", "{\"command\":\"ls\"}", &caps())
                .contains('⟳'),
        );
        // Partial / invalid JSON still produces a spinner header.
        assert_eq!(
            BashRenderer.streaming_header("bash", "{\"comm", &caps()),
            "$ ⟳",
        );
    }

    #[test]
    fn bash_empty_output_returns_none_body() {
        assert!(
            BashRenderer
                .body(
                    &json!({ "command": "true" }),
                    &json!({ "exit_code": 0, "stdout": "", "stderr": "", "timed_out": false }),
                    &caps(),
                )
                .is_none(),
        );
    }
}
