//! Response classification and provider interaction.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{NornError, ProviderError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::{AssembledResponse, assemble_response};
use crate::r#loop::compaction::{InFlightPartial, SharedTimeoutState};
use crate::r#loop::config::TruncationKind;
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::response_audio_capture::ResponseAudioCapture;
use crate::r#loop::schema::validate_against_schema;
use crate::provider::agent_event::{AgentEventSender, AgentStreamRetry};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ProviderRequest;
use crate::provider::traits::Provider;
use crate::session::ResponseAudioStore;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::tool::envelope::split_envelope_fields;

/// Persist the `loop.truncated` Custom event marking a truncated no-schema
/// response (REVIEW item 5).
///
/// The partial text, per-call usage, and stop reason are already persisted
/// on the preceding `AssistantMessage` event; the Custom event marks the
/// abort for observers. The runner then returns
/// [`AgentStepResult::Truncated`](crate::agent_loop::config::AgentStepResult),
/// which carries the partial text and accumulated usage — a truncated run
/// is a stopped run with partial output, never a transport error and never
/// retryable (the stop is deterministic).
///
/// # Errors
///
/// Returns [`SessionError`] if the Custom event cannot be appended.
pub(super) async fn record_truncation(
    store: &EventStore,
    hooks: Option<&HookRegistry>,
    kind: TruncationKind,
    partial_text: &str,
    iterations: u32,
) -> Result<(), SessionError> {
    append_and_notify(
        store,
        SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: "loop.truncated".to_string(),
            data: serde_json::json!({
                "stop_reason": kind.as_str(),
                "partial_text_chars": partial_text.chars().count(),
                "iterations": iterations,
            }),
        },
        hooks,
    )
    .await
    .map(|_event_id| ())
}

/// Internal classification of a provider response.
pub(super) enum ResponseClass {
    /// Schema tool called with valid output (no other tools after it matter).
    SchemaValid { output: Value },

    /// Schema tool called but output failed validation.
    SchemaInvalid {
        output: Value,
        errors: Vec<String>,
        schema_call_index: usize,
    },

    /// Only non-schema tools in the response.
    ToolsOnly { tool_calls: Vec<usize> },

    /// No tool calls and model stopped (text-only response).
    TextStopNoSchema,

    /// The backend requested another response in the current user turn.
    ContinueTurn,

    /// The model returned refusal content. This takes precedence over tool
    /// dispatch so a mixed malformed response cannot execute calls after a
    /// refusal outcome.
    Refused {
        /// Refusal text projected from the canonical message.
        refusal: String,
    },

    /// No tool calls and the provider stopped abnormally (`MaxTokens` or
    /// `ContentFilter`) in no-schema mode: any text is an incomplete
    /// fragment and must not be returned as successful output (REVIEW
    /// item 5).
    Truncated {
        /// Which abnormal stop cut the response off.
        kind: TruncationKind,
    },

    /// Tools before schema tool + schema valid. Post-schema tools rejected.
    ToolsAndSchemaValid {
        pre_schema_tools: Vec<usize>,
        output: Value,
    },
}

