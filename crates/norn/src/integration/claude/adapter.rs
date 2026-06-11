//! [`ClaudeRunnerAdapter`] — implements [`Provider`] over the Claude Code CLI.
//!
//! The adapter builds a [`ClaudeCommand`] with `--output-format stream-json`,
//! spawns a [`ClaudeProcess`], and translates its events into
//! [`ProviderEvent`]s. [`StepOutcome`] is the structured return value
//! produced by a single step.

use std::path::PathBuf;
use std::pin::Pin;

use claude_runner::events::{
    ClaudeMessage, ContentItem, StreamEvent, ToolData, Usage as ClaudeUsage,
};
use claude_runner::types::{Model, OutputFormat};
use claude_runner::{ClaudeCommand, ClaudeEvent, ClaudeProcess};
use futures_util::Stream;
use serde_json::Value;

use crate::error::{IntegrationError, ProviderError};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;

/// Configuration for [`ClaudeRunnerAdapter`].
///
/// Note: `max_tokens` is recorded for completeness, but Claude CLI exposes
/// `--max-turns` rather than `--max-tokens`; the adapter currently ignores
/// this field.
#[derive(Clone, Debug)]
pub struct ClaudeRunnerConfig {
    /// Path to the Claude CLI binary or runner script.
    pub runner_path: PathBuf,
    /// Model identifier (alias or full name) passed via `--model`.
    pub model: String,
    /// Reserved for future use — Claude CLI exposes `--max-turns`, not
    /// `--max-tokens`; the adapter currently records but ignores this value.
    pub max_tokens: Option<u32>,
}

/// Result of executing one agent step through the Claude Runner adapter.
///
/// `result` is the validated structured output from the step (pre-validated
/// by the N-005 schema mechanism); `usage` is the token usage reported by
/// Claude; `stop_reason` describes how the step terminated.
#[derive(Clone, Debug)]
pub struct StepOutcome {
    /// Pre-validated structured output of the step.
    pub result: Value,
    /// Token usage reported by Claude for this call.
    pub usage: Usage,
    /// Reason the step stopped.
    pub stop_reason: StopReason,
}

/// Provider implementation that routes requests through the Claude CLI.
///
/// `ClaudeRunnerAdapter::stream` builds a [`ClaudeCommand`] with
/// `--output-format stream-json --include-partial-messages`, spawns a
/// [`ClaudeProcess`], and forwards each line-delimited [`ClaudeEvent`] as a
/// [`ProviderEvent`].
pub struct ClaudeRunnerAdapter {
    config: ClaudeRunnerConfig,
}

impl ClaudeRunnerAdapter {
    /// Construct a new adapter with the given configuration.
    #[must_use]
    pub fn new(config: ClaudeRunnerConfig) -> Self {
        Self { config }
    }

    /// The runner binary path this adapter invokes. Exposed so callers
    /// (and their tests) can verify which binary a constructed adapter
    /// resolved — e.g. that `settings.provider.runner_path` was honored.
    #[must_use]
    pub fn runner_path(&self) -> &std::path::Path {
        &self.config.runner_path
    }

    /// Build the [`ClaudeCommand`] for one call.
    pub(crate) fn build_command(&self, request: &ProviderRequest) -> ClaudeCommand {
        let prompt = render_prompt(&request.messages);
        let system = render_system_prompt(&request.messages);

        let mut cmd = ClaudeCommand::new()
            .binary(self.config.runner_path.to_string_lossy().into_owned())
            .prompt(prompt)
            .print_mode()
            .output_format(OutputFormat::StreamJson)
            .include_partial_messages()
            .dangerously_skip_permissions();
        if !system.is_empty() {
            cmd = cmd.system_prompt(system);
        }
        let model_name = if request.model.is_empty() {
            self.config.model.clone()
        } else {
            request.model.clone()
        };
        cmd = cmd.model(Model::full(model_name));
        cmd
    }

    /// Execute one call against the adapter and return the consolidated
    /// [`StepOutcome`]. Convenience helper that consumes the stream and
    /// rolls each event up to a single result/usage pair.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::ClaudeRunnerError`] when spawning or
    /// reading the runner process fails, or when the runner reports a
    /// terminal error result.
    pub fn run_step(&self, request: &ProviderRequest) -> Result<StepOutcome, IntegrationError> {
        let cmd = self.build_command(request);
        let events = spawn_and_collect(&cmd)?;
        let outcome = consolidate_outcome(&events)?;
        Ok(outcome)
    }
}

