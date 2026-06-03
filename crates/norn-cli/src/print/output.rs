//! Output formatters for print-mode execution (NC-003 R6 / R7 / R8).
//!
//! Three output formats, each with a distinct contract:
//!
//! - [`render_text`]: human-readable text. Final output to stdout,
//!   pretty-printed when the model produced structured JSON. Tool /
//!   progress text and diagnostics on stderr.
//! - [`render_json`]: a single JSON envelope on stdout (NC18) holding
//!   `output`, `usage`, `model`, `session_id`, `events`, `result`,
//!   `diagnostics`.
//! - [`spawn_stream_renderer`]: a background tokio task that consumes
//!   `ProviderEvent`s arriving on the [`tokio::sync::broadcast`] channel
//!   and writes one NDJSON object per line to stdout as they arrive.
//!   [`emit_stream_completed`] writes the final `completed` event after
//!   `run_agent_step` returns.
//!
//! Every formatter takes its writers as parameters (`&mut dyn Write`) so
//! tests can capture both streams without touching the process's real
//! stdout / stderr.

use std::io::Write;
use std::sync::Arc;

use norn::integration::{DiagnosticCollector, NornDiagnostic};
use norn::r#loop::config::AgentStepResult;
use norn::provider::events::ProviderEvent;
use norn::provider::usage::Usage;
use norn::session::events::SessionEvent;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

/// Result of writing the final output. Errors propagate up so the
/// orchestrator can report them.
type IoResult = std::io::Result<()>;

/// Map an [`AgentStepResult`] onto its NC18 result-string label.
#[must_use]
pub fn result_label(result: &AgentStepResult) -> &'static str {
    match result {
        AgentStepResult::Completed { .. } => "completed",
        AgentStepResult::SchemaUnreachable { .. } => "schema_unreachable",
        AgentStepResult::MaxIterationsReached { .. } => "max_iterations",
        AgentStepResult::TimedOut { .. } => "timed_out",
        AgentStepResult::Cancelled { .. } => "cancelled",
    }
}

/// Project an [`AgentStepResult`] onto the `(output, usage)` pair the
/// envelope needs. Returns the partial output for `TimedOut` and `None`
/// for results that have no output to report.
#[must_use]
pub fn extract_output_and_usage(result: &AgentStepResult) -> (Option<Value>, Usage) {
    match result {
        AgentStepResult::Completed { output, usage } => (Some(output.clone()), usage.clone()),
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            usage,
            ..
        } => (best_attempt.clone(), usage.clone()),
        AgentStepResult::MaxIterationsReached { usage } | AgentStepResult::Cancelled { usage } => {
            (None, usage.clone())
        }
        AgentStepResult::TimedOut { partial_output, .. } => {
            (partial_output.clone(), Usage::default())
        }
    }
}

/// Render the text-mode output (NC-003 R6).
///
/// Writes the final model output to `stdout`. When the value is a JSON
/// string, the unquoted string is written verbatim; otherwise the value
/// is pretty-printed.
///
/// Diagnostics are written to `stderr` unless `quiet` is set. Each
/// diagnostic carries severity, code, message, and (when present) the
/// suggestion line.
///
/// # Errors
///
/// Returns any I/O error from the underlying writers.
pub fn render_text<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    output: Option<&Value>,
    diagnostics: &[NornDiagnostic],
    quiet: bool,
) -> IoResult {
    if let Some(value) = output {
        write_output_value(stdout, value)?;
    }
    if !quiet {
        for diag in diagnostics {
            write_diagnostic_line(stderr, diag)?;
        }
    }
    Ok(())
}

