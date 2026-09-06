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
use claude_runner::types::{InputFormat, Model, OutputFormat};
use claude_runner::{ClaudeCommand, ClaudeEvent, ClaudeProcess};
use futures_util::Stream;
use serde_json::Value;

use crate::error::{IntegrationError, ProviderError};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::{Message, MessageRole, ProviderRequest};
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::resource::DescriptorGovernor;

mod validation;

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

/// Provider implementation that routes model-only requests through the Claude CLI.
///
/// `ClaudeRunnerAdapter::stream` builds a [`ClaudeCommand`] with
/// `--output-format stream-json --include-partial-messages`, spawns a
/// [`ClaudeProcess`], and forwards each line-delimited [`ClaudeEvent`] as a
/// [`ProviderEvent`]. Native Claude tools and ambient settings are disabled.
/// Requests containing Norn tool schemas are rejected because the provider
/// adapter cannot safely bind those schemas to Norn's governed tool runtime;
/// use [`NornWrappedClaudeSession`](super::NornWrappedClaudeSession) with its
/// strict MCP bridge for agentic execution.
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
    pub(crate) fn build_command(
        &self,
        request: &ProviderRequest,
    ) -> Result<ClaudeCommand, ProviderError> {
        validation::reject_canonical_response_items(request)?;
        validation::reject_unbound_tools(request)?;
        let prompt = render_prompt(&request.messages);
        let system = render_system_prompt(&request.messages);

        let mut cmd = ClaudeCommand::minimal_subscription()
            .binary(self.config.runner_path.to_string_lossy().into_owned())
            .prompt(prompt)
            .input_format(InputFormat::Text)
            .output_format(OutputFormat::StreamJson)
            .include_partial_messages();
        if !system.is_empty() {
            cmd = cmd.system_prompt(system);
        }
        let model_name = if request.model.is_empty() {
            self.config.model.clone()
        } else {
            request.model.clone()
        };
        cmd = cmd.model(Model::full(model_name));
        if let Some(effort) = validation::claude_effort(request)? {
            cmd = cmd.effort(effort);
        }
        Ok(cmd)
    }

    /// Execute one call against the adapter and return the consolidated
    /// [`StepOutcome`]. Convenience helper that consumes the stream and
    /// rolls each event up to a single result/usage pair.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::ClaudeRunnerError`] when spawning or
    /// reading the runner process fails, when the runner reports a terminal
    /// error result, or when it died before emitting one — a step with no
    /// terminal `result` event produced no outcome to return. Canonical
    /// Responses items are also rejected before the command is rendered
    /// because the Claude CLI prompt shape cannot preserve them.
    pub fn run_step(&self, request: &ProviderRequest) -> Result<StepOutcome, IntegrationError> {
        let cmd =
            self.build_command(request)
                .map_err(|error| IntegrationError::ClaudeRunnerError {
                    reason: error.to_string(),
                })?;
        let events = spawn_and_collect(&cmd)?;
        let outcome = consolidate_outcome(&events)?;
        Ok(outcome)
    }
}

impl Provider for ClaudeRunnerAdapter {
    fn model_catalog_backend(&self) -> Option<crate::model_selection::CatalogBackend> {
        Some(crate::model_selection::CatalogBackend::CLAUDE)
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let cmd = self.build_command(&request)?;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);

        tokio::task::spawn_blocking(move || {
            let governor = match DescriptorGovernor::global() {
                Ok(governor) => governor,
                Err(error) => {
                    let _ =
                        tx.blocking_send(Err(ProviderError::DescriptorAdmission(Box::new(error))));
                    return;
                }
            };
            let _permit = match governor.try_acquire(crate::resource::ONE_PIPE_SPAWN_PEAK) {
                Ok(permit) => permit,
                Err(error) => {
                    let _ =
                        tx.blocking_send(Err(ProviderError::DescriptorAdmission(Box::new(error))));
                    return;
                }
            };
            let mut process = match ClaudeProcess::spawn(&cmd) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.blocking_send(Err(spawn_failure(&e)));
                    return;
                }
            };

            let mut total_usage = Usage::default();
            let mut sent_done = false;

