//! Structural extraction helpers for reconciliation frames.

use serde_json::Value;

use super::{ResponseDeltaChannel, ResponseItemIdentity, ResponseReconciliationError};
use crate::provider::openai::response_contract::{OutputItemActionability, public_output_item};
use crate::provider::openai::sse::SseEvent;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

pub(super) fn required_sequence_number(
    event: &SseEvent,
) -> Result<u64, ResponseReconciliationError> {
    match event.data.get("sequence_number") {
        None => Err(ResponseReconciliationError::MissingSequenceNumber),
        Some(value) => value
            .as_u64()
            .ok_or(ResponseReconciliationError::InvalidSequenceNumber),
    }
}

pub(super) fn required_object<'a>(
    event: &'a SseEvent,
    event_type: &'static str,
    field: &'static str,
) -> Result<&'a Value, ResponseReconciliationError> {
    event
        .data
        .get(field)
        .filter(|value| value.is_object())
        .ok_or(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}

pub(super) fn required_u64(
    event: &SseEvent,
    event_type: &'static str,
    field: &'static str,
) -> Result<u64, ResponseReconciliationError> {
    event
        .data
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}

pub(super) fn validate_output_text_delta_logprobs(
    event: &SseEvent,
) -> Result<(), ResponseReconciliationError> {
    const EVENT_TYPE: &str = "response.output_text.delta";
    let logprobs = event.data.get("logprobs").and_then(Value::as_array).ok_or(
        ResponseReconciliationError::InvalidEnvelopeField {
            event_type: EVENT_TYPE,
            field: "logprobs",
        },
    )?;

    for logprob in logprobs {
        if logprob.get("token").and_then(Value::as_str).is_none() {
            return Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: EVENT_TYPE,
                field: "logprobs[].token",
            });
        }
        if logprob
            .get("logprob")
            .is_none_or(|value| !value.is_number())
        {
            return Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: EVENT_TYPE,
                field: "logprobs[].logprob",
            });
        }

        let Some(top_logprobs) = logprob.get("top_logprobs") else {
            continue;
        };
        let Some(top_logprobs) = top_logprobs.as_array() else {
            return Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: EVENT_TYPE,
                field: "logprobs[].top_logprobs",
            });
        };
        for candidate in top_logprobs {
            if !candidate.is_object() {
                return Err(ResponseReconciliationError::InvalidEnvelopeField {
                    event_type: EVENT_TYPE,
                    field: "logprobs[].top_logprobs[]",
                });
            }
            if candidate
                .get("token")
                .is_some_and(|value| !value.is_string())
            {
                return Err(ResponseReconciliationError::InvalidEnvelopeField {
                    event_type: EVENT_TYPE,
                    field: "logprobs[].top_logprobs[].token",
                });
            }
            if candidate
                .get("logprob")
                .is_some_and(|value| !value.is_number())
            {
                return Err(ResponseReconciliationError::InvalidEnvelopeField {
                    event_type: EVENT_TYPE,
                    field: "logprobs[].top_logprobs[].logprob",
                });
            }
        }
    }
    Ok(())
}

pub(super) fn envelope_identity(
    event: &SseEvent,
) -> Result<ResponseItemIdentity, ResponseReconciliationError> {
    let item_id = event.data.get("item_id").and_then(Value::as_str).ok_or(
        ResponseReconciliationError::InvalidEnvelopeField {
            event_type: "response delta",
            field: "item_id",
        },
    )?;
    let output_index = required_u64(event, "response delta", "output_index")?;
    Ok(ResponseItemIdentity {
        item_id: Some(item_id.to_owned()),
        output_index,
    })
}

pub(super) fn parse_item(
    raw: Value,
    event_type: &'static str,
) -> Result<ResponseItem, ResponseReconciliationError> {
    ResponseItem::from_value(raw).map_err(|error| ResponseReconciliationError::MalformedItem {
        event_type,
        reason: error.to_string(),
    })
}

pub(super) fn announced_item_id<'a>(
    raw: &'a Value,
    item_type: &str,
    event_type: &'static str,
) -> Result<Option<&'a str>, ResponseReconciliationError> {
    match raw.get("id") {
        Some(Value::String(item_id)) => Ok(Some(item_id)),
        None if item_type_allows_missing_id(item_type) => Ok(None),
        None | Some(_) => Err(ResponseReconciliationError::InvalidEnvelopeField {
            event_type,
            field: "item.id",
        }),
    }
}

pub(super) fn item_type_allows_missing_id(item_type: &str) -> bool {
    matches!(item_type, "function_call" | "custom_tool_call")
}

pub(super) fn embedded_item_id(
    event: &SseEvent,
    item: &ResponseItem,
) -> Result<Option<String>, ResponseReconciliationError> {
    let embedded = item.id();
    let envelope = match event.data.get("item_id") {
        None => None,
        Some(Value::String(item_id)) => Some(item_id.as_str()),
        Some(_) => {
            return Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.output_item.done",
                field: "item_id",
            });
        }
    };
    if let (Some(envelope), Some(embedded)) = (envelope, embedded)
        && envelope != embedded
    {
        return Err(ResponseReconciliationError::EmbeddedItemIdConflict);
    }
    Ok(embedded.or(envelope).map(str::to_owned))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputItemSupport {
    Inert,
    SupportedExecutable,
    UnsupportedExecutable,
    Unknown,
}

