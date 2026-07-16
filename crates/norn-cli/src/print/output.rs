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
//! - [`super::stream_renderer::spawn_stream_renderer`]: a background
//!   tokio task that consumes `ProviderEvent`s arriving on the
//!   [`tokio::sync::broadcast`] channel and writes one NDJSON object per
//!   line to stdout as they arrive (the per-event payload mapping lives
//!   HERE, in [`agent_event_to_value`]). [`emit_stream_completed`]
//!   writes the final `completed` event after `run_agent_step` returns.
//!
//! On a FAILED plain-mode run the same two machine surfaces still emit a
//! terminal envelope, carrying [`StopInfo::Error`]
//! (`{"reason":"error","message":...,"class":...}`) with a minimal
//! payload — see `super::step_output::emit_error_envelope` for the
//! emission rules and the owner-ruled boundaries (argument errors and
//! torn streams stay stderr-only; driven mode answers over JSON-RPC
//! instead).
//!
//! Every formatter takes its writers as parameters (`&mut dyn Write`) so
//! tests can capture both streams without touching the process's real
//! stdout / stderr.

use std::io::Write;
use std::sync::Arc;

use norn::agent_loop::config::AgentStepResult;
use norn::integration::{DiagnosticCollector, NornDiagnostic};
use norn::provider::events::ProviderEvent;
use norn::provider::usage::Usage;
use norn::session::events::SessionEvent;
use serde::Serialize;
use serde_json::{Value, json};

/// Result of writing the final output. Errors propagate up so the
/// orchestrator can report them.
type IoResult = std::io::Result<()>;

/// The machine-stable contract version of the print / driven output
/// envelope ([`JsonEnvelope`] and the stream-json `completed` event).
/// Bumped when the envelope shape changes incompatibly, so subprocess
/// consumers can gate on it (`DRIVEN-PROTOCOL.md` "Stop envelope").
pub const ENVELOPE_VERSION: u32 = 1;

/// The typed stop information of a finished agent step: the serde-stable
/// projection of [`AgentStepResult`] that rides the output envelope.
///
/// Serialised internally tagged on `reason` (`snake_case`), so consumers
/// branch on `stop.reason` and read the variant's detail fields directly:
/// `{"reason":"timed_out","elapsed_ms":...,"iterations":...}`.
///
/// Deliberately carries NO `retryable` field: whether a stop is worth
/// retrying is the caller's judgment (budget, policy, partial usefulness),
/// not a property Norn can decide for it (`DRIVEN-PROTOCOL.md` "Stop
/// envelope").
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum StopInfo {
    /// The model produced valid structured output (or text in no-schema
    /// mode). The envelope's `output` holds the final value.
    Completed,
    /// The schema enforcement budget was exhausted without valid output.
    /// The envelope's `output` holds the best attempt, if any.
    SchemaUnreachable {
        /// Total schema-budget-consuming attempts made.
        attempts: u32,
        /// Validation errors from the final attempt.
        validation_errors: Vec<String>,
    },
    /// The optional max-iterations cap was reached.
    MaxIterations,
    /// The configured step timeout elapsed before the loop completed. The
    /// envelope's `output` holds the partial output, if any.
    TimedOut {
        /// Wall-clock milliseconds the loop ran before being cancelled.
        elapsed_ms: u128,
        /// Completed provider iterations at the moment of the timeout.
        iterations: usize,
    },
    /// The run was cancelled (operator-initiated abort).
    Cancelled,
    /// The model stopped deterministically before completing its output.
    /// The envelope's `output` holds the partial text, if any.
    Truncated {
        /// Which deterministic stop cut the response off
        /// (`max_tokens` / `content_filter`).
        truncation: &'static str,
        /// Completed provider iterations, including the truncated one.
        iterations: u32,
    },
    /// The run failed with a typed error before producing a result
    /// (provider call, auth, session persistence, I/O). Emitted by plain
    /// print mode for every post-argument-parsing failure so machine
    /// consumers receive a parseable typed stop instead of bare stderr +
    /// a non-zero exit (owner rulings 2026-07-06,
    /// `docs/reviews/2026-07-05-context-window-incident.md` "Second
    /// bug"). Never produced by [`StopInfo::from_result`] — an
    /// `AgentStepResult` has no error variant; the orchestrator builds
    /// this stop from its `PrintError` directly. The envelope stays
    /// minimal: `output` is `null`, usage is zeroed, events are empty.
    Error {
        /// Human-readable failure description: the `Display` rendering of
        /// the CLI's typed print error, class-prefixed (`agent error: …`,
        /// `auth error: …`, `I/O error: …`, `session error: …`). At most
        /// emit sites this is also the stderr line without its `norn: `
        /// prefix; two sites keep stderr wording byte-frozen from before
        /// this envelope existed and therefore diverge: the pre-runtime
        /// tokio-runtime-build failure (`norn: failed to build tokio
        /// runtime: …`, no class prefix) and the forwarded `session
        /// resume` / `session fork` resolve failures (human lines like
        /// `Session not found: …`). The message here is always the
        /// class-prefixed `PrintError` Display regardless; machine
        /// consumers correlate on `class`, never by string-matching
        /// stderr.
        message: String,
        /// Machine-stable failure class: `agent` | `auth` | `io` |
        /// `session`. Argument errors (exit 2) never reach the envelope
        /// (clap parity), so there is no `argument` class.
        class: String,
    },
}