fn write_output_value<W: Write>(stdout: &mut W, value: &Value) -> IoResult {
    match value {
        Value::String(s) => {
            stdout.write_all(s.as_bytes())?;
            // Match println! behaviour so text-mode output ends with a
            // trailing newline, matching what a user expects from a CLI.
            if !s.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
        }
        other => {
            let pretty = serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string());
            stdout.write_all(pretty.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn write_diagnostic_line<E: Write>(stderr: &mut E, diag: &NornDiagnostic) -> IoResult {
    let severity = match diag.severity {
        norn::integration::DiagnosticSeverity::Error => "error",
        norn::integration::DiagnosticSeverity::Warning => "warning",
        norn::integration::DiagnosticSeverity::Info => "info",
        norn::integration::DiagnosticSeverity::Hint => "hint",
    };
    writeln!(
        stderr,
        "{severity}: [{code}] {message}",
        code = diag.code,
        message = diag.message
    )?;
    if let Some(suggestion) = &diag.suggestion {
        writeln!(stderr, "  suggestion: {suggestion}")?;
    }
    Ok(())
}

/// Token usage reported in the JSON envelope.
#[derive(Debug, Serialize)]
pub struct UsageOut {
    /// Number of input tokens consumed across the step.
    pub input_tokens: u64,
    /// Number of output tokens produced across the step.
    pub output_tokens: u64,
    /// Tokens served from the provider's prompt cache.
    pub cache_read_tokens: u64,
    /// Tokens written into the provider's prompt cache.
    pub cache_write_tokens: u64,
    /// Estimated cost in USD, if the provider reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl From<&Usage> for UsageOut {
    fn from(usage: &Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cost_usd: usage.cost_usd,
        }
    }
}

/// JSON envelope written by [`render_json`].
#[derive(Debug, Serialize)]
pub struct JsonEnvelope<'a> {
    /// Final model output value (may be `null` for non-completed results).
    pub output: Option<&'a Value>,
    /// Token usage subset (input + output only).
    pub usage: UsageOut,
    /// Model identifier used for the call.
    pub model: &'a str,
    /// Session ID if persistence is enabled; `null` when `--no-session`.
    pub session_id: Option<&'a str>,
    /// Session events emitted during this step.
    pub events: &'a [SessionEvent],
    /// Result label: `completed` / `schema_unreachable` / `max_iterations` / `timed_out`.
    pub result: &'static str,
    /// Diagnostics collected during the step.
    pub diagnostics: &'a [NornDiagnostic],
}

/// Render the JSON envelope (NC-003 R7) to `stdout`.
///
/// # Errors
///
/// Returns any I/O error from the writer.
pub fn render_json<W: Write>(stdout: &mut W, envelope: &JsonEnvelope<'_>) -> IoResult {
    let body = serde_json::to_string(envelope).map_err(std::io::Error::other)?;
    stdout.write_all(body.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}

/// Spawn the streaming renderer for `stream-json` mode (NC-003 R8).
///
/// Subscribes to `tx`, then writes one NDJSON object per line to stdout
/// for every [`ProviderEvent`]. The task exits when the broadcast sender
/// is dropped or when the receiver lags too far (best-effort —
/// downstream pipes may miss events; the brief accepts this trade-off).
///
/// When `partial` is `false` (the default), only complete events are
/// emitted: `text`, `thinking`, `tool_call`, `tool_result`, `done`.
/// Delta events (`text_delta`, `thinking_delta`, `tool_call_delta`) are
/// silently consumed. When `partial` is `true`, all events are emitted.
///
/// Returns the [`tokio::task::JoinHandle`] so the caller can `await` the
/// renderer after `run_agent_step` finishes, ensuring every event has
/// been written before the `completed` line is emitted.
#[must_use]
pub fn spawn_stream_renderer(
    tx: &tokio::sync::broadcast::Sender<norn::provider::AgentEvent>,
    partial: bool,
) -> tokio::task::JoinHandle<()> {
    let mut rx = tx.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(agent_event) => {
                    let event = &agent_event.event;
                    if !partial && is_delta_event(event) {
                        continue;
                    }
                    if let Some(line) = provider_event_to_ndjson(event) {
                        let mut stdout = std::io::stdout().lock();
                        if stdout.write_all(line.as_bytes()).is_err()
                            || stdout.write_all(b"\n").is_err()
                            || stdout.flush().is_err()
                        {
                            return;
                        }
                    }
                }
                Err(RecvError::Closed) => return,
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "stream renderer lagged — {n} events dropped");
                }
            }
        }
    })
}

