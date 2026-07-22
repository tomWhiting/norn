//! Strict terminal projection for reconciled Responses streams.

use serde_json::Value;

use super::request::CATALOG_BACKEND_CODEX_SUBSCRIPTION;
use super::response_reconciler::{ReconcileUpdate, TerminalOutputPolicy};
use super::sse::SseEvent;
use super::sse_types::{completed_stop_reason, incomplete_stop_reason};
use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::response_item::ResponseItem;
use crate::provider::usage::Usage;

/// Terminal dialect selected from the trusted provider backend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ResponsesDialect {
    /// Public Responses semantics; private Codex fields fail closed.
    #[default]
    Public,
    /// ChatGPT/Codex subscription semantics, including `end_turn`.
    Codex,
}

impl ResponsesDialect {
    /// Selects private overlay semantics only for the trusted Codex backend.
    pub(super) fn for_catalog_backend(catalog_backend: &str) -> Self {
        if catalog_backend == CATALOG_BACKEND_CODEX_SUBSCRIPTION {
            Self::Codex
        } else {
            Self::Public
        }
    }

    pub(super) const fn terminal_output_policy(self) -> TerminalOutputPolicy {
        match self {
            Self::Public => TerminalOutputPolicy::StrictPublic,
            Self::Codex => TerminalOutputPolicy::CodexCompletedItemsFallback,
        }
    }
}

/// Decode one reconciled completed or incomplete event into the legacy
/// provider terminal projection.
pub(super) fn decode_terminal(
    event: &SseEvent,
    update: &ReconcileUpdate,
    dialect: ResponsesDialect,
) -> Result<ProviderEvent, ProviderError> {
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err(parse_error(
            "terminal event did not complete reconciliation",
        ));
    };
    let response = required_object(&event.data, "response")?;
    let response_id = required_string(response, "id")?.to_owned();
    let expected_status = match event.event_type.as_str() {
        "response.completed" => "completed",
        "response.incomplete" => "incomplete",
        _ => return Err(parse_error("non-terminal event reached terminal decoder")),
    };
    if let Some(status) = response.get("status")
        && status.as_str() != Some(expected_status)
    {
        return Err(parse_error(
            "terminal response carried an inconsistent status",
        ));
    }
    let usage = decode_usage(response.get("usage"))?;
    let stop_reason = if expected_status == "incomplete" {
        let reason = response
            .get("incomplete_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str);
        incomplete_stop_reason(reason)?
    } else {
        completed_stop_reason(
            response,
            items.iter().any(|item| {
                matches!(
                    &item.item,
                    ResponseItem::FunctionCall(_) | ResponseItem::CustomToolCall(_)
                )
            }),
            dialect == ResponsesDialect::Codex,
        )?
    };

    Ok(ProviderEvent::Done {
        stop_reason,
        usage,
        response_id: Some(response_id),
    })
}

fn decode_usage(raw: Option<&Value>) -> Result<Usage, ProviderError> {
    let Some(raw) = raw else {
        // The public Response schema makes usage optional. The exact absence
        // remains on the raw terminal envelope; P6 owns promoting that
        // presence bit through aggregate accounting.
        return Ok(Usage::default());
    };
    let usage = raw
        .as_object()
        .ok_or_else(|| parse_error("terminal response usage was not an object"))?;
    let required_token = |field: &'static str| {
        usage
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| parse_error("terminal response usage omitted a required token field"))
    };
    let input_details = usage
        .get("input_tokens_details")
        .and_then(Value::as_object)
        .ok_or_else(|| parse_error("terminal response usage omitted input-token details"))?;
    let cached_tokens = required_nested_token(input_details, "cached_tokens")?;
    let cache_write_tokens = required_nested_token(input_details, "cache_write_tokens")?;
    let output_details = usage
        .get("output_tokens_details")
        .and_then(Value::as_object)
        .ok_or_else(|| parse_error("terminal response usage omitted output-token details"))?;
    required_nested_token(output_details, "reasoning_tokens")?;
    required_token("total_tokens")?;
    Ok(Usage {
        input_tokens: required_token("input_tokens")?,
        output_tokens: required_token("output_tokens")?,
        cache_read_tokens: cached_tokens,
        cache_write_tokens,
        cost_usd: None,
    })
}

fn required_nested_token(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<u64, ProviderError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| parse_error("terminal response usage detail omitted a required token field"))
}

fn required_object<'a>(value: &'a Value, field: &str) -> Result<&'a Value, ProviderError> {
    value
        .get(field)
        .filter(|nested| nested.is_object())
        .ok_or_else(|| parse_error("terminal Responses event omitted its response object"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| parse_error("terminal response omitted a required string field"))
}

fn parse_error(reason: &'static str) -> ProviderError {
    ProviderError::ResponseParseError {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests;