impl StopInfo {
    /// Project an [`AgentStepResult`] onto its typed stop information.
    #[must_use]
    pub fn from_result(result: &AgentStepResult) -> Self {
        match result {
            AgentStepResult::Completed { .. } => Self::Completed,
            AgentStepResult::SchemaUnreachable {
                attempts,
                validation_errors,
                ..
            } => Self::SchemaUnreachable {
                attempts: *attempts,
                validation_errors: validation_errors.clone(),
            },
            AgentStepResult::MaxIterationsReached { .. } => Self::MaxIterations,
            AgentStepResult::TimedOut {
                elapsed,
                iterations,
                ..
            } => Self::TimedOut {
                elapsed_ms: elapsed.as_millis(),
                iterations: *iterations,
            },
            AgentStepResult::Cancelled { .. } => Self::Cancelled,
            AgentStepResult::Truncated {
                kind, iterations, ..
            } => Self::Truncated {
                truncation: kind.as_str(),
                iterations: *iterations,
            },
        }
    }
}

/// Project an [`AgentStepResult`] onto the `(output, usage)` pair the
/// envelope needs. Returns the partial output for `TimedOut` and `None`
/// for results that have no output to report.
#[must_use]
pub fn extract_output_and_usage(result: &AgentStepResult) -> (Option<Value>, Usage) {
    match result {
        AgentStepResult::Completed { output, usage, .. } => (Some(output.clone()), usage.clone()),
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            usage,
            ..
        } => (best_attempt.clone(), usage.clone()),
        AgentStepResult::MaxIterationsReached { usage, .. }
        | AgentStepResult::Cancelled { usage, .. } => (None, usage.clone()),
        AgentStepResult::TimedOut {
            partial_output,
            usage,
            ..
        } => (partial_output.clone(), usage.clone()),
        AgentStepResult::Truncated {
            partial_text,
            usage,
            ..
        } => (partial_text.clone().map(Value::String), usage.clone()),
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

/// JSON envelope written by [`render_json`] (and returned as the driven
/// `run/execute` result). The machine-stable contract is
/// `envelope_version` + the typed `stop` — see `DRIVEN-PROTOCOL.md`
/// "Stop envelope".
#[derive(Debug, Serialize)]
pub struct JsonEnvelope<'a> {
    /// Contract version of this envelope shape ([`ENVELOPE_VERSION`]).
    pub envelope_version: u32,
    /// Typed stop information: `stop.reason` plus per-variant detail.
    pub stop: &'a StopInfo,
    /// Model output value: the final output for a completed stop, the
    /// partial output (best attempt / partial text) for a non-completion
    /// stop that produced one, `null` otherwise.
    pub output: Option<&'a Value>,
    /// Token usage subset (input + output only).
    pub usage: UsageOut,
    /// Model identifier used for the call. `None` (serialised `null`)
    /// only on an error envelope for a failure that occurred before the
    /// model was resolved (pre-assembly).
    pub model: Option<&'a str>,
    /// Session ID if persistence is enabled; `null` when `--no-session`.
    pub session_id: Option<&'a str>,
    /// Session events emitted during this step.
    pub events: &'a [SessionEvent],
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

/// Serialise one agent event to its NDJSON wire line, honouring the
/// `partial` delta filter. Returns [`None`] for events with no on-wire
/// representation. Consumed by the stream renderer
/// ([`super::stream_renderer`]).
pub(crate) fn agent_event_to_ndjson(
    agent_event: &norn::provider::AgentEvent,
    partial: bool,
) -> Option<String> {
    agent_event_to_value(agent_event, partial).map(|value| value.to_string())
}

/// Map an [`AgentEvent`] onto the structured NDJSON payload `Value` that
/// [`agent_event_to_ndjson`] serialises verbatim.
///
/// This is the single source of truth for the per-event payload shape:
/// [`agent_event_to_ndjson`] simply `to_string`s the returned `Value`, so
/// the stream-json wire form is byte-identical whether it is produced here
/// or by a downstream consumer (the JSON-RPC `event/*` emitter,
/// `DRIVEN-PROTOCOL.md` "Event notifications"). Delta events are filtered
/// when `partial` is `false`, exactly as before.
///
/// Returns [`None`] for events with no on-wire representation (filtered
/// deltas, provider `Error`, serialisation failures).
#[must_use]
pub(crate) fn agent_event_to_value(
    agent_event: &norn::provider::AgentEvent,
    partial: bool,
) -> Option<Value> {
    match &agent_event.event {
        norn::provider::AgentEventKind::Provider(event) => {
            if !partial && is_delta_event(event) {
                return None;
            }
            provider_event_to_value(event)
        }
        norn::provider::AgentEventKind::Subagent(lifecycle) => subagent_event_to_value(lifecycle),
        norn::provider::AgentEventKind::Message(lifecycle) => message_event_to_value(lifecycle),
        norn::provider::AgentEventKind::UsageEstimate(estimate) => Some(json!({
            "type": "usage_estimate",
            "input_tokens": estimate.input_tokens,
        })),
        norn::provider::AgentEventKind::StreamRetry(retry) => Some(json!({
            "type": "stream_retry",
            "attempt": retry.attempt,
        })),
        norn::provider::AgentEventKind::Compaction(compaction) => {
            // Serialize the typed payload verbatim, tagged for the driven
            // protocol like the other non-provider events. A serialization
            // failure must never silently drop the event — warn like the
            // sibling `*_event_to_value` helpers.
            let mut value = match serde_json::to_value(compaction) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("failed to serialize compaction event to NDJSON: {err}");
                    return None;
                }
            };
            if let Value::Object(map) = &mut value {
                map.insert("type".to_string(), json!("compaction"));
            }
            Some(value)
        }
    }
}

