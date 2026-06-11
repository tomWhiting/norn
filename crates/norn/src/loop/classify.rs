//! Response classification and provider interaction.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{NornError, ProviderError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::{AssembledResponse, assemble_response};
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::schema::validate_against_schema;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ProviderRequest;
use crate::provider::traits::Provider;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Why a response was cut off before the model finished its turn.
///
/// Only the two abnormal [`StopReason`] variants are representable here, so
/// a [`ResponseClass::Truncated`] can never carry a normal stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TruncationKind {
    /// The model hit its maximum output-token limit mid-response.
    MaxTokens,
    /// The provider's content filter cut the response off.
    ContentFilter,
}

impl TruncationKind {
    /// Stable string form used in session events and error messages.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::ContentFilter => "content_filter",
        }
    }
}

/// Persist a `loop.truncated` Custom event and build the typed error the
/// runner returns for a truncated no-schema response (REVIEW item 5).
///
/// The partial text, per-call usage, and stop reason are already persisted
/// on the preceding `AssistantMessage` event; the Custom event marks the
/// abort for observers and the returned [`NornError`] makes the truncation
/// impossible to mistake for a successful completion. The error is
/// [`ProviderError::Truncated`], which never classifies as retryable —
/// truncation and content-filter stops are deterministic, so retrying
/// the identical request reproduces the same stop.
///
/// # Errors
///
/// Returns [`SessionError`] if the Custom event cannot be appended.
pub(super) async fn truncation_failure(
    store: &EventStore,
    hooks: Option<&HookRegistry>,
    kind: TruncationKind,
    partial_text: &str,
    iterations: u32,
) -> Result<NornError, SessionError> {
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
    .await?;
    Ok(NornError::Provider(ProviderError::Truncated {
        stop_reason: kind.as_str().to_string(),
        reason: "model output truncated; the response is an incomplete \
                 fragment — partial text and usage are persisted in the \
                 session event store"
            .to_string(),
    }))
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
