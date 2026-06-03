//! Response classification and provider interaction.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{NornError, ProviderError};
use crate::r#loop::assembly::{AssembledResponse, assemble_response};
use crate::r#loop::schema::validate_against_schema;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::ProviderEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::traits::Provider;

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