fn is_delta_event(event: &ProviderEvent) -> bool {
    matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { .. }
    )
}

fn stop_reason_label(reason: &norn::provider::events::StopReason) -> &'static str {
    match reason {
        norn::provider::events::StopReason::EndTurn => "end_turn",
        norn::provider::events::StopReason::ToolUse => "tool_use",
        norn::provider::events::StopReason::MaxTokens => "max_tokens",
        norn::provider::events::StopReason::ContentFilter => "content_filter",
    }
}

/// Translate a single [`ProviderEvent`] into the NDJSON line documented
/// in NC18. Returns [`None`] for variants that are not surfaced on the
/// wire (e.g. `Error` is reported via the agent-error exit path).
fn provider_event_to_ndjson(event: &ProviderEvent) -> Option<String> {
    let value = match event {
        ProviderEvent::TextDelta { text } => json!({
            "type": "text_delta",
            "text": text,
        }),
        ProviderEvent::ThinkingDelta { text } => json!({
            "type": "thinking_delta",
            "text": text,
        }),
        ProviderEvent::ToolCallDelta {
            item_id,
            name,
            arguments_delta,
            kind,
        } => json!({
            "type": "tool_call_delta",
            "item_id": item_id,
            "name": name,
            "arguments_delta": arguments_delta,
            "kind": kind,
        }),
        ProviderEvent::TextComplete { text } => json!({
            "type": "text",
            "text": text,
        }),
        ProviderEvent::ThinkingComplete { text } => json!({
            "type": "thinking",
            "text": text,
        }),
        ProviderEvent::ToolCallComplete {
            call_id,
            name,
            arguments,
            kind,
        } => {
            let args: Value =
                serde_json::from_str(arguments).unwrap_or(Value::String(arguments.clone()));
            json!({
                "type": "tool_call",
                "call_id": call_id,
                "name": name,
                "arguments": args,
                "kind": kind,
            })
        }
        ProviderEvent::ToolResult {
            tool_call_id,
            tool_name,
            output,
            duration_ms,
        } => json!({
            "type": "tool_result",
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "output": output,
            "duration_ms": duration_ms,
        }),
        ProviderEvent::Compaction {
            item_type,
            encrypted_content,
        } => json!({
            "type": "compaction",
            "item_type": item_type,
            "encrypted_content": encrypted_content,
        }),
        ProviderEvent::Done {
            stop_reason,
            usage,
            response_id,
        } => {
            let mut obj = json!({
                "type": "done",
                "stop_reason": stop_reason_label(stop_reason),
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_write_tokens": usage.cache_write_tokens,
                },
            });
            if let Some(cost) = usage.cost_usd {
                obj["usage"]["cost_usd"] = json!(cost);
            }
            if let Some(rid) = response_id {
                obj["response_id"] = json!(rid);
            }
            obj
        }
        ProviderEvent::Error { .. } => return None,
    };
    match serde_json::to_string(&value) {
        Ok(s) => Some(s),
        Err(err) => {
            tracing::warn!("failed to serialize provider event to NDJSON: {err}");
            None
        }
    }
}