/// Classify a response according to which tools were called and whether
/// the schema tool's output is valid.
pub(super) fn classify_response(
    response: &AssembledResponse,
    output_schema: Option<&Value>,
    schema_tool_name: &str,
) -> ResponseClass {
    // Current Codex treats `end_turn: false` as an independent request for
    // another model sample. Preserve a refusal-only response canonically, then
    // continue instead of reporting that intermediate refusal as the turn's
    // final outcome. A malformed mixed refusal-plus-tool response does not
    // reach this arm: the refusal-authoritative check below still prevents
    // execution of calls embedded alongside refusal content.
    if response.tool_calls.is_empty() && response.stop_reason == StopReason::ContinueTurn {
        return ResponseClass::ContinueTurn;
    }

    if let Some(refusal) = &response.refusal {
        return ResponseClass::Refused {
            refusal: refusal.clone(),
        };
    }

    if response.tool_calls.is_empty() {
        // REVIEW item 5: a tool-call-free response that stopped on
        // `MaxTokens`/`ContentFilter` is an incomplete fragment. In
        // no-schema mode the runner previously returned it as successful
        // `Completed` output, indistinguishable from a clean `EndTurn` —
        // classify it distinctly so the runner can refuse. With a schema
        // present the response falls through to `TextStopNoSchema`, whose
        // nudge path consumes schema budget and terminates in
        // `SchemaUnreachable` rather than silent success.
        if output_schema.is_none() {
            match response.stop_reason {
                StopReason::MaxTokens => {
                    return ResponseClass::Truncated {
                        kind: TruncationKind::MaxTokens,
                    };
                }
                StopReason::ContentFilter => {
                    return ResponseClass::Truncated {
                        kind: TruncationKind::ContentFilter,
                    };
                }
                StopReason::EndTurn | StopReason::ContinueTurn | StopReason::ToolUse => {}
            }
        }
        return ResponseClass::TextStopNoSchema;
    }

    let schema_index = response
        .tool_calls
        .iter()
        .position(|tc| tc.name == schema_tool_name);

    let Some(schema_idx) = schema_index else {
        let indices: Vec<usize> = (0..response.tool_calls.len()).collect();
        return ResponseClass::ToolsOnly {
            tool_calls: indices,
        };
    };

    let schema_tc = &response.tool_calls[schema_idx];
    let parsed = serde_json::from_str::<Value>(&schema_tc.arguments);

    let output = match parsed {
        Ok(v) => v,
        Err(e) => {
            return ResponseClass::SchemaInvalid {
                output: Value::String(schema_tc.arguments.clone()),
                errors: vec![format!(
                    "failed to parse schema tool arguments as JSON: {e}"
                )],
                schema_call_index: schema_idx,
            };
        }
    };

    // The schema tool's model-facing definition is envelope-wrapped like
    // every other tool (`build_schema_tool`), so the call legitimately
    // carries `tool_use_description`/`tool_use_metadata`. Those are
    // envelope fields, not output: split them off before validating, or
    // any user schema with `additionalProperties: false` rejects the
    // call and the structured output leaks envelope keys. The raw call
    // (description included) is persisted on the `AssistantMessage`
    // event, so observability surfaces lose nothing.
    let output = split_envelope_fields(output).tool_args;

    let Some(schema) = output_schema else {
        return ResponseClass::SchemaValid { output };
    };

    match validate_against_schema(schema, &output) {
        Ok(()) => {
            let pre_schema: Vec<usize> = (0..schema_idx).collect();
            let has_post_schema = schema_idx + 1 < response.tool_calls.len();
            if pre_schema.is_empty() && !has_post_schema {
                ResponseClass::SchemaValid { output }
            } else {
                ResponseClass::ToolsAndSchemaValid {
                    pre_schema_tools: pre_schema,
                    output,
                }
            }
        }
        Err(errors) => ResponseClass::SchemaInvalid {
            output,
            errors,
            schema_call_index: schema_idx,
        },
    }
}