pub(super) fn output_item_support(
    item_type: &str,
    raw: &Value,
) -> Result<OutputItemSupport, ResponseReconciliationError> {
    let Some(entry) = public_output_item(item_type) else {
        return Ok(OutputItemSupport::Unknown);
    };
    match entry.actionability() {
        OutputItemActionability::Inert => Ok(OutputItemSupport::Inert),
        OutputItemActionability::Conditional => conditional_output_item_support(item_type, raw),
        OutputItemActionability::Executable
            if matches!(item_type, "function_call" | "custom_tool_call") =>
        {
            Ok(OutputItemSupport::SupportedExecutable)
        }
        OutputItemActionability::Executable => Ok(OutputItemSupport::UnsupportedExecutable),
    }
}

fn conditional_output_item_support(
    item_type: &str,
    raw: &Value,
) -> Result<OutputItemSupport, ResponseReconciliationError> {
    match item_type {
        "tool_search_call" => match required_actionability_string(
            raw.get("execution"),
            "tool_search_call",
            "execution",
        )? {
            "server" => Ok(OutputItemSupport::Inert),
            "client" => Ok(OutputItemSupport::UnsupportedExecutable),
            _ => Err(ResponseReconciliationError::InvalidAuthoritativeItemField {
                item_type: "tool_search_call",
                field: "execution",
            }),
        },
        "shell_call" => shell_call_support(raw),
        _ => Ok(OutputItemSupport::UnsupportedExecutable),
    }
}

fn shell_call_support(raw: &Value) -> Result<OutputItemSupport, ResponseReconciliationError> {
    let environment = raw.get("environment").ok_or(
        ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "shell_call",
            field: "environment",
        },
    )?;
    if environment.is_null() {
        return Ok(OutputItemSupport::UnsupportedExecutable);
    }
    let environment = environment.as_object().ok_or(
        ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "shell_call",
            field: "environment",
        },
    )?;
    match required_actionability_string(environment.get("type"), "shell_call", "environment.type")?
    {
        "local" => Ok(OutputItemSupport::UnsupportedExecutable),
        "container_reference" => Ok(OutputItemSupport::Inert),
        _ => Err(ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "shell_call",
            field: "environment.type",
        }),
    }
}

fn required_actionability_string<'a>(
    value: Option<&'a Value>,
    item_type: &'static str,
    field: &'static str,
) -> Result<&'a str, ResponseReconciliationError> {
    match value {
        None => {
            Err(ResponseReconciliationError::MissingAuthoritativeItemField { item_type, field })
        }
        Some(Value::String(value)) => Ok(value),
        Some(_) => {
            Err(ResponseReconciliationError::InvalidAuthoritativeItemField { item_type, field })
        }
    }
}

pub(super) fn item_is_explicitly_unresolved(item: &ResponseItem) -> bool {
    match item.raw().get("status") {
        None => false,
        Some(Value::String(status)) => status != "completed",
        Some(_) => true,
    }
}

pub(super) fn authoritative_items_failure(
    items: &[ResponseTranscriptItem],
    reject_unresolved: bool,
) -> Result<Option<ResponseReconciliationError>, ResponseReconciliationError> {
    for transcript in items {
        match output_item_support(transcript.item.item_type(), transcript.item.raw())? {
            OutputItemSupport::Unknown => {
                return Ok(Some(ResponseReconciliationError::UnknownOutputItemType {
                    retained_items: items.to_vec(),
                }));
            }
            OutputItemSupport::UnsupportedExecutable => {
                return Ok(Some(
                    ResponseReconciliationError::UnsupportedExecutableItem {
                        retained_items: items.to_vec(),
                    },
                ));
            }
            OutputItemSupport::SupportedExecutable
                if reject_unresolved && item_is_explicitly_unresolved(&transcript.item) =>
            {
                return Ok(Some(
                    ResponseReconciliationError::UnresolvedActionableItem {
                        retained_items: items.to_vec(),
                    },
                ));
            }
            OutputItemSupport::Inert | OutputItemSupport::SupportedExecutable => {}
        }
    }
    Ok(None)
}

pub(super) fn parse_terminal_items(
    output: &[Value],
    sequence_number: u64,
) -> Result<Vec<ResponseTranscriptItem>, ResponseReconciliationError> {
    output
        .iter()
        .enumerate()
        .map(|(position, raw)| {
            let output_index = u64::try_from(position).map_err(|error| {
                ResponseReconciliationError::OutputIndexOverflow {
                    reason: error.to_string(),
                }
            })?;
            let item = parse_item(raw.clone(), "terminal response")?;
            let item_id = item.id().map(str::to_owned);
            Ok(ResponseTranscriptItem {
                item,
                provenance: ResponseStreamProvenance {
                    item_id,
                    output_index: Some(output_index),
                    content_index: None,
                    sequence_number: Some(sequence_number),
                },
            })
        })
        .collect()
}

pub(super) fn delta_event_type(channel: fn(u64) -> ResponseDeltaChannel) -> &'static str {
    match channel(0) {
        ResponseDeltaChannel::OutputText(_) => "response.output_text.delta",
        ResponseDeltaChannel::Refusal(_) => "response.refusal.delta",
        ResponseDeltaChannel::ReasoningText(_) => "response.reasoning_text.delta",
        _ => "response delta",
    }
}

pub(super) fn delta_event_name(channel: ResponseDeltaChannel) -> &'static str {
    match channel {
        ResponseDeltaChannel::OutputText(_) => "response.output_text.delta",
        ResponseDeltaChannel::Refusal(_) => "response.refusal.delta",
        ResponseDeltaChannel::FunctionCallArguments => "response.function_call_arguments.delta",
        ResponseDeltaChannel::CustomToolCallInput => "response.custom_tool_call_input.delta",
        ResponseDeltaChannel::ReasoningSummaryText(_) => "response.reasoning_summary_text.delta",
        ResponseDeltaChannel::ReasoningText(_) => "response.reasoning_text.delta",
    }
}