/// Derive the JSON-RPC `event/*` method name for an [`AgentEvent`] from the
/// same [`AgentEventKind`] discrimination the payload mapping uses
/// (`DRIVEN-PROTOCOL.md` "Event notifications").
///
/// The mapping is intentionally coarse — it groups the fine-grained
/// [`ProviderEvent`] variants into the design's locked `event/*` method set
/// (`event/message`, `event/toolCall`, `event/toolResult`, `event/progress`,
/// `event/stop`) plus `event/raw` for anything without a dedicated method —
/// so the `method` carries the semantic category while the `params` (from
/// [`agent_event_to_value`]) carry the byte-identical native payload.
#[must_use]
pub(crate) fn agent_event_method(agent_event: &norn::provider::AgentEvent) -> &'static str {
    use norn::provider::AgentEventKind;
    use norn::provider::events::ProviderEvent;
    match &agent_event.event {
        AgentEventKind::Provider(event) => match event {
            ProviderEvent::TextComplete { .. } | ProviderEvent::ThinkingComplete { .. } => {
                "event/message"
            }
            ProviderEvent::ToolCallComplete { .. } => "event/toolCall",
            ProviderEvent::ToolResult { .. } => "event/toolResult",
            ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { .. } => "event/progress",
            ProviderEvent::Done { .. } => "event/stop",
            ProviderEvent::Compaction { .. }
            | ProviderEvent::ReasoningItemDone { .. }
            | ProviderEvent::ResponseItemDone { .. }
            | ProviderEvent::Error { .. } => "event/raw",
        },
        AgentEventKind::Message(_) => "event/message",
        AgentEventKind::UsageEstimate(_) | AgentEventKind::StreamRetry(_) => "event/progress",
        AgentEventKind::Subagent(_) | AgentEventKind::Compaction(_) => "event/raw",
    }
}