/// Call the provider with a prebuilt request, collect all streaming events,
/// forward to broadcast channel if present, and assemble the response.
///
/// An in-band [`ProviderEvent::Error`] fails the call immediately with its
/// **typed** [`ProviderError`] — the provider itself reported the failure,
/// so the retry policy classifies the real error (5xx retryable, quota
/// terminal, ...) instead of the turn dying later on a generic
/// "stream ended without a Done event".
///
/// When `partial_capture` is supplied, the in-flight text, refusal, and
/// thinking deltas
/// are mirrored into
/// [`TimeoutState::in_flight_partial`](crate::agent_loop::compaction::TimeoutState)
/// as they arrive: the capture is reset when the stream attempt starts and
/// stays armed until the runner durably appends the `AssistantMessage`
/// (`persist_assistant_turn` clears it after the append). If the step's
/// timeout or cancellation drops this future mid-stream — or in the
/// hook-running window between assembly and the append — whatever the
/// model had produced survives in the shared state for the exit path to
/// persist; otherwise a hard cut loses that content entirely (Gap 7).
pub(super) async fn call_provider(
    provider: &dyn Provider,
    request: ProviderRequest,
    event_tx: Option<&AgentEventSender>,
    partial_capture: Option<&SharedTimeoutState>,
    audio_store: Option<&ResponseAudioStore>,
    attempt: u32,
) -> Result<AssembledResponse, NornError> {
    if let Some(state) = partial_capture {
        // A fresh attempt discards any previous attempt's partials — the
        // durable analogue of the live `StreamRetry` reset marker.
        state.lock().in_flight_partial = Some(InFlightPartial::default());
    }
    let mut stream = provider.stream(request)?;
    let mut events: Vec<ProviderEvent> = Vec::new();
    let mut audio = ResponseAudioCapture::new(audio_store, attempt);

    while let Some(result) = stream.next().await {
        let event = result?;
        if let Some(sender) = event_tx {
            sender.send(event.clone());
        }
        if let Some(state) = partial_capture {
            match &event {
                ProviderEvent::TextDelta { text } => {
                    if let Some(partial) = state.lock().in_flight_partial.as_mut() {
                        partial.text.push_str(text);
                    }
                }
                ProviderEvent::ThinkingDelta { text } => {
                    if let Some(partial) = state.lock().in_flight_partial.as_mut() {
                        partial.thinking.push_str(text);
                    }
                }
                ProviderEvent::RefusalDelta { refusal, .. } => {
                    if let Some(partial) = state.lock().in_flight_partial.as_mut() {
                        partial.refusal.get_or_insert_default().push_str(refusal);
                    }
                }
                ProviderEvent::RefusalComplete { refusal, .. } => {
                    if let Some(partial) = state.lock().in_flight_partial.as_mut() {
                        partial.refusal = Some(refusal.clone());
                    }
                }
                _ => {}
            }
        }
        if let ProviderEvent::Error { error } = event {
            return Err(NornError::Provider(error));
        }
        match &event {
            ProviderEvent::ResponseStreamEvent { .. } => {}
            ProviderEvent::ResponseAudioFrame {
                stream_event,
                event,
            } => {
                audio.append(stream_event, event)?;
                if let Some(state) = partial_capture
                    && let Some(partial) = state.lock().in_flight_partial.as_mut()
                {
                    partial.response_audio = audio.reference();
                }
            }
            _ => events.push(event),
        }
    }

    let mut assembled = assemble_response(&events).ok_or_else(|| {
        // Terminal by construction (`transient: None`): the transport-level
        // cutoff cases surface earlier as retryable `StreamInterrupted`
        // from the executor; a stream that yielded events but no `Done` is
        // a provider-contract violation replays cannot fix.
        NornError::Provider(ProviderError::StreamError {
            reason: "provider stream ended without a Done event".to_string(),
            transient: None,
        })
    })?;
    assembled.response_audio = audio.seal(assembled.response_id.as_deref())?;
    if let Some(state) = partial_capture {
        // Repair previews with the terminal, canonical projection. This is
        // load-bearing in the post-LLM/pre-persist window: a timeout there
        // must retain completed text/refusal even when preview events were
        // absent, duplicated, or superseded by an authoritative done item.
        state.lock().in_flight_partial = Some(InFlightPartial {
            text: assembled.text.clone(),
            thinking: assembled.thinking.clone(),
            refusal: assembled.refusal.clone(),
            response_audio: assembled.response_audio,
        });
    }
    // The capture is deliberately NOT cleared here: assembly is not
    // durability. Between this return and the `AssistantMessage` append,
    // `run_post_llm` hooks run arbitrary user shell hooks — a step timeout
    // or cancellation landing in that window would otherwise lose the
    // complete response from the durable log (the exact loss class Gap 7
    // closes). The runner clears the capture in `persist_assistant_turn`,
    // immediately after the append succeeds.
    Ok(assembled)
}

