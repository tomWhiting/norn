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
//! 1. Typed `response.failed` / `response.incomplete` payloads
//!    ([`ResponseFailedPayload`] and friends) for nested-error
//!    deserialization without hand-walking `serde_json::Value`.
//! 2. The classifiers — pure functions mapping typed wire payloads into
//!    domain values: [`classify_failed_error`] (a `response.failed` error
//!    description into a [`ProviderError`] variant),
//!    [`incomplete_stop_reason`] (a `response.incomplete` reason into a
//!    typed [`StopReason`]), and the `Retry-After` regex parser
//!    ([`parse_retry_after`]).

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{ProviderError, TransientKind};
use crate::provider::events::StopReason;

use super::opaque_discriminator::{OpaqueDiscriminator, opaque_tag};

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

/// Optional ChatGPT/Codex turn directive on a completed response.
#[derive(Debug, Deserialize)]
struct CompletedTurnDirective {
    #[serde(default)]
    end_turn: Option<bool>,
}

/// Maps a completed Responses terminal onto its loop stop reason.
///
/// Locally actionable calls always require a follow-up request so their
/// results can be returned. Without one, an explicit Codex `false` continues
/// the same user turn; `true`, `null`, and absence retain the public Responses
/// terminal behavior. Deserialization is deliberately strict when the field is
/// present so an unknown wire shape cannot silently end a turn.
pub(super) fn completed_stop_reason(
    response: &Value,
    has_actionable_call: bool,
    codex_overlay: bool,
) -> Result<StopReason, ProviderError> {
    if !codex_overlay && response.get("end_turn").is_some() {
        return Err(ProviderError::ResponseParseError {
            reason: "public Responses backend returned a Codex-only end_turn directive".to_owned(),
        });
    }
    let directive = CompletedTurnDirective::deserialize(response).map_err(|_source| {
        ProviderError::ResponseParseError {
            reason: "completed response carried an invalid end_turn directive".to_owned(),
        }
    })?;

    if has_actionable_call {
        Ok(StopReason::ToolUse)
    } else if directive.end_turn == Some(false) {
        Ok(StopReason::ContinueTurn)
    } else {
        Ok(StopReason::EndTurn)
    }
}

/// Classifies a `response.failed` error detail into a typed [`ProviderError`].
///
/// Thin label-binding wrapper over [`classify_error_code`], which carries the
/// shared code table for every wire envelope that delivers a provider error
/// code.
pub(super) fn classify_failed_error(detail: &ApiErrorDetail) -> ProviderError {
    classify_error_code(detail, "response.failed")
}

/// Classifies a standalone `error` SSE event (`ResponseErrorEvent`).
///
/// The standalone event carries `code`/`message`/`param`/`sequence_number`
/// at the TOP level — unlike `response.failed`, which nests its detail under
/// `response.error`. It rides the same code table as `response.failed` so a
/// given provider code classifies identically regardless of which wire
/// envelope delivered it. Before this classifier existed, the standalone arm
/// discarded the payload and hard-coded `transient: None` — which turned the
/// provider's own "You can retry your request" `server_error` into an
/// unretryable, undiagnosable death (the 2026-07-24 fleet incident: fourteen
/// headless sessions killed by a stochastic ~1%/turn provider transient the
/// default retry policy would have absorbed).
pub(super) fn classify_standalone_error(detail: &ApiErrorDetail) -> ProviderError {
    if detail.code.is_none() {
        return ProviderError::StreamError {
            reason: "provider returned a standalone Responses error event without an error code"
                .to_owned(),
            transient: None,
        };
    }
    classify_error_code(detail, "standalone-error")
}