            loop {
                match process.read_event() {
                    Ok(Some(event)) => {
                        let (events, stop) = match map_claude_event(event, &mut total_usage) {
                            Ok(mapped) => mapped,
                            Err(error) => {
                                let _ = tx.blocking_send(Err(error));
                                return;
                            }
                        };
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
                            transient: None,
                        }));
                        return;
                    }
                }
            }

            // `sent_done` is set only where a stop reason actually arrived —
            // a `result` event, an assistant message carrying `stop_reason`,
            // or a `message_delta` carrying one — and every other way out of
            // the loop returns early. Reaching here therefore means exactly
            // one thing: the runner's stdout hit EOF before the protocol's
            // terminal event. The turn did not complete, so it is reported as
            // the typed failure it is; synthesizing `Done` here would present
            // a crashed, OOM-killed, or immediately-exiting runner as a
            // successful turn carrying whatever partial text had arrived.
            if !sent_done {
                let _ = tx.blocking_send(Err(ProviderError::StreamError {
                    reason: missing_terminal_result_reason(process),
                    transient: None,
                }));
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

/// Map a Claude runner process-creation failure onto the provider taxonomy.
///
/// Total over [`claude_runner::Error`]: every way the runner process can fail
/// to start becomes [`ProviderError::RunnerSpawnFailed`], which classifies
/// [`ErrorClass::Terminal`](crate::error::ErrorClass::Terminal). The mapping
/// is structural — it never inspects the operating system's message text —
/// and it is deliberately terminal for the entire set, including host fork
/// pressure; [`ProviderError::RunnerSpawnFailed`]'s rustdoc carries the
/// full rationale and records the one taxonomy gap that could ever split it.
///
/// The previous mapping (`ConnectionFailed { kind: ConnectionReset }`) was
/// retryable and untrue on both counts: no connection was ever established,
/// and under the loop's unbounded default policy a missing or non-executable
/// runner binary respawned forever against a fault that cannot heal.
fn spawn_failure(error: &claude_runner::Error) -> ProviderError {
    ProviderError::RunnerSpawnFailed {
        reason: format!("failed to spawn Claude runner: {error}"),
    }
}

/// Describe a runner stream that ended before the protocol's terminal
/// `result` event, reaping the child so the description carries how it died.
///
/// Reaping is safe here and only here: the caller reaches this function
/// because the runner's stdout reached EOF, which the Claude CLI closes on
/// exit — after its stop hooks have run. Waiting is therefore bounded by a
/// process that has already finished writing, unlike the completed-turn path,
/// where the terminal event can precede minutes of stop-hook work.
///
/// The exit status text is the operating system's own (`exit status: 3`,
/// `signal: 9 (SIGKILL)`): kernel/libc authored, never provider-controlled,
/// and exactly what distinguishes a crash from an OOM kill for an operator.
fn missing_terminal_result_reason(process: ClaudeProcess) -> String {
    let disposition = match process.wait() {
        Ok(status) => format!("runner {status}"),
        Err(error) => format!("runner exit status unavailable: {error}"),
    };
    format!("Claude runner stream ended without a terminal result event ({disposition})")
}

fn render_prompt(messages: &[Message]) -> String {
    let mut buf = String::new();
    for msg in messages {
        // Claude Runner has no distinct Developer-message CLI surface. D8's
        // compatibility policy explicitly downgrades Developer to the ordinary
        // prompt rather than silently dropping it or promoting it to System.
        if !matches!(
            msg.role,
            MessageRole::Developer | MessageRole::User | MessageRole::ToolResult
        ) {
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
                buf.push_str("\n\n");
            }
            buf.push_str(content);
        }
    }
    buf
}