/// [`call_provider`] under the loop's retry policy.
///
/// Each retry replays the full request, and the failed attempt may have
/// already forwarded partial stream deltas to observers on `event_tx`.
/// A typed [`AgentStreamRetry`] marker is broadcast immediately before
/// every retry attempt's stream begins, so observers reset the failed
/// attempt's partial output instead of rendering the replay appended to
/// it. The in-flight `partial_capture` resets the same way — each
/// attempt starts a fresh capture inside [`call_provider`].
///
/// # Errors
///
/// Returns the final [`NornError`] after the retry budget is exhausted,
/// or the first non-retryable error encountered.
pub(super) async fn call_provider_with_retry(
    policy: &crate::r#loop::retry::RetryPolicy,
    provider: &dyn Provider,
    request: ProviderRequest,
    event_tx: Option<&AgentEventSender>,
    partial_capture: Option<&SharedTimeoutState>,
    audio_store: Option<&ResponseAudioStore>,
) -> Result<AssembledResponse, NornError> {
    let attempts = std::sync::atomic::AtomicU32::new(0);
    crate::r#loop::retry::retry_with_backoff(policy, || {
        let req = request.clone();
        let attempt = attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        async move {
            if attempt > 1
                && let Some(sender) = event_tx
            {
                sender.send_stream_retry(AgentStreamRetry { attempt });
            }
            call_provider(
                provider,
                req,
                event_tx,
                partial_capture,
                audio_store,
                attempt,
            )
            .await
        }
    })
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::r#loop::assembly::AssembledToolCall;
    use crate::provider::openai::sse::{map_sse_event, parse_sse_bytes};
    use crate::provider::request::ToolCallKind;
    use crate::provider::usage::Usage;

    fn schema_call_response(arguments: &str) -> AssembledResponse {
        AssembledResponse {
            response_items: Vec::new(),
            refusal: None,
            text: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![AssembledToolCall {
                call_id: "call_1".to_owned(),
                name: "structured_output".to_owned(),
                arguments: arguments.to_owned(),
                kind: ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: None,
            response_audio: None,
        }
    }

    fn strict_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"],
            "additionalProperties": false
        })
    }

    #[test]
    fn refusal_precedes_schema_validation_and_tool_dispatch() {
        let mut response = schema_call_response("{\"answer\":\"would otherwise pass\"}");
        response.refusal = Some("request declined".to_owned());
        response.text = "ordinary text must not win".to_owned();
        let schema = strict_schema();

        assert!(matches!(
            classify_response(&response, Some(&schema), "structured_output"),
            ResponseClass::Refused { refusal } if refusal == "request declined"
        ));
    }

    #[test]
    fn empty_refusal_is_still_a_refusal_outcome() {
        let mut response = schema_call_response("{\"answer\":\"would otherwise pass\"}");
        response.refusal = Some(String::new());

        assert!(matches!(
            classify_response(&response, None, "structured_output"),
            ResponseClass::Refused { refusal } if refusal.is_empty()
        ));
    }

    #[test]
    fn explicit_continuation_precedes_a_refusal_without_tool_calls() {
        let mut response = schema_call_response("{\"answer\":\"unused\"}");
        response.tool_calls.clear();
        response.stop_reason = StopReason::ContinueTurn;
        response.refusal = Some("not terminal yet".to_owned());

        assert!(matches!(
            classify_response(&response, None, "structured_output"),
            ResponseClass::ContinueTurn
        ));
    }

    #[test]
    fn envelope_fields_are_stripped_before_schema_validation() {
        // Regression: the model attaches `tool_use_description` (the
        // envelope-wrapped definition marks it required) — an
        // `additionalProperties: false` user schema must not reject it,
        // and the accepted output must not contain the envelope keys.
        let response = schema_call_response(
            "{\"answer\":\"42\",\
             \"tool_use_description\":\"submitting the final result\",\
             \"tool_use_metadata\":{\"task\":\"T-1\"}}",
        );
        let schema = strict_schema();
        match classify_response(&response, Some(&schema), "structured_output") {
            ResponseClass::SchemaValid { output } => {
                assert_eq!(output, serde_json::json!({"answer": "42"}));
            }
            other => panic!(
                "expected SchemaValid, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn genuine_violations_still_fail_with_clean_output_in_feedback() {
        // Stripping the envelope must not mask real violations, and the
        // feedback output the model sees is the clean args — never the
        // envelope keys it was told to send.
        let response =
            schema_call_response("{\"answer\":7,\"tool_use_description\":\"submitting\"}");
        let schema = strict_schema();
        match classify_response(&response, Some(&schema), "structured_output") {
            ResponseClass::SchemaInvalid { output, errors, .. } => {
                assert_eq!(output, serde_json::json!({"answer": 7}));
                assert!(!errors.is_empty());
                assert!(
                    !errors.join("; ").contains("tool_use_description"),
                    "envelope fields must not appear as violations: {errors:?}",
                );
            }
            other => panic!(
                "expected SchemaInvalid, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    /// Maps a raw `OpenAI` Responses SSE transcript through the real
    /// provider parser into the loop's event stream, asserting no event
    /// surfaces as an error.
    fn provider_events(raw: &str) -> Vec<ProviderEvent> {
        parse_sse_bytes(raw)
            .iter()
            .filter_map(map_sse_event)
            .map(|r| r.expect("truncation frames must not map to errors"))
            .collect()
    }

    #[test]
    fn openai_incomplete_max_tokens_classifies_as_truncated() {
        // BLOCKER regression seam test: an OpenAI-shaped stream cut by
        // `response.incomplete` (max_output_tokens) must flow through
        // assembly into `ResponseClass::Truncated { MaxTokens }` — the
        // classification the runner turns into `AgentStepResult::Truncated`
        // (see the `ResponseClass::Truncated` arm in `runner.rs`) — with
        // the partial text and usage preserved on the assembled response.
        let raw = "event: response.output_text.delta\n\
                   data: {\"delta\":\"cut \"}\n\n\
                   event: response.output_text.delta\n\
                   data: {\"delta\":\"off\"}\n\n\
                   event: response.incomplete\n\
                   data: {\"response\":{\"id\":\"resp_t\",\"status\":\"incomplete\",\
                   \"incomplete_details\":{\"reason\":\"max_output_tokens\"},\
                   \"usage\":{\"input_tokens\":21,\"output_tokens\":34}}}\n\n";

        let events = provider_events(raw);
        let response = assemble_response(&events).expect("Done event must terminate assembly");
        assert_eq!(response.text, "cut off", "partial text must be preserved");
        assert_eq!(response.stop_reason, StopReason::MaxTokens);
        assert_eq!(response.usage.input_tokens, 21);
        assert_eq!(response.usage.output_tokens, 34);

        match classify_response(&response, None, "norn_schema_output") {
            ResponseClass::Truncated { kind } => {
                assert_eq!(kind, TruncationKind::MaxTokens);
            }
            other => panic!(
                "expected Truncated(MaxTokens), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn openai_incomplete_content_filter_classifies_as_truncated() {
        let raw = "event: response.output_text.delta\n\
                   data: {\"delta\":\"partial\"}\n\n\
                   event: response.incomplete\n\
                   data: {\"response\":{\"id\":\"resp_cf\",\"status\":\"incomplete\",\
                   \"incomplete_details\":{\"reason\":\"content_filter\"},\
                   \"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n";

        let events = provider_events(raw);
        let response = assemble_response(&events).expect("Done event must terminate assembly");
        assert_eq!(response.text, "partial");
        assert_eq!(response.stop_reason, StopReason::ContentFilter);

        match classify_response(&response, None, "norn_schema_output") {
            ResponseClass::Truncated { kind } => {
                assert_eq!(kind, TruncationKind::ContentFilter);
            }
            other => panic!(
                "expected Truncated(ContentFilter), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // -- In-band provider Error events and stream-retry markers -----------

    use std::sync::Mutex;

    use futures_util::stream;

    use crate::provider::agent_event::{AgentEvent, AgentEventKind};
    use crate::provider::mock::MockProvider;
    use crate::provider::tools::ProviderCapabilities;
    use crate::provider::traits::ProviderStream;

    /// Scripted provider whose `stream()` calls pop pre-built
    /// `Result<ProviderEvent, ProviderError>` sequences, so tests can
    /// script transport-level `Err` items mid-stream (which
    /// [`MockProvider`] cannot).
    struct ScriptedResultProvider {
        attempts: Mutex<Vec<Vec<Result<ProviderEvent, ProviderError>>>>,
    }

    impl ScriptedResultProvider {
        fn new(attempts: Vec<Vec<Result<ProviderEvent, ProviderError>>>) -> Self {
            Self {
                attempts: Mutex::new(attempts),
            }
        }
    }

    impl Provider for ScriptedResultProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            let mut attempts = self.attempts.lock().expect("scripted provider lock");
            if attempts.is_empty() {
                return Err(ProviderError::StreamError {
                    reason: "scripted provider exhausted".to_owned(),
                    transient: None,
                });
            }
            let events = attempts.remove(0);
            Ok(Box::pin(stream::iter(events)))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
    }

    fn empty_request() -> ProviderRequest {
        ProviderRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            model: "test-model".to_owned(),
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

    fn done_event() -> ProviderEvent {
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }
    }

    /// Regression (in-band `ProviderEvent::Error` was silently ignored):
    /// a scripted mock provider emitting an Error event must fail the
    /// call with that event's typed [`ProviderError`], not the generic
    /// "stream ended without a Done event".
    #[tokio::test]
    async fn call_provider_fails_fast_with_typed_error_from_error_event() {
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "partial".to_owned(),
            },
            ProviderEvent::Error {
                error: ProviderError::QuotaExceeded,
            },
        ]]);

        let err = call_provider(&provider, empty_request(), None, None, None, 1)
            .await
            .expect_err("in-band Error event must fail the call");
        match err {
            NornError::Provider(ProviderError::QuotaExceeded) => {}
            other => panic!("expected the typed in-band provider error, got {other:?}"),
        }
    }

    /// Regression (retry re-broadcast with no marker): a retryable
    /// mid-stream failure after partial deltas must broadcast a typed
    /// [`AgentStreamRetry`] marker *before* the replay's events, so
    /// observers reset the failed attempt's partial output.
    #[tokio::test]
    async fn retry_broadcasts_stream_retry_marker_before_replay() -> Result<(), NornError> {
        let provider = ScriptedResultProvider::new(vec![
            vec![
                Ok(ProviderEvent::TextDelta {
                    text: "doomed partial".to_owned(),
                }),
                Err(ProviderError::StreamInterrupted {
                    reason: "connection reset by test".to_owned(),
                }),
            ],
            vec![
                Ok(ProviderEvent::TextDelta {
                    text: "full answer".to_owned(),
                }),
                Ok(done_event()),
            ],
        ]);
        let policy = crate::r#loop::retry::RetryPolicy {
            initial_backoff: std::time::Duration::from_millis(1),
            ..crate::r#loop::retry::RetryPolicy::default()
        };
        let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(32);
        let sender = AgentEventSender::new(tx, uuid::Uuid::nil(), "root".to_owned());

        let response = call_provider_with_retry(
            &policy,
            &provider,
            empty_request(),
            Some(&sender),
            None,
            None,
        )
        .await?;
        assert_eq!(response.text, "full answer");

        let mut received = Vec::new();
        while let Ok(event) = rx.try_recv() {
            received.push(event.event);
        }
        assert!(
            matches!(
                &received[0],
                AgentEventKind::Provider(ProviderEvent::TextDelta { text })
                    if text == "doomed partial",
            ),
            "first broadcast must be the failed attempt's partial delta",
        );
        assert!(
            matches!(
                &received[1],
                AgentEventKind::StreamRetry(AgentStreamRetry { attempt: 2 }),
            ),
            "the retry marker must precede the replay, got {received:?}",
        );
        assert!(
            matches!(
                &received[2],
                AgentEventKind::Provider(ProviderEvent::TextDelta { text })
                    if text == "full answer",
            ),
            "the replay's deltas must follow the marker",
        );
        assert!(matches!(
            &received[3],
            AgentEventKind::Provider(ProviderEvent::Done { .. }),
        ));
        assert_eq!(received.len(), 4, "no extra events are broadcast");
        Ok(())
    }

    /// A first-attempt success must broadcast no retry marker.
    #[tokio::test]
    async fn successful_first_attempt_emits_no_retry_marker() -> Result<(), NornError> {
        let provider = ScriptedResultProvider::new(vec![vec![
            Ok(ProviderEvent::TextDelta {
                text: "clean".to_owned(),
            }),
            Ok(done_event()),
        ]]);
        let policy = crate::r#loop::retry::RetryPolicy::default();
        let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(32);
        let sender = AgentEventSender::new(tx, uuid::Uuid::nil(), "root".to_owned());

        let response = call_provider_with_retry(
            &policy,
            &provider,
            empty_request(),
            Some(&sender),
            None,
            None,
        )
        .await?;
        assert_eq!(response.text, "clean");

        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(event.event, AgentEventKind::StreamRetry(_)),
                "no retry marker may be broadcast on a clean first attempt",
            );
        }
        Ok(())
    }
}