/// The shared provider-error-code table.
///
/// Mirrors the Codex reference's seven-way classification (see
/// `reference/codex-rs/sse-responses.rs:346-391`). The three terminal client
/// faults — context window, quota, and invalid request — map to dedicated
/// non-retryable variants. `server_is_overloaded` and `slow_down` are
/// transient server-side back-pressure: they carry an explicit
/// `transient: Some(ServerError { status: 503 })` (the HTTP-equivalent
/// condition — the SSE frame itself carries no status) so the public
/// taxonomy classifies them retryable and the loop-level retry policy in
/// [`crate::agent_loop::retry`] matches them as
/// [`RetryableError::ServerError`](crate::agent_loop::retry::RetryableError::ServerError)
/// with exponential backoff. `server_error` is the provider's generic
/// internal failure whose message invites retry ("You can retry your
/// request" — captured strike payload, 2026-07-24); it carries
/// `ServerError { status: 0 }`, the any-5xx wildcard the default retry
/// policy matches, plus the bounded request-id token extracted from the
/// message when one is present. Any unrecognized code degrades to a terminal
/// [`ProviderError::StreamError`] (`transient: None`) — unknown failure
/// modes do not opt in to automatic retry.
fn classify_error_code(detail: &ApiErrorDetail, event_label: &str) -> ProviderError {
    match detail.code.as_deref() {
        Some("rate_limit_exceeded") => ProviderError::RateLimited {
            retry_after: detail.message.as_deref().and_then(parse_retry_after),
        },
        Some("context_length_exceeded") => ProviderError::ContextWindowExceeded,
        Some("insufficient_quota") => ProviderError::QuotaExceeded,
        Some("invalid_prompt") => ProviderError::InvalidRequest {
            message: "provider rejected the request as invalid_prompt".to_owned(),
        },
        Some("cyber_policy") => ProviderError::InvalidRequest {
            message: "provider rejected the request under its cybersecurity policy".to_owned(),
        },
        // Transient server-side back-pressure: retryable with the
        // HTTP-equivalent 503 server-error kind, set structurally.
        Some("server_is_overloaded") => ProviderError::StreamError {
            reason: "provider reported server_is_overloaded".to_owned(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        },
        Some("slow_down") => ProviderError::StreamError {
            reason: "provider reported slow_down".to_owned(),
            transient: Some(TransientKind::ServerError { status: 503 }),
        },
        Some("server_error") => {
            let reason = match detail.message.as_deref().and_then(extract_request_id) {
                Some(request_id) => {
                    format!("provider reported server_error; request id {request_id}")
                }
                None => "provider reported server_error".to_owned(),
            };
            ProviderError::StreamError {
                reason,
                transient: Some(TransientKind::ServerError { status: 0 }),
            }
        }
        Some(unknown) => unknown_error_code(event_label, unknown),
        None => ProviderError::StreamError {
            reason: format!("provider returned {event_label} without an error code"),
            transient: None,
        },
    }
}

fn unknown_error_code(event_label: &str, raw_code: &str) -> ProviderError {
    match opaque_tag(OpaqueDiscriminator::FailedCode, raw_code) {
        Ok(tag) => ProviderError::StreamError {
            reason: format!("provider returned unknown {event_label} code [opaque:{tag}]"),
            transient: None,
        },
        Err(_) => ProviderError::ResponseParseError {
            reason: "could not initialize the non-disclosing failed-code diagnostic".to_owned(),
        },
    }
}

/// Extracts the bounded request-correlation token from a provider error
/// message, when one is present.
///
/// Only a pattern-matched id (`req_*` or a UUID) ever crosses into the error
/// reason — never the provider's free-text message, which is
/// authority-controlled and may echo prompts or credentials. This satisfies
/// the incident-forensics requirement for a "bounded, sanitized provider
/// request ID" without weakening the no-body-disclosure posture.
fn extract_request_id(message: &str) -> Option<&str> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE
        .get_or_init(|| {
            Regex::new(
                r"req_[A-Za-z0-9]+|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            )
            .ok()
        })
        .as_ref()?;
    re.find(message).map(|found| found.as_str())
}

/// Maps the `incomplete_details.reason` of a `response.incomplete` SSE
/// event onto a typed [`StopReason`].
///
/// A `response.incomplete` event is the Responses API's terminal frame for
/// a deterministic model-side stop with partial output — NOT a transport
/// fault. The API documents exactly two incomplete reasons, and both map
/// onto the same [`StopReason`] values the Claude adapter produces, so the
/// loop's truncation handling (`ResponseClass::Truncated` →
/// `AgentStepResult::Truncated`) is reachable identically from both
/// providers:
///
/// * `max_output_tokens` → [`StopReason::MaxTokens`]
/// * `content_filter` → [`StopReason::ContentFilter`]
///
/// Any other (or missing) reason is a wire contract this client does not
/// understand. It surfaces as [`ProviderError::ResponseParseError`] —
/// classified [`ErrorClass::Terminal`](crate::error::ErrorClass) — carrying
/// only a locally generated process-scoped opaque tag, never the
/// provider-supplied reason text. Guessing `MaxTokens` would silently mislabel
/// an unknown stop condition, and any retryable classification would replay a
/// deterministic stop; a terminal error stops the run honestly instead.
pub(super) fn incomplete_stop_reason(reason: Option<&str>) -> Result<StopReason, ProviderError> {
    match reason {
        Some("max_output_tokens") => Ok(StopReason::MaxTokens),
        Some("content_filter") => Ok(StopReason::ContentFilter),
        Some(unknown) => unknown_incomplete_reason(unknown),
        None => Err(ProviderError::ResponseParseError {
            reason: "response.incomplete carried no incomplete_details.reason".to_string(),
        }),
    }
}

fn unknown_incomplete_reason(raw_reason: &str) -> Result<StopReason, ProviderError> {
    let Ok(tag) = opaque_tag(OpaqueDiscriminator::IncompleteReason, raw_reason) else {
        return Err(ProviderError::ResponseParseError {
            reason: "could not initialize the non-disclosing incomplete-reason diagnostic"
                .to_owned(),
        });
    };
    Err(ProviderError::ResponseParseError {
        reason: format!("response.incomplete carried an unknown reason [opaque:{tag}]"),
    })
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
mod tests {
    use std::io;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_retry_after_seconds() -> TestResult {
        let Some(d) = parse_retry_after("Please try again in 11.054s.") else {
            return Err(io::Error::other("seconds duration did not parse").into());
        };
        assert!((d.as_secs_f64() - 11.054).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn parse_retry_after_milliseconds() -> TestResult {
        let Some(d) = parse_retry_after("Try again in 500ms") else {
            return Err(io::Error::other("millisecond duration did not parse").into());
        };
        assert_eq!(d, Duration::from_millis(500));
        Ok(())
    }

    #[test]
    fn parse_retry_after_returns_none_when_message_lacks_a_duration() {
        assert!(parse_retry_after("rate limited").is_none());
    }

    #[test]
    fn classify_unknown_code_stays_terminal_and_opaque() -> TestResult {
        let detail = ApiErrorDetail {
            code: Some("future_error".to_string()),
            message: Some("weird".to_string()),
        };
        let ProviderError::StreamError { reason, transient } = classify_failed_error(&detail)
        else {
            return Err(io::Error::other("unknown code did not produce StreamError").into());
        };
        assert!(reason.starts_with("provider returned unknown response.failed code [opaque:"));
        assert!(!reason.contains("future_error"));
        assert_eq!(transient, None, "unknown codes must not opt into retry");
        Ok(())
    }

    #[test]
    fn distinct_unknown_failed_codes_have_distinct_opaque_tags() -> TestResult {
        let first = classify_failed_error(&ApiErrorDetail {
            code: Some("private-first\r\ncode".to_owned()),
            message: None,
        });
        let second = classify_failed_error(&ApiErrorDetail {
            code: Some("private-second\r\ncode".to_owned()),
            message: None,
        });
        let ProviderError::StreamError {
            reason: first_reason,
            transient: None,
        } = first
        else {
            return Err(io::Error::other("first unknown code was not terminal").into());
        };
        let ProviderError::StreamError {
            reason: second_reason,
            transient: None,
        } = second
        else {
            return Err(io::Error::other("second unknown code was not terminal").into());
        };

        assert_ne!(first_reason, second_reason);
        assert!(!first_reason.contains("private-first"));
        assert!(!second_reason.contains("private-second"));
        assert!(!first_reason.contains('\r'));
        assert!(!first_reason.contains('\n'));
        assert!(!second_reason.contains('\r'));
        assert!(!second_reason.contains('\n'));
        Ok(())
    }

    #[test]
    fn failed_error_messages_never_expose_authority_text() {
        for code in [
            "invalid_prompt",
            "cyber_policy",
            "server_is_overloaded",
            "slow_down",
            "unknown",
        ] {
            let detail = ApiErrorDetail {
                code: Some(code.to_owned()),
                message: Some("sentinel-private-provider-message".to_owned()),
            };
            let error = classify_failed_error(&detail);
            assert!(
                !error
                    .to_string()
                    .contains("sentinel-private-provider-message")
            );
            assert!(!format!("{error:?}").contains("sentinel-private-provider-message"));
        }
    }

    #[test]
    fn incomplete_error_never_exposes_unknown_reason_text() -> TestResult {
        let Err(error) = incomplete_stop_reason(Some("sentinel-private-incomplete-reason")) else {
            return Err(io::Error::other("unknown incomplete reason was accepted").into());
        };

        assert!(
            !error
                .to_string()
                .contains("sentinel-private-incomplete-reason")
        );
        assert!(!format!("{error:?}").contains("sentinel-private-incomplete-reason"));
        Ok(())
    }

    #[test]
    fn distinct_unknown_incomplete_reasons_have_distinct_opaque_tags() -> TestResult {
        let Err(first) = incomplete_stop_reason(Some("private-incomplete-first")) else {
            return Err(io::Error::other("first unknown incomplete reason was accepted").into());
        };
        let Err(second) = incomplete_stop_reason(Some("private-incomplete-second")) else {
            return Err(io::Error::other("second unknown incomplete reason was accepted").into());
        };

        assert_ne!(first.to_string(), second.to_string());
        assert!(!first.to_string().contains("private-incomplete-first"));
        assert!(!second.to_string().contains("private-incomplete-second"));
        Ok(())
    }
}