/// Spawn a [`ClaudeProcess`] from the given command and collect all events
/// synchronously. Used by both the adapter and the wrapped Claude Code
/// runner.
///
/// # Errors
///
/// Returns [`IntegrationError::ClaudeRunnerError`] when the descriptor budget
/// refuses the spawn, when the process cannot be started, when reading its
/// stdout fails, or when the stream ends before the protocol's terminal
/// `result` event. Both callers drive the CLI in `--print`
/// `--output-format stream-json` mode, whose every completed invocation ends
/// with that event; its absence means the runner died mid-turn, and returning
/// the partial event list would present that death as a completed run.
pub(super) fn spawn_and_collect(cmd: &ClaudeCommand) -> Result<Vec<ClaudeEvent>, IntegrationError> {
    let governor =
        DescriptorGovernor::global().map_err(|error| IntegrationError::ClaudeRunnerError {
            reason: error.to_string(),
        })?;
    let _permit = governor
        .try_acquire(crate::resource::ONE_PIPE_SPAWN_PEAK)
        .map_err(|error| IntegrationError::ClaudeRunnerError {
            reason: error.to_string(),
        })?;
    let mut process =
        ClaudeProcess::spawn(cmd).map_err(|e| IntegrationError::ClaudeRunnerError {
            reason: format!("failed to spawn Claude runner: {e}"),
        })?;
    let mut events = Vec::new();
    let mut saw_terminal_result = false;
    loop {
        match process.read_event() {
            Ok(Some(ev)) => {
                saw_terminal_result |= matches!(ev, ClaudeEvent::Result { .. });
                events.push(ev);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(IntegrationError::ClaudeRunnerError {
                    reason: format!("read failed: {e}"),
                });
            }
        }
    }
    if !saw_terminal_result {
        return Err(IntegrationError::ClaudeRunnerError {
            reason: missing_terminal_result_reason(process),
        });
    }
    Ok(events)
}

/// Roll a collected event stream up into the single [`StepOutcome`] a step
/// claims. The claim is only made when the runner's terminal `result` event
/// is present: without it there is no outcome, and defaulting to a null
/// result with an `EndTurn` stop reason would assert a successful step over a
/// stream that was cut short.
fn consolidate_outcome(events: &[ClaudeEvent]) -> Result<StepOutcome, IntegrationError> {
    let mut result = Value::Null;
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::EndTurn;
    let mut error: Option<String> = None;
    let mut saw_terminal_result = false;

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
                saw_terminal_result = true;
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
    if !saw_terminal_result {
        return Err(IntegrationError::ClaudeRunnerError {
            reason: "Claude runner stream ended without a terminal result event".to_owned(),
        });
    }
    Ok(StepOutcome {
        result,
        usage,
        stop_reason,
    })
}

/// Translate one runner event into the provider events it carries and the
/// stop reason it may announce.
///
/// # Errors
///
/// Returns [`ProviderError::ResponseParseError`] when a tool-use payload in
/// the event is malformed; see [`tool_data_pair`].
fn map_claude_event(
    event: ClaudeEvent,
    usage_accum: &mut Usage,
) -> Result<(Vec<ProviderEvent>, Option<StopReason>), ProviderError> {
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
                        let (name, input) = tool_data_pair(tool_data)?;
                        // Claude's `id` is the streaming item identifier — the
                        // same role `item_id` plays in the OpenAI Responses
                        // stream. It is used by `assemble_response` to merge
                        // deltas, and is later promoted to `call_id` on the
                        // emitted ToolCallComplete (synthesized below).
                        emitted.push(ProviderEvent::ToolCallDelta {
                            item_id: id.clone(),
                            // Claude's tool-use `id` is both the streaming merge
                            // key and the identifier promoted to `call_id` on
                            // the synthesized ToolCallComplete, so it is the
                            // correlation id embedders see for this call.
                            call_id: Some(id.clone()),
                            name: Some(name),
                            // `Value`'s `Display` is serde_json's compact
                            // serializer over a `String` sink: a `Value` holds
                            // only string-keyed maps and finite numbers, so
                            // this rendering has no failure mode to hide. The
                            // previous `to_string(&input).unwrap_or_default()`
                            // could only ever have turned a serialization
                            // failure into an empty argument set — a tool call
                            // silently stripped of its arguments.
                            arguments_delta: input.to_string(),
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
                        // Anthropic's `input_json_delta` fragments arrive with
                        // no tool id in the same event; the correlation id is
                        // unavailable here (honest `None`, never fabricated).
                        call_id: None,
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
                        transient: None,
                    },
                });
            }
            stop = Some(sr.as_deref().map_or(StopReason::EndTurn, parse_stop_reason));
        }
        _ => {}
    }

    Ok((emitted, stop))
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
///
/// # Errors
///
/// Returns [`ProviderError::ResponseParseError`] when the runner's tool-use
/// payload does not render as a named JSON object. Every such shape is
/// malformed provider output: a tool call Norn cannot name is a tool call it
/// cannot execute or attribute, and the previous `("unknown", null)` fallback
/// invented both a tool name that was never requested and an empty argument
/// set, silently, in the middle of the tool-call path.
pub(super) fn tool_data_pair(data: &ToolData) -> Result<(String, Value), ProviderError> {
    let rendered =
        serde_json::to_value(data).map_err(|error| ProviderError::ResponseParseError {
            reason: format!("Claude tool-use payload could not be rendered as JSON: {error}"),
        })?;
    let Value::Object(mut map) = rendered else {
        return Err(ProviderError::ResponseParseError {
            reason: "Claude tool-use payload did not render as a JSON object".to_owned(),
        });
    };
    let Some(Value::String(name)) = map.remove("name") else {
        return Err(ProviderError::ResponseParseError {
            reason: "Claude tool-use payload carries no tool name".to_owned(),
        });
    };
    let input = map
        .remove("input")
        .ok_or_else(|| ProviderError::ResponseParseError {
            reason: "Claude tool-use payload carries no input field".to_owned(),
        })?;
    Ok((name, input))
}