/// Translate a typed [`norn::provider::SubagentLifecycle`] event into the
/// NDJSON payload `Value`: the event's stable serde form (`snake_case`
/// `phase` / `kind` tags) under `"type": "subagent_started"` /
/// `"subagent_completed"`.
fn subagent_event_to_value(lifecycle: &norn::provider::SubagentLifecycle) -> Option<Value> {
    let type_label = match lifecycle {
        norn::provider::SubagentLifecycle::Started { .. } => "subagent_started",
        norn::provider::SubagentLifecycle::Completed { .. } => "subagent_completed",
    };
    let mut value = match serde_json::to_value(lifecycle) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("failed to serialize subagent lifecycle event to NDJSON: {err}");
            return None;
        }
    };
    if let Some(obj) = value.as_object_mut() {
        obj.remove("phase");
        obj.insert("type".to_owned(), json!(type_label));
    }
    Some(value)
}

/// Translate a typed [`norn::provider::AgentMessageLifecycle`] event
/// into the NDJSON payload `Value`: the event's stable serde form under
/// `"type": "agent_message_sent"` / `"agent_message_delivered"`.
fn message_event_to_value(lifecycle: &norn::provider::AgentMessageLifecycle) -> Option<Value> {
    let type_label = match lifecycle {
        norn::provider::AgentMessageLifecycle::Sent { .. } => "agent_message_sent",
        norn::provider::AgentMessageLifecycle::Delivered { .. } => "agent_message_delivered",
    };
    let mut value = match serde_json::to_value(lifecycle) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("failed to serialize agent message event to NDJSON: {err}");
            return None;
        }
    };
    if let Some(obj) = value.as_object_mut() {
        obj.remove("phase");
        obj.insert("type".to_owned(), json!(type_label));
    }
    Some(value)
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

