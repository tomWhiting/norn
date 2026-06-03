//! Typed deserialization targets and error-classification helpers for the
//! `OpenAI` Responses API SSE stream.
//!
//! Split out of [`super::sse`] so the streaming-protocol surface (the
//! `SseEvent`/`SseParser` data flow plus the `map_sse_event` dispatcher) and
//! the wire-payload schemas live in separate files. The structs and helpers
//! here are deliberately scoped `pub(super)` — they are implementation
//! details consumed only by `sse.rs` (and re-used by its tests), never
//! surfaced outside the `openai` provider.
//!
//! Three kinds of items live here:
//!
//! 1. Typed `output_item.done` payloads ([`FunctionCallItem`],
//!    [`CustomToolCallItem`]) so serde enforces the presence of the
//!    correlation `call_id`. The streaming `id` is intentionally ignored —
//!    serde silently drops unknown fields, and no downstream consumer has a
//!    legitimate use for it once `call_id` is the canonical key.
//! 2. Typed `response.failed` / `response.incomplete` payloads
//!    ([`ResponseFailedPayload`] and friends) for nested-error
//!    deserialization without hand-walking `serde_json::Value`.
//! 3. The error-code classifier ([`classify_failed_error`]) plus the
//!    `Retry-After` regex parser ([`parse_retry_after`]) — pure functions
//!    that map a typed error description into a [`ProviderError`] variant.

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;

use crate::error::ProviderError;

/// Typed deserialization target for the `item` payload of a
/// `response.output_item.done` event when `item.type == "function_call"`.
///
/// The Responses API emits two distinct identifiers on a `function_call`
/// item — the `fc_*` item `id` (output-stream identity, internal to the
/// server) and the `call_*` `call_id` (the correlation key the model
/// expects on a follow-up `function_call_output`). They are ALWAYS
/// different values. The item `id` is intentionally absent from this
/// struct: serde's default behaviour silently ignores unknown JSON
/// fields, so the wire payload deserializes cleanly without the field,
/// and no callsite has a legitimate use for the item id once `call_id`
/// is the canonical correlation key downstream.
#[derive(Debug, Deserialize)]
pub(super) struct FunctionCallItem {
    /// Required `call_*` correlation identifier echoed on
    /// `function_call_output`.
    pub(super) call_id: String,
    /// Tool name to invoke.
    pub(super) name: String,
    /// Raw JSON arguments string (the API does not parse it for us).
    pub(super) arguments: String,
}

/// Typed deserialization target for the `item` payload of a
/// `response.output_item.done` event when `item.type == "custom_tool_call"`.
///
/// Mirrors the [`FunctionCallItem`] shape, except the freeform body is
/// carried in the `input` field (no JSON envelope) rather than `arguments`.
/// The upstream `id` (`ctc_*` on the wire) is ignored for the same reason as
/// the function-call variant: `call_id` is the only identifier downstream
/// consumers correlate on, and serde silently drops unknown fields.
///
/// See `reference/codex-rs/protocol-models.rs:815-826`.
#[derive(Debug, Deserialize)]
pub(super) struct CustomToolCallItem {
    /// Required `call_*` correlation identifier echoed on
    /// `custom_tool_call_output`.
    pub(super) call_id: String,
    /// Tool name to invoke.
    pub(super) name: String,
    /// Freeform input string (the API does not parse it for us).
    pub(super) input: String,
}

/// Typed payload of a `response.failed` or `response.incomplete` SSE event.
///
/// Both events nest their detail under a top-level `response` key — the same
/// nesting `response.completed` uses. Deserializing rather than hand-walking
/// `serde_json::Value` keeps the read at the correct level and surfaces the
/// error `code` that classification depends on.
#[derive(Debug, Deserialize)]
pub(super) struct ResponseFailedPayload {
    pub(super) response: Option<ResponseErrorDetail>,
}

/// The `response` object carried by a failed or incomplete event.
#[derive(Debug, Deserialize)]
pub(super) struct ResponseErrorDetail {
    pub(super) error: Option<ApiErrorDetail>,
    pub(super) incomplete_details: Option<IncompleteDetail>,
}

/// The provider's structured error description.
#[derive(Debug, Deserialize)]
pub(super) struct ApiErrorDetail {
    pub(super) code: Option<String>,
    pub(super) message: Option<String>,
}

/// The reason an incomplete response stopped early.
#[derive(Debug, Deserialize)]
pub(super) struct IncompleteDetail {
    pub(super) reason: Option<String>,
}