/// Emit the `completed` NDJSON line plus any collected diagnostics.
///
/// Per NC-003 R8: diagnostics are emitted as `{"type":"diagnostic",...}`
/// events BEFORE the final `completed` event.
///
/// # Errors
///
/// Returns any I/O error from the writer.
pub fn emit_stream_completed<W: Write>(
    stdout: &mut W,
    output: Option<&Value>,
    usage: &Usage,
    result_label: &'static str,
    diagnostics: &[NornDiagnostic],
) -> IoResult {
    for diag in diagnostics {
        let value = serde_json::to_value(diag).map_err(std::io::Error::other)?;
        let mut object = match value {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("value".to_owned(), other);
                map
            }
        };
        object.insert("type".to_owned(), Value::String("diagnostic".to_owned()));
        let body = serde_json::to_string(&Value::Object(object)).map_err(std::io::Error::other)?;
        stdout.write_all(body.as_bytes())?;
        stdout.write_all(b"\n")?;
    }

    let completed = json!({
        "type": "completed",
        "result": result_label,
        "output": output,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
        },
    });
    let body = serde_json::to_string(&completed).map_err(std::io::Error::other)?;
    stdout.write_all(body.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// Drain the collector and clone its contents for downstream rendering.
///
/// Wraps [`DiagnosticCollector::drain`] so consumers do not have to
/// import the libnorn type directly.
#[must_use]
pub fn drain_diagnostics(collector: &Arc<DiagnosticCollector>) -> Vec<NornDiagnostic> {
    collector.drain()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use norn::integration::DiagnosticSeverity;
    use serde_json::json;

    fn diag_warning() -> NornDiagnostic {
        NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "schema-violation".to_owned(),
            message: "missing required field 'name'".to_owned(),
            source_tool: None,
            file_path: None,
            suggestion: Some("add 'name' to the output".to_owned()),
        }
    }

    #[test]
    fn render_text_writes_string_output_without_quotes() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let output = json!("hello there");
        render_text(&mut stdout, &mut stderr, Some(&output), &[], false).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert_eq!(text, "hello there\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn render_text_pretty_prints_structured_output() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let output = json!({"name": "n", "items": [1, 2]});
        render_text(&mut stdout, &mut stderr, Some(&output), &[], false).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.contains("\"name\""));
        assert!(text.contains("\"items\""));
        // Must be multi-line — pretty-printed JSON.
        assert!(text.lines().count() > 1);
    }

    #[test]
    fn render_text_diagnostics_appear_on_stderr() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let output = json!("ok");
        render_text(
            &mut stdout,
            &mut stderr,
            Some(&output),
            &[diag_warning()],
            false,
        )
        .unwrap();
        let err_text = String::from_utf8(stderr).unwrap();
        assert!(err_text.contains("warning"));
        assert!(err_text.contains("schema-violation"));
        assert!(err_text.contains("missing required field"));
        assert!(err_text.contains("suggestion: add"));
    }

    #[test]
    fn render_text_quiet_suppresses_stderr() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let output = json!("ok");
        render_text(
            &mut stdout,
            &mut stderr,
            Some(&output),
            &[diag_warning()],
            true,
        )
        .unwrap();
        assert!(stderr.is_empty(), "stderr must be empty with quiet=true");
    }

    #[test]
    fn render_text_omits_output_when_none() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        render_text(&mut stdout, &mut stderr, None, &[], false).unwrap();
        assert!(stdout.is_empty());
    }

    #[test]
    fn json_envelope_contains_all_required_fields() {
        let usage = Usage {
            input_tokens: 42,
            output_tokens: 17,
            ..Usage::default()
        };
        let output = json!({"answer": 42});
        let envelope = JsonEnvelope {
            output: Some(&output),
            usage: UsageOut::from(&usage),
            model: "gpt-5",
            session_id: Some("abc"),
            events: &[],
            result: "completed",
            diagnostics: &[],
        };
        let mut stdout = Vec::new();
        render_json(&mut stdout, &envelope).unwrap();
        let line = String::from_utf8(stdout).unwrap();
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        let object = parsed.as_object().unwrap();
        assert!(object.contains_key("output"));
        assert!(object.contains_key("usage"));
        assert!(object.contains_key("model"));
        assert!(object.contains_key("session_id"));
        assert!(object.contains_key("events"));
        assert!(object.contains_key("result"));
        assert!(object.contains_key("diagnostics"));
        assert_eq!(object["result"].as_str(), Some("completed"));
        assert_eq!(object["usage"]["input_tokens"].as_u64(), Some(42));
        assert_eq!(object["usage"]["output_tokens"].as_u64(), Some(17));
        assert_eq!(object["session_id"].as_str(), Some("abc"));
    }

    #[test]
    fn json_envelope_no_session_serialises_session_id_as_null() {
        let usage = Usage::default();
        let envelope = JsonEnvelope {
            output: None,
            usage: UsageOut::from(&usage),
            model: "gpt-5",
            session_id: None,
            events: &[],
            result: "completed",
            diagnostics: &[],
        };
        let mut stdout = Vec::new();
        render_json(&mut stdout, &envelope).unwrap();
        let parsed: Value =
            serde_json::from_str(String::from_utf8(stdout).unwrap().trim_end()).unwrap();
        assert!(parsed["session_id"].is_null());
    }

    #[test]
    fn emit_stream_completed_appends_diagnostic_then_completed() {
        let mut stdout = Vec::new();
        let usage = Usage {
            input_tokens: 3,
            output_tokens: 4,
            ..Usage::default()
        };
        let output = json!("done");
        emit_stream_completed(
            &mut stdout,
            Some(&output),
            &usage,
            "completed",
            &[diag_warning()],
        )
        .unwrap();
        let text = String::from_utf8(stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "expected diagnostic + completed lines");
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"].as_str(), Some("diagnostic"));
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["type"].as_str(), Some("completed"));
        assert_eq!(second["result"].as_str(), Some("completed"));
    }

    #[test]
    fn provider_event_text_delta_serialises_correctly() {
        let event = ProviderEvent::TextDelta {
            text: "hello".to_owned(),
        };
        let line = provider_event_to_ndjson(&event).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("text_delta"));
        assert_eq!(parsed["text"].as_str(), Some("hello"));
    }

    #[test]
    fn provider_event_thinking_delta_serialises_correctly() {
        let event = ProviderEvent::ThinkingDelta {
            text: "let me think".to_owned(),
        };
        let line = provider_event_to_ndjson(&event).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("thinking_delta"));
        assert_eq!(parsed["text"].as_str(), Some("let me think"));
    }

    #[test]
    fn provider_event_tool_call_delta_includes_item_id_name_arguments() {
        let event = ProviderEvent::ToolCallDelta {
            item_id: "fc_1".to_owned(),
            name: Some("read".to_owned()),
            arguments_delta: "{\"path\":\"".to_owned(),
            kind: norn::provider::request::ToolCallKind::Function,
        };
        let line = provider_event_to_ndjson(&event).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("tool_call_delta"));
        assert_eq!(parsed["item_id"].as_str(), Some("fc_1"));
        assert_eq!(parsed["name"].as_str(), Some("read"));
    }

    #[test]
    fn result_label_maps_each_variant() {
        assert_eq!(
            result_label(&AgentStepResult::Completed {
                output: json!(null),
                usage: Usage::default(),
            }),
            "completed"
        );
        assert_eq!(
            result_label(&AgentStepResult::SchemaUnreachable {
                best_attempt: None,
                validation_errors: vec![],
                attempts: 0,
                usage: Usage::default(),
            }),
            "schema_unreachable"
        );
        assert_eq!(
            result_label(&AgentStepResult::MaxIterationsReached {
                usage: Usage::default(),
            }),
            "max_iterations"
        );
        assert_eq!(
            result_label(&AgentStepResult::TimedOut {
                elapsed: std::time::Duration::ZERO,
                iterations: 0,
                partial_output: None,
            }),
            "timed_out"
        );
        assert_eq!(
            result_label(&AgentStepResult::Cancelled {
                usage: Usage::default(),
            }),
            "cancelled"
        );
    }
}