#[cfg(test)]
mod effort_tests;

#[cfg(test)]
mod role_authority_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::provider::request::{Message, MessageRole, ProviderRequest};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
                response_items: Vec::new(),
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some(prompt.to_owned()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            tools: vec![],
            model: "sonnet".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    fn request_with_canonical_item() -> TestResult<ProviderRequest> {
        use crate::provider::response_item::{
            ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
        };

        let mut request = user_request("new turn");
        request.messages.insert(
            0,
            Message {
                response_items: vec![ResponseTranscriptItem {
                    item: ResponseItem::from_value(serde_json::json!({
                        "type": "future_response_item",
                        "id": "item_1"
                    }))?,
                    provenance: ResponseStreamProvenance::default(),
                }],
                reasoning: Vec::new(),
                role: MessageRole::Assistant,
                content: Some("lossy projection".to_owned()),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            },
        );
        Ok(request)
    }

    // R1 verification: ClaudeRunnerAdapter implements Provider -- static
    // coercion compiles only when the impl exists.
    #[test]
    fn adapter_implements_provider() {
        let adapter = ClaudeRunnerAdapter::new(config());
        let provider: &dyn Provider = &adapter;
        assert_eq!(
            provider.model_catalog_backend(),
            Some(crate::model_selection::CatalogBackend::CLAUDE),
        );
    }

    // R1 acceptance: built command carries prompt, stream-json format, model.
    #[test]
    fn build_command_includes_prompt_and_stream_json() -> TestResult {
        let adapter = ClaudeRunnerAdapter::new(config());
        let cmd = adapter.build_command(&user_request("hello"))?;
        let args = cmd.build_args();
        let joined = args.join(" ");
        assert!(joined.contains("hello"), "args carry prompt: {joined}");
        assert!(
            joined.contains("stream-json"),
            "stream-json format: {joined}"
        );
        assert!(joined.contains("-p"), "print mode: {joined}");
        assert!(joined.contains("--model"), "model flag: {joined}");
        Ok(())
    }

    #[test]
    fn run_step_rejects_canonical_items_before_spawning() -> TestResult {
        let adapter = ClaudeRunnerAdapter::new(config());
        let request = request_with_canonical_item()?;
        let Err(error) = adapter.run_step(&request) else {
            return Err("canonical Responses items must fail before spawning Claude Runner".into());
        };

        match error {
            IntegrationError::ClaudeRunnerError { reason } => assert_eq!(
                reason,
                "unsupported feature: canonical Responses item replay through Claude Runner"
            ),
            other => return Err(format!("expected ClaudeRunnerError, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn provider_stream_rejects_canonical_items_before_rendering() -> TestResult {
        let adapter = ClaudeRunnerAdapter::new(config());
        let result = adapter.stream(request_with_canonical_item()?);

        match result {
            Err(ProviderError::UnsupportedFeature { feature }) => assert_eq!(
                feature,
                "canonical Responses item replay through Claude Runner"
            ),
            Err(other) => return Err(format!("expected UnsupportedFeature, got {other:?}").into()),
            Ok(_) => return Err("canonical Responses items must fail closed".into()),
        }
        Ok(())
    }

    /// Drive the adapter's provider stream against `runner_path` and return
    /// the first item's error, failing the test if the spawn unexpectedly
    /// succeeded or the stream produced an event instead.
    async fn first_stream_error(runner_path: PathBuf) -> TestResult<ProviderError> {
        use futures_util::StreamExt;

        let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
            runner_path,
            model: "sonnet".to_owned(),
            max_tokens: None,
        });
        let mut stream = adapter.stream(user_request("hello"))?;
        let Some(first) = stream.next().await else {
            return Err("a failed spawn must surface an error on the stream".into());
        };
        match first {
            Err(error) => Ok(error),
            Ok(event) => Err(format!("a failed spawn must not yield {event:?}").into()),
        }
    }

    /// A runner path that does not exist is a deterministic configuration
    /// fault. The loop's default retry policy is unbounded, so a retryable
    /// classification here respawns forever against a fault that can never
    /// heal; the spawn must fail the turn loudly instead.
    #[tokio::test]
    async fn nonexistent_runner_path_spawn_failure_classifies_terminal() -> TestResult {
        let error =
            first_stream_error(PathBuf::from("/nonexistent/norn-f1/claude-binary-absent")).await?;

        assert!(
            matches!(error, ProviderError::RunnerSpawnFailed { .. }),
            "expected RunnerSpawnFailed, got {error:?}"
        );
        assert_eq!(
            error.class(),
            crate::error::ErrorClass::Terminal,
            "spawn against a nonexistent runner path must be terminal, got {error:?}"
        );
        assert!(!error.is_retryable());
        Ok(())
    }

    /// Mirror of the not-found case: a runner path that exists but is not
    /// executable is equally deterministic and must not be retried either.
    #[cfg(unix)]
    #[tokio::test]
    async fn non_executable_runner_path_spawn_failure_classifies_terminal() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let runner_path = dir.path().join("claude-not-executable");
        std::fs::write(&runner_path, b"#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&runner_path, std::fs::Permissions::from_mode(0o600))?;

        let error = first_stream_error(runner_path).await?;

        assert!(
            matches!(error, ProviderError::RunnerSpawnFailed { .. }),
            "expected RunnerSpawnFailed, got {error:?}"
        );
        assert_eq!(
            error.class(),
            crate::error::ErrorClass::Terminal,
            "spawn against a non-executable runner path must be terminal, got {error:?}"
        );
        assert!(!error.is_retryable());
        Ok(())
    }

    /// The spawn mapping is structural: no operating-system message text can
    /// steer the classification, and no spawn fault is dressed up as a
    /// transport failure the way the previous `ConnectionFailed` mapping did.
    #[test]
    fn spawn_failure_maps_every_runner_error_to_a_terminal_spawn_fault() {
        let cases = [
            claude_runner::Error::Spawn(std::io::Error::from(std::io::ErrorKind::NotFound)),
            claude_runner::Error::Spawn(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            // Host process-creation pressure (EAGAIN/ENOMEM). Terminal by the
            // same rule the descriptor governor already applies to this spawn:
            // local resource exhaustion is not dressed up as a transport fault.
            claude_runner::Error::Spawn(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            claude_runner::Error::Spawn(std::io::Error::from(std::io::ErrorKind::OutOfMemory)),
            claude_runner::Error::Spawn(std::io::Error::from(std::io::ErrorKind::Other)),
            claude_runner::Error::Timeout,
        ];
        for case in cases {
            let mapped = spawn_failure(&case);
            assert!(
                matches!(mapped, ProviderError::RunnerSpawnFailed { .. }),
                "expected RunnerSpawnFailed for {case}, got {mapped:?}"
            );
            assert_eq!(
                mapped.class(),
                crate::error::ErrorClass::Terminal,
                "spawn fault {case} must be terminal"
            );
            assert!(!mapped.is_retryable(), "spawn fault {case} must not retry");
        }
    }

    /// One line of `--output-format stream-json` carrying assistant text and
    /// no `stop_reason`: partial output, turn not finished.
    const PARTIAL_TEXT_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"partial answer"}]},"session_id":"s1"}"#;

    /// An assistant line that itself carries the turn's `stop_reason` — the
    /// adapter treats this as a legitimate end of turn.
    const ASSISTANT_STOP_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn"},"session_id":"s1"}"#;

    /// The protocol's terminal event.
    const RESULT_LINE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":{"ok":true},"stop_reason":"end_turn","session_id":"s1"}"#;

    /// Write an executable fake Claude CLI that prints `lines` to stdout and
    /// exits with `exit_code`, ignoring its arguments. The JSON fixtures above
    /// contain no single quotes, so single-quoted `printf` operands are exact.
    #[cfg(unix)]
    fn fake_runner(
        dir: &std::path::Path,
        lines: &[&str],
        exit_code: i32,
    ) -> TestResult<std::path::PathBuf> {
        use std::fmt::Write as _;
        use std::os::unix::fs::PermissionsExt;

        let mut script = String::from("#!/bin/sh\n");
        for line in lines {
            writeln!(script, "printf '%s\\n' '{line}'")?;
        }
        writeln!(script, "exit {exit_code}")?;

        let path = dir.join("claude-fake");
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }

    /// Drive the adapter's provider stream against `runner_path` to completion.
    #[cfg(unix)]
    async fn collect_stream(
        runner_path: std::path::PathBuf,
    ) -> TestResult<Vec<Result<ProviderEvent, ProviderError>>> {
        use futures_util::StreamExt;

        let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
            runner_path,
            model: "sonnet".to_owned(),
            max_tokens: None,
        });
        let stream = adapter.stream(user_request("hello"))?;
        Ok(stream.collect::<Vec<_>>().await)
    }

    /// A runner that dies before emitting the protocol's terminal `result`
    /// event has not completed a turn. Synthesizing `Done { EndTurn }` over
    /// whatever partial text arrived reports a crash as a successful turn —
    /// the worst possible shape. The stream must fail loudly and typed.
    #[cfg(unix)]
    #[tokio::test]
    async fn runner_death_without_a_result_event_fails_instead_of_synthesizing_done() -> TestResult
    {
        let dir = tempfile::tempdir()?;
        let runner = fake_runner(dir.path(), &[PARTIAL_TEXT_LINE], 3)?;

        let items = collect_stream(runner).await?;

        assert!(
            items
                .iter()
                .any(|item| matches!(item, Ok(ProviderEvent::TextDelta { .. }))),
            "the partial text the runner did emit is still forwarded: {items:?}"
        );
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, Ok(ProviderEvent::Done { .. }))),
            "a runner that never emitted a result event must not report Done: {items:?}"
        );

        let Some(Err(error)) = items.last() else {
            return Err(format!("expected a trailing typed error, got {items:?}").into());
        };
        match error {
            ProviderError::StreamError { reason, transient } => {
                assert!(
                    transient.is_none(),
                    "a violated protocol contract is not a transport transient: {error:?}"
                );
                assert!(
                    reason.contains("terminal result event"),
                    "reason names the missing terminal event: {reason}"
                );
                assert!(
                    reason.contains("exit status: 3"),
                    "reason carries the reaped child exit status: {reason}"
                );
            }
            other => return Err(format!("expected StreamError, got {other:?}").into()),
        }
        assert_eq!(error.class(), crate::error::ErrorClass::Terminal);
        assert!(!error.is_retryable());
        Ok(())
    }

    /// The legitimate flow: the runner emits its terminal `result` event and
    /// then stdout reaches EOF. The turn completed, so `Done` is truthful and
    /// must keep flowing.
    #[cfg(unix)]
    #[tokio::test]
    async fn result_event_then_eof_still_completes_the_turn() -> TestResult {
        let dir = tempfile::tempdir()?;
        let runner = fake_runner(dir.path(), &[PARTIAL_TEXT_LINE, RESULT_LINE], 0)?;

        let items = collect_stream(runner).await?;

        assert!(
            items.iter().all(Result::is_ok),
            "a completed turn carries no error: {items:?}"
        );
        assert!(
            matches!(
                items.last(),
                Some(Ok(ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    ..
                }))
            ),
            "the terminal result event completes the turn: {items:?}"
        );
        Ok(())
    }

    /// The other legitimate flow: the turn's `stop_reason` arrives on the
    /// assistant message itself. The adapter completes the turn there and
    /// stops reading, so no terminal-result check applies.
    #[cfg(unix)]
    #[tokio::test]
    async fn assistant_stop_reason_still_completes_the_turn() -> TestResult {
        let dir = tempfile::tempdir()?;
        let runner = fake_runner(dir.path(), &[ASSISTANT_STOP_LINE], 0)?;

        let items = collect_stream(runner).await?;

        assert!(
            items.iter().all(Result::is_ok),
            "a completed turn carries no error: {items:?}"
        );
        assert!(
            matches!(
                items.last(),
                Some(Ok(ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    ..
                }))
            ),
            "an assistant stop_reason completes the turn: {items:?}"
        );
        Ok(())
    }

    /// The synchronous path has the same contract: a runner that dies before
    /// its terminal `result` event produced no step outcome, and `run_step`
    /// must not hand back a `StepOutcome` whose `result` is merely `null`.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_step_fails_when_the_runner_dies_without_a_result_event() -> TestResult {
        let dir = tempfile::tempdir()?;
        let runner = fake_runner(dir.path(), &[PARTIAL_TEXT_LINE], 3)?;
        let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
            runner_path: runner,
            model: "sonnet".to_owned(),
            max_tokens: None,
        });

        let outcome = tokio::task::spawn_blocking(move || adapter.run_step(&user_request("hello")))
            .await
            .map_err(|error| format!("run_step task failed: {error}"))?;

        match outcome {
            Err(IntegrationError::ClaudeRunnerError { reason }) => {
                assert!(
                    reason.contains("terminal result event"),
                    "reason names the missing terminal event: {reason}"
                );
                assert!(
                    reason.contains("exit status: 3"),
                    "reason carries the reaped child exit status: {reason}"
                );
            }
            Err(other) => return Err(format!("expected ClaudeRunnerError, got {other:?}").into()),
            Ok(outcome) => {
                return Err(
                    format!("a runner death must not produce a step outcome: {outcome:?}").into(),
                );
            }
        }
        Ok(())
    }

    /// `consolidate_outcome` builds the success claim for a step. A stream
    /// with no terminal `result` event carries no outcome to claim.
    #[test]
    fn consolidate_outcome_rejects_a_stream_without_a_result_event() -> TestResult {
        let events = vec![ClaudeEvent::Assistant {
            message: ClaudeMessage {
                id: Some("m1".to_owned()),
                message_type: Some("message".to_owned()),
                role: "assistant".to_owned(),
                model: None,
                content: vec![ContentItem::Text {
                    text: "partial".to_owned(),
                }],
                stop_reason: None,
                usage: None,
            },
            session_id: Some("s1".to_owned()),
            parent_tool_use_id: None,
            uuid: None,
        }];

        let Err(error) = consolidate_outcome(&events) else {
            return Err("a stream with no result event must not consolidate to success".into());
        };
        match error {
            IntegrationError::ClaudeRunnerError { reason } => assert!(
                reason.contains("terminal result event"),
                "reason names the missing terminal event: {reason}"
            ),
            other => return Err(format!("expected ClaudeRunnerError, got {other:?}").into()),
        }
        Ok(())
    }

    /// A tool-use payload the runner sent without a name cannot be executed.
    /// Naming it `"unknown"` invents a tool that was never requested.
    #[test]
    fn tool_data_pair_rejects_a_nameless_tool_payload() -> TestResult {
        let data = ToolData::Unknown {
            name: None,
            input: Some(serde_json::json!({"a": 1})),
            extra: std::collections::HashMap::new(),
        };

        let Err(error) = tool_data_pair(&data) else {
            return Err("a nameless tool payload must not be renamed 'unknown'".into());
        };
        assert!(
            matches!(error, ProviderError::ResponseParseError { .. }),
            "expected ResponseParseError, got {error:?}"
        );
        assert_eq!(error.class(), crate::error::ErrorClass::Terminal);
        Ok(())
    }

    /// The ordinary shape still round-trips: a named tool keeps its name and
    /// its input verbatim.
    #[test]
    fn tool_data_pair_extracts_name_and_input() -> TestResult {
        let data = ToolData::Read {
            file_path: "/tmp/x".to_owned(),
            offset: None,
            limit: None,
        };

        let (name, input) = tool_data_pair(&data)?;
        assert_eq!(name, "Read");
        assert_eq!(input["file_path"], serde_json::json!("/tmp/x"));
        Ok(())
    }

    /// The malformed payload must reach the caller as a typed failure rather
    /// than being smuggled into the event stream as a tool call.
    #[test]
    fn map_claude_event_propagates_a_malformed_tool_payload() -> TestResult {
        let event = ClaudeEvent::Assistant {
            message: ClaudeMessage {
                id: Some("m1".to_owned()),
                message_type: Some("message".to_owned()),
                role: "assistant".to_owned(),
                model: None,
                content: vec![ContentItem::ToolUse {
                    id: "call_1".to_owned(),
                    tool_data: ToolData::Unknown {
                        name: None,
                        input: Some(serde_json::json!({"a": 1})),
                        extra: std::collections::HashMap::new(),
                    },
                }],
                stop_reason: None,
                usage: None,
            },
            session_id: Some("s1".to_owned()),
            parent_tool_use_id: None,
            uuid: None,
        };

        let mut usage = Usage::default();
        let Err(error) = map_claude_event(event, &mut usage) else {
            return Err("a malformed tool payload must not map to provider events".into());
        };
        assert!(
            matches!(error, ProviderError::ResponseParseError { .. }),
            "expected ResponseParseError, got {error:?}"
        );
        Ok(())
    }

    /// Tool-call arguments are the runner's own JSON, forwarded verbatim —
    /// never quietly replaced by an empty string.
    #[test]
    fn tool_call_arguments_are_forwarded_verbatim() -> TestResult {
        let event = ClaudeEvent::Assistant {
            message: ClaudeMessage {
                id: Some("m1".to_owned()),
                message_type: Some("message".to_owned()),
                role: "assistant".to_owned(),
                model: None,
                content: vec![ContentItem::ToolUse {
                    id: "call_1".to_owned(),
                    tool_data: ToolData::Read {
                        file_path: "/tmp/x".to_owned(),
                        offset: None,
                        limit: None,
                    },
                }],
                stop_reason: None,
                usage: None,
            },
            session_id: Some("s1".to_owned()),
            parent_tool_use_id: None,
            uuid: None,
        };

        let mut usage = Usage::default();
        let (events, stop) = map_claude_event(event, &mut usage)?;
        assert!(stop.is_none());
        let Some(ProviderEvent::ToolCallDelta {
            arguments_delta,
            name,
            ..
        }) = events.first()
        else {
            return Err(format!("expected a ToolCallDelta, got {events:?}").into());
        };
        assert_eq!(name.as_deref(), Some("Read"));
        let parsed: Value = serde_json::from_str(arguments_delta)?;
        assert_eq!(parsed["file_path"], serde_json::json!("/tmp/x"));
        Ok(())
    }

    #[test]
    fn consolidate_outcome_extracts_result_and_stop_reason() -> TestResult {
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
                sdk_metadata: Box::default(),
            },
        ];
        let outcome = consolidate_outcome(&events)?;
        assert_eq!(outcome.result, serde_json::json!({"ok": true}));
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(outcome.usage.input_tokens, 30);
        assert_eq!(outcome.usage.output_tokens, 12);
        assert_eq!(outcome.usage.cost_usd, Some(0.001));
        Ok(())
    }

    #[test]
    fn consolidate_outcome_propagates_is_error() -> TestResult {
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
            sdk_metadata: Box::default(),
        }];
        let Err(err) = consolidate_outcome(&events) else {
            return Err("an error result event must fail outcome consolidation".into());
        };
        match err {
            IntegrationError::ClaudeRunnerError { reason } => assert_eq!(reason, "internal"),
            other => return Err(format!("expected ClaudeRunnerError, got {other:?}").into()),
        }
        Ok(())
    }
}