impl Provider for ClaudeRunnerAdapter {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let cmd = self.build_command(&request);
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);

        tokio::task::spawn_blocking(move || {
            let mut process = match ClaudeProcess::spawn(&cmd) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.blocking_send(Err(ProviderError::ConnectionFailed {
                        reason: format!("failed to spawn Claude runner: {e}"),
                    }));
                    return;
                }
            };

            let mut total_usage = Usage::default();
            let mut sent_done = false;

            loop {
                match process.read_event() {
                    Ok(Some(event)) => {
                        let (events, stop) = map_claude_event(event, &mut total_usage);
                        for ev in events {
                            if tx.blocking_send(Ok(ev)).is_err() {
                                return;
                            }
                        }
                        if let Some(stop_reason) = stop {
                            let _ = tx.blocking_send(Ok(ProviderEvent::Done {
                                stop_reason,
                                usage: total_usage.clone(),
                                response_id: None,
                            }));
                            sent_done = true;
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.blocking_send(Err(ProviderError::StreamError {
                            reason: format!("read failed: {e}"),
                        }));
                        return;
                    }
                }
            }

            if !sent_done {
                let _ = tx.blocking_send(Ok(ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: total_usage,
                    response_id: None,
                }));
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

fn render_prompt(messages: &[Message]) -> String {
    let mut buf = String::new();
    for msg in messages {
        if !matches!(msg.role, MessageRole::User | MessageRole::ToolResult) {
            continue;
        }
        if let Some(content) = msg.content.as_deref() {
            if !buf.is_empty() {
                buf.push_str("\n\n");
            }
            buf.push_str(content);
        }
    }
    buf
}

fn render_system_prompt(messages: &[Message]) -> String {
    let mut buf = String::new();
    for msg in messages {
        if !matches!(msg.role, MessageRole::System) {
            continue;
        }
        if let Some(content) = msg.content.as_deref() {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(content);
        }
    }
    buf
}

/// Spawn a [`ClaudeProcess`] from the given command and collect all events
/// synchronously. Used by both the adapter and the wrapped Claude Code
/// runner.
pub(super) fn spawn_and_collect(cmd: &ClaudeCommand) -> Result<Vec<ClaudeEvent>, IntegrationError> {
    let mut process =
        ClaudeProcess::spawn(cmd).map_err(|e| IntegrationError::ClaudeRunnerError {
            reason: format!("failed to spawn Claude runner: {e}"),
        })?;
    let mut events = Vec::new();
    loop {
        match process.read_event() {
            Ok(Some(ev)) => events.push(ev),
            Ok(None) => break,
            Err(e) => {
                return Err(IntegrationError::ClaudeRunnerError {
                    reason: format!("read failed: {e}"),
                });
            }
        }
    }
    Ok(events)
}

fn consolidate_outcome(events: &[ClaudeEvent]) -> Result<StepOutcome, IntegrationError> {
    let mut result = Value::Null;
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::EndTurn;
    let mut error: Option<String> = None;

    for event in events {
        match event {
            ClaudeEvent::Result {
                is_error,
                result: r,
                error: err,
                stop_reason: sr,
                total_cost_usd,
                usage: u,
                ..
            } => {
                if let Some(r) = r {
                    result = r.clone();
                }
                if let Some(reason) = sr {
                    stop_reason = parse_stop_reason(reason);
                }
                if let Some(u) = u {
                    let mut converted = convert_usage(u);
                    if let Some(cost) = total_cost_usd {
                        converted.cost_usd = Some(*cost);
                    }
                    usage += converted;
                }
                if is_error.unwrap_or(false) {
                    error.clone_from(err);
                }
            }
            ClaudeEvent::Assistant { message, .. } => {
                usage += message_usage(message);
                if let Some(reason) = message.stop_reason.as_deref() {
                    stop_reason = parse_stop_reason(reason);
                }
            }
            ClaudeEvent::StreamEvent {
                event: StreamEvent::MessageDelta { usage: Some(u), .. },
                ..
            } => {
                usage += convert_usage(u);
            }
            _ => {}
        }
    }

    if let Some(err) = error {
        return Err(IntegrationError::ClaudeRunnerError { reason: err });
    }
    Ok(StepOutcome {
        result,
        usage,
        stop_reason,
    })
}

fn map_claude_event(
    event: ClaudeEvent,
    usage_accum: &mut Usage,
) -> (Vec<ProviderEvent>, Option<StopReason>) {
    let mut emitted = Vec::new();
    let mut stop: Option<StopReason> = None;

    match event {
        ClaudeEvent::Assistant { message, .. } => {
            *usage_accum += message_usage(&message);
            for item in &message.content {
                match item {
                    ContentItem::Text { text } => {
                        emitted.push(ProviderEvent::TextDelta { text: text.clone() });
                    }
                    ContentItem::Thinking { thinking, .. } => {
                        emitted.push(ProviderEvent::ThinkingDelta {
                            text: thinking.clone(),
                        });
                    }
                    ContentItem::ToolUse { id, tool_data } => {
                        let (name, input) = tool_data_pair(tool_data);
                        // Claude's `id` is the streaming item identifier — the
                        // same role `item_id` plays in the OpenAI Responses
                        // stream. It is used by `assemble_response` to merge
                        // deltas, and is later promoted to `call_id` on the
                        // emitted ToolCallComplete (synthesized below).
                        emitted.push(ProviderEvent::ToolCallDelta {
                            item_id: id.clone(),
                            name: Some(name),
                            arguments_delta: serde_json::to_string(&input).unwrap_or_default(),
                            kind: crate::provider::request::ToolCallKind::Function,
                        });
                    }
                    ContentItem::ToolResult { .. } => {}
                }
            }
            if let Some(reason) = message.stop_reason.as_deref() {
                stop = Some(parse_stop_reason(reason));
            }
        }
        ClaudeEvent::StreamEvent { event, .. } => match event {
            StreamEvent::ContentBlockDelta { delta, .. } => {
                if let Some(text) = delta.text() {
                    emitted.push(ProviderEvent::TextDelta {
                        text: text.to_owned(),
                    });
                } else if let Some(thinking) = delta.thinking() {
                    emitted.push(ProviderEvent::ThinkingDelta {
                        text: thinking.to_owned(),
                    });
                } else if let Some(partial) = delta.partial_json() {
                    emitted.push(ProviderEvent::ToolCallDelta {
                        item_id: String::new(),
                        name: None,
                        arguments_delta: partial.to_owned(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    });
                }
            }
            StreamEvent::MessageDelta { delta, usage } => {
                if let Some(u) = usage {
                    *usage_accum += convert_usage(&u);
                }
                if let Some(d) = delta
                    && let Some(reason) = d.stop_reason.as_deref()
                {
                    stop = Some(parse_stop_reason(reason));
                }
            }
            _ => {}
        },
        ClaudeEvent::Result {
            stop_reason: sr,
            usage: u,
            total_cost_usd,
            error: err,
            is_error,
            ..
        } => {
            if let Some(u) = u.as_ref() {
                let mut converted = convert_usage(u);
                if let Some(cost) = total_cost_usd {
                    converted.cost_usd = Some(cost);
                }
                *usage_accum += converted;
            }
            if is_error.unwrap_or(false) {
                emitted.push(ProviderEvent::Error {
                    error: ProviderError::StreamError {
                        reason: err.unwrap_or_else(|| "Claude runner reported error".to_owned()),
                    },
                });
            }
            stop = Some(sr.as_deref().map_or(StopReason::EndTurn, parse_stop_reason));
        }
        _ => {}
    }

    (emitted, stop)
}

fn message_usage(message: &ClaudeMessage) -> Usage {
    message
        .usage
        .as_ref()
        .map(convert_usage)
        .unwrap_or_default()
}

fn convert_usage(u: &ClaudeUsage) -> Usage {
    Usage {
        input_tokens: u.input_tokens.unwrap_or(0),
        output_tokens: u.output_tokens.unwrap_or(0),
        cache_read_tokens: u.cache_read_input_tokens.unwrap_or(0),
        cache_write_tokens: u.cache_creation_input_tokens.unwrap_or(0),
        cost_usd: None,
    }
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "content_filter" | "refusal" => StopReason::ContentFilter,
        _ => StopReason::EndTurn,
    }
}

/// Extract the `(name, input)` pair from a [`ToolData`] value. Used by both
/// the adapter (for [`ProviderEvent`] emission) and the wrapped runner (for
/// [`SessionEvent`] capture).
pub(super) fn tool_data_pair(data: &ToolData) -> (String, Value) {
    serde_json::to_value(data)
        .ok()
        .and_then(|v| match v {
            Value::Object(mut map) => {
                let name = map
                    .remove("name")
                    .and_then(|n| n.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned());
                let input = map
                    .remove("input")
                    .unwrap_or(Value::Object(serde_json::Map::default()));
                Some((name, input))
            }
            _ => None,
        })
        .unwrap_or_else(|| ("unknown".to_owned(), Value::Null))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::provider::request::{Message, MessageRole, ProviderRequest};

    fn config() -> ClaudeRunnerConfig {
        ClaudeRunnerConfig {
            runner_path: PathBuf::from("/usr/local/bin/claude"),
            model: "sonnet".to_owned(),
            max_tokens: None,
        }
    }

    fn user_request(prompt: &str) -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(prompt.to_owned()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "sonnet".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    // R1 verification: ClaudeRunnerAdapter implements Provider -- static
    // coercion compiles only when the impl exists.
    #[test]
    fn adapter_implements_provider() {
        let adapter = ClaudeRunnerAdapter::new(config());
        let _provider: &dyn Provider = &adapter;
    }

    // R1 acceptance: built command carries prompt, stream-json format, model.
    #[test]
    fn build_command_includes_prompt_and_stream_json() {
        let adapter = ClaudeRunnerAdapter::new(config());
        let cmd = adapter.build_command(&user_request("hello"));
        let args = cmd.build_args();
        let joined = args.join(" ");
        assert!(joined.contains("hello"), "args carry prompt: {joined}");
        assert!(
            joined.contains("stream-json"),
            "stream-json format: {joined}"
        );
        assert!(joined.contains("-p"), "print mode: {joined}");
        assert!(joined.contains("--model"), "model flag: {joined}");
    }

    #[test]
    fn consolidate_outcome_extracts_result_and_stop_reason() {
        let events = vec![
            ClaudeEvent::Assistant {
                message: ClaudeMessage {
                    id: Some("m1".to_owned()),
                    message_type: Some("message".to_owned()),
                    role: "assistant".to_owned(),
                    model: None,
                    content: vec![ContentItem::Text {
                        text: "hi".to_owned(),
                    }],
                    stop_reason: Some("end_turn".to_owned()),
                    usage: Some(ClaudeUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        ..Default::default()
                    }),
                },
                session_id: Some("s1".to_owned()),
                parent_tool_use_id: None,
                uuid: None,
            },
            ClaudeEvent::Result {
                subtype: Some("success".to_owned()),
                is_error: Some(false),
                duration_ms: Some(100),
                duration_api_ms: Some(80),
                result: Some(serde_json::json!({"ok": true})),
                error: None,
                num_turns: Some(1),
                session_id: Some("s1".to_owned()),
                structured_output: None,
                stop_reason: Some("end_turn".to_owned()),
                total_cost_usd: Some(0.001),
                usage: Some(ClaudeUsage {
                    input_tokens: Some(20),
                    output_tokens: Some(7),
                    ..Default::default()
                }),
            },
        ];
        let outcome = consolidate_outcome(&events).unwrap();
        assert_eq!(outcome.result, serde_json::json!({"ok": true}));
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(outcome.usage.input_tokens, 30);
        assert_eq!(outcome.usage.output_tokens, 12);
        assert_eq!(outcome.usage.cost_usd, Some(0.001));
    }

    #[test]
    fn consolidate_outcome_propagates_is_error() {
        let events = vec![ClaudeEvent::Result {
            subtype: Some("error".to_owned()),
            is_error: Some(true),
            duration_ms: None,
            duration_api_ms: None,
            result: None,
            error: Some("internal".to_owned()),
            num_turns: None,
            session_id: None,
            structured_output: None,
            stop_reason: None,
            total_cost_usd: None,
            usage: None,
        }];
        let err = consolidate_outcome(&events).unwrap_err();
        match err {
            IntegrationError::ClaudeRunnerError { reason } => assert_eq!(reason, "internal"),
            other => panic!("expected ClaudeRunnerError, got {other:?}"),
        }
    }
}