/// Translate a single [`ProviderEvent`] into the NDJSON payload `Value`
/// documented in NC18. Returns [`None`] for variants that are not surfaced
/// on the wire (e.g. `Error` is reported via the agent-error exit path).
fn provider_event_to_value(event: &ProviderEvent) -> Option<Value> {
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
            call_id,
            name,
            arguments_delta,
            kind,
        } => json!({
            "type": "tool_call_delta",
            "item_id": item_id,
            // C7: the `call_id` (`call_*`) an embedder correlates live tool
            // input against — `null` when the provider has not surfaced it yet
            // (Anthropic input fragments); always present on the Responses path.
            "call_id": call_id,
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
        // Structured reasoning items exist for provider-side replay; the
        // display text is already surfaced through the thinking events.
        ProviderEvent::ReasoningItemDone { item } => json!({
            "type": "reasoning_item",
            "item": item,
        }),
        ProviderEvent::ResponseItemDone { item } => json!({
            "type": "response_item",
            "item": item,
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
    Some(value)
}

/// Emit the `completed` NDJSON line plus any collected diagnostics.
///
/// Per NC-003 R8: diagnostics are emitted as `{"type":"diagnostic",...}`
/// events BEFORE the final `completed` event. The `completed` line carries
/// the same `envelope_version` + typed `stop` contract as [`JsonEnvelope`]
/// (`DRIVEN-PROTOCOL.md` "Stop envelope").
///
/// # Errors
///
/// Returns any I/O error from the writer.
pub fn emit_stream_completed<W: Write>(
    stdout: &mut W,
    output: Option<&Value>,
    usage: &Usage,
    stop: &StopInfo,
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

    let stop_value = serde_json::to_value(stop).map_err(std::io::Error::other)?;
    let completed = json!({
        "type": "completed",
        "envelope_version": ENVELOPE_VERSION,
        "stop": stop_value,
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

    /// Test-only shim preserving the pre-refactor `String`-returning shape
    /// over [`provider_event_to_value`]: the wire form is `Value::to_string`.
    fn provider_event_to_ndjson(event: &ProviderEvent) -> Option<String> {
        provider_event_to_value(event).map(|value| value.to_string())
    }

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
            envelope_version: ENVELOPE_VERSION,
            stop: &StopInfo::Completed,
            output: Some(&output),
            usage: UsageOut::from(&usage),
            model: Some("gpt-5"),
            session_id: Some("abc"),
            events: &[],
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
        assert!(object.contains_key("stop"));
        assert!(object.contains_key("diagnostics"));
        assert_eq!(object["envelope_version"].as_u64(), Some(1));
        assert_eq!(object["stop"]["reason"].as_str(), Some("completed"));
        assert_eq!(object["usage"]["input_tokens"].as_u64(), Some(42));
        assert_eq!(object["usage"]["output_tokens"].as_u64(), Some(17));
        assert_eq!(object["session_id"].as_str(), Some("abc"));
    }

    #[test]
    fn json_envelope_no_session_serialises_session_id_as_null() {
        let usage = Usage::default();
        let envelope = JsonEnvelope {
            envelope_version: ENVELOPE_VERSION,
            stop: &StopInfo::Completed,
            output: None,
            usage: UsageOut::from(&usage),
            model: Some("gpt-5"),
            session_id: None,
            events: &[],
            diagnostics: &[],
        };
        let mut stdout = Vec::new();
        render_json(&mut stdout, &envelope).unwrap();
        let parsed: Value =
            serde_json::from_str(String::from_utf8(stdout).unwrap().trim_end()).unwrap();
        assert!(parsed["session_id"].is_null());
    }

    /// A non-completion stop rides the envelope as the TYPED `stop` object
    /// with its detail fields and the partial output under `output` — the
    /// machine-stable contract a subprocess consumer branches on. Pre-fix,
    /// the envelope carried only a lossy `result` string label.
    #[test]
    fn json_envelope_carries_typed_stop_with_partial_for_non_completion() {
        let usage = Usage {
            input_tokens: 9,
            output_tokens: 5,
            ..Usage::default()
        };
        let result = AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_millis(2500),
            iterations: 3,
            partial_output: Some(json!("half done")),
            usage,
            children_usage: Usage::default(),
        };
        let stop = StopInfo::from_result(&result);
        let (output, usage) = extract_output_and_usage(&result);
        let envelope = JsonEnvelope {
            envelope_version: ENVELOPE_VERSION,
            stop: &stop,
            output: output.as_ref(),
            usage: UsageOut::from(&usage),
            model: Some("gpt-5"),
            session_id: None,
            events: &[],
            diagnostics: &[],
        };
        let mut stdout = Vec::new();
        render_json(&mut stdout, &envelope).unwrap();
        let parsed: Value =
            serde_json::from_str(String::from_utf8(stdout).unwrap().trim_end()).unwrap();
        assert_eq!(parsed["stop"]["reason"].as_str(), Some("timed_out"));
        assert_eq!(parsed["stop"]["elapsed_ms"].as_u64(), Some(2500));
        assert_eq!(parsed["stop"]["iterations"].as_u64(), Some(3));
        assert_eq!(parsed["output"].as_str(), Some("half done"));
        assert_eq!(parsed["usage"]["input_tokens"].as_u64(), Some(9));
        assert!(
            parsed["stop"].get("retryable").is_none(),
            "retryability is the caller's judgment — never encoded"
        );
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
            &StopInfo::Completed,
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
        assert_eq!(second["envelope_version"].as_u64(), Some(1));
        assert_eq!(second["stop"]["reason"].as_str(), Some("completed"));
    }

    /// The stream-json `completed` line carries the SAME typed stop
    /// contract as the JSON envelope — applied consistently across both
    /// output surfaces.
    #[test]
    fn emit_stream_completed_carries_typed_stop_for_truncation() {
        let mut stdout = Vec::new();
        let result = AgentStepResult::Truncated {
            kind: norn::agent_loop::config::TruncationKind::ContentFilter,
            partial_text: Some("cut".to_owned()),
            iterations: 2,
            usage: Usage::default(),
            children_usage: Usage::default(),
        };
        let stop = StopInfo::from_result(&result);
        let (output, usage) = extract_output_and_usage(&result);
        emit_stream_completed(&mut stdout, output.as_ref(), &usage, &stop, &[]).unwrap();
        let text = String::from_utf8(stdout).unwrap();
        let parsed: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(parsed["stop"]["reason"].as_str(), Some("truncated"));
        assert_eq!(
            parsed["stop"]["truncation"].as_str(),
            Some("content_filter")
        );
        assert_eq!(parsed["stop"]["iterations"].as_u64(), Some(2));
        assert_eq!(parsed["output"].as_str(), Some("cut"));
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
            call_id: Some("call_1".to_owned()),
            name: Some("read".to_owned()),
            arguments_delta: "{\"path\":\"".to_owned(),
            kind: norn::provider::request::ToolCallKind::Function,
        };
        let line = provider_event_to_ndjson(&event).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("tool_call_delta"));
        assert_eq!(parsed["item_id"].as_str(), Some("fc_1"));
        // C7: the correlation id an embedder needs to match live tool input.
        assert_eq!(parsed["call_id"].as_str(), Some("call_1"));
        assert_eq!(parsed["name"].as_str(), Some("read"));
    }

    #[test]
    fn usage_estimate_event_serialises_for_stream_json() {
        let event = norn::provider::AgentEvent {
            agent_id: uuid::Uuid::nil(),
            agent_role: Arc::from("root"),
            event: norn::provider::AgentEventKind::UsageEstimate(
                norn::provider::AgentUsageEstimate {
                    input_tokens: 12_345,
                },
            ),
        };
        let line = agent_event_to_ndjson(&event, false).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("usage_estimate"));
        assert_eq!(parsed["input_tokens"].as_u64(), Some(12_345));
    }

    /// Every [`AgentStepResult`] variant maps onto its typed stop with the
    /// stable `snake_case` `reason` tag and the variant's detail fields.
    #[test]
    fn stop_info_maps_each_variant_with_detail() {
        fn stop_json(result: &AgentStepResult) -> Value {
            serde_json::to_value(StopInfo::from_result(result)).unwrap()
        }

        let completed = stop_json(&AgentStepResult::Completed {
            output: json!(null),
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(completed, json!({"reason": "completed"}));

        let schema = stop_json(&AgentStepResult::SchemaUnreachable {
            best_attempt: None,
            validation_errors: vec!["missing field `name`".to_owned()],
            attempts: 4,
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(schema["reason"], json!("schema_unreachable"));
        assert_eq!(schema["attempts"], json!(4));
        assert_eq!(schema["validation_errors"], json!(["missing field `name`"]));

        let max_iter = stop_json(&AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(max_iter, json!({"reason": "max_iterations"}));

        let timed_out = stop_json(&AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_secs(2),
            iterations: 7,
            partial_output: None,
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(timed_out["reason"], json!("timed_out"));
        assert_eq!(timed_out["elapsed_ms"], json!(2000));
        assert_eq!(timed_out["iterations"], json!(7));

        let cancelled = stop_json(&AgentStepResult::Cancelled {
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(cancelled, json!({"reason": "cancelled"}));

        let truncated = stop_json(&AgentStepResult::Truncated {
            kind: norn::agent_loop::config::TruncationKind::MaxTokens,
            partial_text: None,
            iterations: 1,
            usage: Usage::default(),
            children_usage: Usage::default(),
        });
        assert_eq!(truncated["reason"], json!("truncated"));
        assert_eq!(truncated["truncation"], json!("max_tokens"));
        assert_eq!(truncated["iterations"], json!(1));
    }

    /// The error stop serialises under the same internally-tagged
    /// contract as every other reason: consumers branch on
    /// `stop.reason == "error"` and read `message` + `class` directly
    /// (owner ruling R1: adding a reason is additive, the envelope
    /// version stays 1).
    #[test]
    fn stop_info_error_serialises_reason_message_and_class() {
        let stop = StopInfo::Error {
            message: "agent error: connection refused".to_owned(),
            class: "agent".to_owned(),
        };
        let value = serde_json::to_value(&stop).unwrap();
        assert_eq!(
            value,
            json!({
                "reason": "error",
                "message": "agent error: connection refused",
                "class": "agent",
            })
        );
    }

    /// An error envelope for a pre-assembly failure carries `model: null`
    /// — the only stop reason where the model can be unresolved.
    #[test]
    fn json_envelope_error_stop_with_unresolved_model_serialises_null() {
        let usage = Usage::default();
        let stop = StopInfo::Error {
            message: "auth error: missing key".to_owned(),
            class: "auth".to_owned(),
        };
        let envelope = JsonEnvelope {
            envelope_version: ENVELOPE_VERSION,
            stop: &stop,
            output: None,
            usage: UsageOut::from(&usage),
            model: None,
            session_id: None,
            events: &[],
            diagnostics: &[],
        };
        let mut stdout = Vec::new();
        render_json(&mut stdout, &envelope).unwrap();
        let parsed: Value =
            serde_json::from_str(String::from_utf8(stdout).unwrap().trim_end()).unwrap();
        assert_eq!(parsed["stop"]["reason"], json!("error"));
        assert_eq!(parsed["stop"]["class"], json!("auth"));
        assert!(parsed["model"].is_null());
        assert!(parsed["output"].is_null());
        assert!(parsed["session_id"].is_null());
    }

    /// `TimedOut` and `Truncated` carry real usage and partial output on
    /// the envelope projection — neither is zeroed or dropped.
    #[test]
    fn extract_output_and_usage_covers_timed_out_and_truncated() {
        let usage = Usage {
            input_tokens: 21,
            output_tokens: 8,
            ..Usage::default()
        };
        let (output, extracted) = extract_output_and_usage(&AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_secs(3),
            iterations: 2,
            partial_output: Some(json!("half done")),
            usage: usage.clone(),
            children_usage: Usage::default(),
        });
        assert_eq!(output, Some(json!("half done")));
        assert_eq!(extracted.input_tokens, 21);

        let (output, extracted) = extract_output_and_usage(&AgentStepResult::Truncated {
            kind: norn::agent_loop::config::TruncationKind::ContentFilter,
            partial_text: Some("cut short".to_string()),
            iterations: 1,
            usage,
            children_usage: Usage::default(),
        });
        assert_eq!(output, Some(json!("cut short")));
        assert_eq!(extracted.output_tokens, 8);
    }
}
