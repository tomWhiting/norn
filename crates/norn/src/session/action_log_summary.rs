//! Level 1 summary-line construction for the action log.
//!
//! One [`ActionLogEntry`](crate::session::action_log::ActionLogEntry) is
//! recorded per completed tool dispatch; its `summary_line` — the
//! one-line, model-readable digest produced here — is what list queries
//! scan. Blocked and errored dispatches summarise from their outcome;
//! successes get per-tool treatment for the built-in file and shell
//! tools and a generic `"{tool} success"` line otherwise.

use crate::session::action_log::Outcome;

/// Build the Level 1 summary line for a completed tool dispatch.
pub(crate) fn compute_summary(
    tool_name: &str,
    outcome: &Outcome,
    output: &serde_json::Value,
) -> String {
    match outcome {
        Outcome::Blocked { reason } => format!("{tool_name} blocked: {}", first_line(reason)),
        Outcome::Error { message } => format!("error: {}", first_line(message)),
        Outcome::Success => success_summary(tool_name, output),
    }
}

/// Per-tool success summary; unknown tools get the generic form.
fn success_summary(tool_name: &str, output: &serde_json::Value) -> String {
    match tool_name {
        "edit" => summarise_edit(output),
        "write" => summarise_write(output),
        "read" => summarise_read(output),
        "bash" => summarise_bash(output),
        _ => format!("{tool_name} success"),
    }
}

fn summarise_edit(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let added = number_field(output, "added")
        .or_else(|| number_field(output, "lines_added"))
        .unwrap_or(0);
    let removed = number_field(output, "removed")
        .or_else(|| number_field(output, "lines_removed"))
        .unwrap_or(0);
    format!("edit committed: {path} +{added}/-{removed}")
}

fn summarise_write(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let bytes = number_field(output, "bytes").unwrap_or(0);
    format!("write committed: {path} ({bytes} bytes)")
}

fn summarise_read(output: &serde_json::Value) -> String {
    let path = string_field(output, "path")
        .or_else(|| string_field(output, "file_path"))
        .unwrap_or_else(|| "<unknown>".to_owned());
    let lines = number_field(output, "lines")
        .or_else(|| number_field(output, "line_count"))
        .unwrap_or(0);
    format!("read: {path} ({lines} lines)")
}

fn summarise_bash(output: &serde_json::Value) -> String {
    let command = string_field(output, "command").unwrap_or_else(|| "<unknown>".to_owned());
    let truncated = truncate(&command, 80);
    let exit = number_field(output, "exit_code")
        .or_else(|| number_field(output, "exit"))
        .unwrap_or(0);
    format!("bash: {truncated} (exit {exit})")
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn number_field(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(serde_json::Value::as_i64)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn summary_edit_includes_path_and_diff_stats() {
        let out = serde_json::json!({ "path": "src/h.rs", "added": 12, "removed": 3 });
        let s = success_summary("edit", &out);
        assert_eq!(s, "edit committed: src/h.rs +12/-3");
    }

    #[test]
    fn summary_read_includes_path_and_line_count() {
        let out = serde_json::json!({ "path": "src/h.rs", "lines": 42 });
        let s = success_summary("read", &out);
        assert_eq!(s, "read: src/h.rs (42 lines)");
    }

    #[test]
    fn summary_bash_truncates_command() {
        let long_cmd = format!("echo {}", "x".repeat(120));
        let out = serde_json::json!({ "command": long_cmd, "exit_code": 0 });
        let s = success_summary("bash", &out);
        // Truncated marker appended, exit visible.
        assert!(s.contains("…"));
        assert!(s.ends_with("(exit 0)"));
    }

    #[test]
    fn summary_generic_fallback() {
        let s = success_summary("unknown_tool", &serde_json::json!({}));
        assert_eq!(s, "unknown_tool success");
    }

    #[test]
    fn summary_error_uses_first_line() {
        let outcome = Outcome::Error {
            message: "first line\nsecond line".to_owned(),
        };
        let s = compute_summary("edit", &outcome, &serde_json::Value::Null);
        assert_eq!(s, "error: first line");
    }

    #[test]
    fn summary_blocked_includes_tool_and_reason() {
        let outcome = Outcome::Blocked {
            reason: "policy violation".to_owned(),
        };
        let s = compute_summary("write", &outcome, &serde_json::Value::Null);
        assert_eq!(s, "write blocked: policy violation");
    }
}