/// Classifies a `response.failed` error detail into a typed [`ProviderError`].
///
/// Mirrors the Codex reference's seven-way classification (see
/// `reference/codex-rs/sse-responses.rs:346-391`). The three terminal client
/// faults — context window, quota, and invalid request — map to dedicated
/// non-retryable variants. `server_is_overloaded` and `slow_down` are
/// transient server-side back-pressure: they encode as
/// `StreamError { reason: "HTTP 503: <message>" }` so the loop-level retry
/// classifier in [`crate::r#loop::retry`] picks them up as
/// [`RetryableError::ServerError`](crate::r#loop::retry::RetryableError::ServerError)
/// and retries with exponential backoff. Any unrecognized code degrades to a
/// generic [`ProviderError::StreamError`] without the `HTTP 5xx` prefix —
/// unknown failure modes do not opt in to automatic retry.
pub(super) fn classify_failed_error(detail: &ApiErrorDetail) -> ProviderError {
    match detail.code.as_deref().unwrap_or_default() {
        "rate_limit_exceeded" => ProviderError::RateLimited {
            retry_after: detail.message.as_deref().and_then(parse_retry_after),
        },
        "context_length_exceeded" => ProviderError::ContextWindowExceeded,
        "insufficient_quota" => ProviderError::QuotaExceeded,
        "invalid_prompt" => ProviderError::InvalidRequest {
            message: message_or(detail.message.as_deref(), "Invalid request."),
        },
        "cyber_policy" => ProviderError::InvalidRequest {
            message: message_or(
                detail.message.as_deref(),
                "This request has been flagged for possible cybersecurity risk.",
            ),
        },
        // Transient server-side back-pressure. Encoded with an `HTTP 503:`
        // prefix so `classify_provider_error` in the loop's retry module
        // matches them as `RetryableError::ServerError` and the policy's
        // exponential backoff kicks in.
        "server_is_overloaded" => ProviderError::StreamError {
            reason: format!(
                "HTTP 503: {}",
                message_or(detail.message.as_deref(), "server is overloaded"),
            ),
        },
        "slow_down" => ProviderError::StreamError {
            reason: format!(
                "HTTP 503: {}",
                message_or(detail.message.as_deref(), "slow down"),
            ),
        },
        // Any unknown code surfaces as a stream error carrying the provider's
        // message verbatim. No `HTTP 5xx` prefix is added — opting an unknown
        // error into automatic retry would silently amplify novel failure
        // modes.
        _ => ProviderError::StreamError {
            reason: message_or(detail.message.as_deref(), "response.failed"),
        },
    }
}

/// Returns the trimmed `message` if non-empty, otherwise `fallback`.
fn message_or(message: Option<&str>, fallback: &str) -> String {
    message
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map_or_else(|| fallback.to_owned(), ToOwned::to_owned)
}

/// Parses a `Retry-After` duration from a `rate_limit_exceeded` error message.
///
/// Matches the Codex pattern (`reference/codex-rs/sse-responses.rs:521-545`):
/// e.g. `"Rate limit reached. Please try again in 11.054s."` yields
/// `Duration::from_secs_f64(11.054)`. Returns `None` when the message lacks a
/// parseable duration, when the regex itself fails to compile, or when the
/// extracted value cannot be represented as a [`Duration`].
fn parse_retry_after(message: &str) -> Option<Duration> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"(?i)try again in\s*(\d+(?:\.\d+)?)\s*(s|ms|seconds?)").ok())
        .as_ref()?;

    let captures = re.captures(message)?;
    let value: f64 = captures.get(1)?.as_str().parse().ok()?;
    let unit = captures.get(2)?.as_str().to_ascii_lowercase();

    let seconds = if unit == "ms" { value / 1000.0 } else { value };
    Duration::try_from_secs_f64(seconds).ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry_after_seconds() {
        let d = parse_retry_after("Please try again in 11.054s.").expect("parses");
        assert!((d.as_secs_f64() - 11.054).abs() < 1e-6);
    }

    #[test]
    fn parse_retry_after_milliseconds() {
        let d = parse_retry_after("Try again in 500ms").expect("parses");
        assert_eq!(d, Duration::from_millis(500));
    }

    #[test]
    fn parse_retry_after_returns_none_when_message_lacks_a_duration() {
        assert!(parse_retry_after("rate limited").is_none());
    }

    #[test]
    fn message_or_returns_message_when_non_empty() {
        assert_eq!(message_or(Some("hello"), "fallback"), "hello");
    }

    #[test]
    fn message_or_returns_fallback_when_none() {
        assert_eq!(message_or(None, "fallback"), "fallback");
    }

    #[test]
    fn message_or_returns_fallback_when_blank() {
        assert_eq!(message_or(Some("   "), "fallback"), "fallback");
    }

    #[test]
    fn classify_unknown_code_does_not_get_http_prefix() {
        let detail = ApiErrorDetail {
            code: Some("future_error".to_string()),
            message: Some("weird".to_string()),
        };
        match classify_failed_error(&detail) {
            ProviderError::StreamError { reason } => assert_eq!(reason, "weird"),
            other => panic!("expected StreamError, got {other:?}"),
        }
    }
}
