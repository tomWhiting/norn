//! Response classification and provider interaction.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{NornError, ProviderError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::{AssembledResponse, assemble_response};
use crate::r#loop::config::TruncationKind;
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::schema::validate_against_schema;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ProviderRequest;
use crate::provider::traits::Provider;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::tool::envelope::split_envelope_fields;

/// Persist the `loop.truncated` Custom event marking a truncated no-schema
/// response (REVIEW item 5).
///
/// The partial text, per-call usage, and stop reason are already persisted
/// on the preceding `AssistantMessage` event; the Custom event marks the
/// abort for observers. The runner then returns
/// [`AgentStepResult::Truncated`](crate::r#loop::config::AgentStepResult),
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
                StopReason::EndTurn | StopReason::ToolUse => {}
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
pub(super) async fn call_provider(
    provider: &dyn Provider,
    request: ProviderRequest,
    event_tx: Option<&AgentEventSender>,
) -> Result<AssembledResponse, NornError> {
    let mut stream = provider.stream(request)?;
    let mut events: Vec<ProviderEvent> = Vec::new();

    while let Some(result) = stream.next().await {
        let event = result?;
        if let Some(sender) = event_tx {
            sender.send(event.clone());
        }
        events.push(event);
    }

    assemble_response(&events).ok_or_else(|| {
        NornError::Provider(ProviderError::StreamError {
            reason: "provider stream ended without a Done event".to_string(),
        })
    })
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
            text: String::new(),
            thinking: String::new(),
            tool_calls: vec![AssembledToolCall {
                call_id: "call_1".to_owned(),
                name: "structured_output".to_owned(),
                arguments: arguments.to_owned(),
                kind: ToolCallKind::Function,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: None,
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
}
