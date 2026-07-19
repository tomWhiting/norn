//! Non-persisted provider state scoped to one logical user turn.

use std::sync::{Arc, OnceLock};

use reqwest::header::HeaderValue;
use serde_json::Value;

use crate::error::ProviderError;

use super::request::SecretString;

/// Private Codex sticky-routing header carried between requests in one turn.
pub(crate) const CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";

/// Provider transport context shared by every request in one logical user turn.
///
/// The agent loop creates a fresh value for each step. Clones share only live
/// transport state; nothing in this type is serializable or persisted. Dropping
/// the step therefore clears any captured sticky-routing token naturally.
#[derive(Clone)]
pub struct ProviderTurnContext {
    inner: Arc<ProviderTurnContextInner>,
}

struct ProviderTurnContextInner {
    session_id: Option<String>,
    turn_id: String,
    codex_turn_state: OnceLock<SecretString>,
}

impl std::fmt::Debug for ProviderTurnContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderTurnContext")
            .field("session_id_present", &self.inner.session_id.is_some())
            .field("turn_id_present", &!self.inner.turn_id.is_empty())
            .field(
                "codex_turn_state_present",
                &self.inner.codex_turn_state.get().is_some(),
            )
            .finish()
    }
}

impl Default for ProviderTurnContext {
    fn default() -> Self {
        Self::new(None, String::new())
    }
}

impl ProviderTurnContext {
    /// Creates a context for direct library use from caller-owned identities.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when either identity is empty.
    pub fn for_turn(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let session_id = session_id.into();
        let turn_id = turn_id.into();
        if session_id.is_empty() || turn_id.is_empty() {
            return Err(ProviderError::InvalidRequest {
                message: "provider turn context requires non-empty session and turn identities"
                    .to_owned(),
            });
        }
        Ok(Self::new(Some(session_id), turn_id))
    }

    /// Creates one context from real Norn session and prompt-event identities.
    pub(crate) fn new(session_id: Option<String>, turn_id: String) -> Self {
        Self {
            inner: Arc::new(ProviderTurnContextInner {
                session_id,
                turn_id,
                codex_turn_state: OnceLock::new(),
            }),
        }
    }

    /// Stable session/thread identity for Codex client metadata, when present.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.inner.session_id.as_deref()
    }

    /// Stable request identity for this user turn.
    #[must_use]
    pub fn turn_id(&self) -> &str {
        &self.inner.turn_id
    }

    /// Captures a server-issued state value without ever replacing the first.
    pub(crate) fn observe_codex_turn_state(&self, value: &str) -> TurnStateObservation {
        if HeaderValue::try_from(value).is_err() {
            tracing::debug!("ignored Codex turn-state value that cannot be replayed as a header");
            return TurnStateObservation::Rejected;
        }
        if let Some(current) = self.inner.codex_turn_state.get() {
            return observation_for_existing(current, value);
        }

        match self
            .inner
            .codex_turn_state
            .set(SecretString::new(value.to_owned()))
        {
            Ok(()) => TurnStateObservation::Captured,
            Err(candidate) => self
                .inner
                .codex_turn_state
                .get()
                .map_or(TurnStateObservation::Conflict, |current| {
                    observation_for_existing(current, candidate.expose())
                }),
        }
    }

    /// Header-safe projection of the first state value, if one was captured.
    pub(crate) fn codex_turn_state_header(&self) -> Option<HeaderValue> {
        self.inner
            .codex_turn_state
            .get()
            .and_then(|state| HeaderValue::try_from(state.expose()).ok())
            .map(|mut value| {
                value.set_sensitive(true);
                value
            })
    }
}

/// Result of observing a turn-state candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnStateObservation {
    /// The candidate could not be represented as the replay header.
    Rejected,
    /// This was the first accepted value.
    Captured,
    /// The candidate exactly repeated the authoritative value.
    Duplicate,
    /// A different candidate was ignored; the first value remains authoritative.
    Conflict,
}

fn observation_for_existing(current: &SecretString, candidate: &str) -> TurnStateObservation {
    if current.expose() == candidate {
        TurnStateObservation::Duplicate
    } else {
        tracing::debug!(
            "ignored conflicting Codex turn-state value; first value remains authoritative"
        );
        TurnStateObservation::Conflict
    }
}

/// Finds a turn-state value in a Codex `response.metadata` headers object.
pub(crate) fn codex_turn_state_from_metadata(value: &Value) -> Option<&str> {
    let headers = value.get("headers")?.as_object()?;
    headers.iter().find_map(|(name, value)| {
        name.eq_ignore_ascii_case(CODEX_TURN_STATE_HEADER)
            .then(|| json_string(value))
            .flatten()
    })
}

fn json_string(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Array(values) => values.first().and_then(Value::as_str),
        _ => None,
    }
}

/// Redacts reusable turn state from an event clone before disclosure.
///
/// This deliberately trusts only the sensitive header name, not the outer
/// SSE discriminator, because dumping occurs before response validation.
pub(crate) fn redact_codex_turn_state(value: &Value) -> Value {
    let mut redacted = value.clone();
    redact_codex_turn_state_in_place(&mut redacted);
    redacted
}

fn redact_codex_turn_state_in_place(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, field) in fields {
                if name.eq_ignore_ascii_case(CODEX_TURN_STATE_HEADER) {
                    *field = Value::String("[REDACTED]".to_owned());
                } else {
                    redact_codex_turn_state_in_place(field);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_codex_turn_state_in_place(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn first_turn_state_wins_without_debug_disclosure() {
        let context = ProviderTurnContext::new(Some("session-a".to_owned()), "turn-a".to_owned());
        assert_eq!(
            context.observe_codex_turn_state("invalid\nstate"),
            TurnStateObservation::Rejected
        );
        assert!(context.codex_turn_state_header().is_none());
        assert_eq!(
            context.observe_codex_turn_state("first-secret"),
            TurnStateObservation::Captured
        );
        assert_eq!(
            context.observe_codex_turn_state("first-secret"),
            TurnStateObservation::Duplicate
        );
        assert_eq!(
            context.observe_codex_turn_state("second-secret"),
            TurnStateObservation::Conflict
        );
        let header = context.codex_turn_state_header();
        assert_eq!(
            header.as_ref().and_then(|value| value.to_str().ok()),
            Some("first-secret")
        );
        assert!(header.as_ref().is_some_and(HeaderValue::is_sensitive));
        assert!(!format!("{header:?}").contains("first-secret"));
        let rendered = format!("{context:?}");
        assert!(!rendered.contains("first-secret"));
        assert!(!rendered.contains("second-secret"));
        assert!(rendered.contains("codex_turn_state_present: true"));
    }

    #[test]
    fn metadata_accepts_case_and_first_array_string_then_redacts() {
        let raw = json!({
            "type": "response.metadata",
            "headers": {"X-CoDeX-TuRn-StAtE": ["secret", "ignored"]},
        });
        assert_eq!(codex_turn_state_from_metadata(&raw), Some("secret"));
        let redacted = redact_codex_turn_state(&raw);
        assert_eq!(redacted["headers"]["X-CoDeX-TuRn-StAtE"], "[REDACTED]");
        assert!(!redacted.to_string().contains("secret"));

        let noncanonical = json!({
            "headers": [
                {"x-codex-turn-state": "array-secret"},
                {"nested": {"X-CoDeX-TuRn-StAtE": ["nested-secret"]}}
            ],
            "outside_headers": {"x-codex-turn-state": "outside-secret"}
        });
        let redacted = redact_codex_turn_state(&noncanonical);
        assert!(!redacted.to_string().contains("secret"));
        assert_eq!(redacted["headers"][0]["x-codex-turn-state"], "[REDACTED]");
        assert_eq!(
            redacted["headers"][1]["nested"]["X-CoDeX-TuRn-StAtE"],
            "[REDACTED]"
        );
        assert_eq!(
            redacted["outside_headers"]["x-codex-turn-state"],
            "[REDACTED]"
        );
        assert_eq!(
            codex_turn_state_from_metadata(&serde_json::json!({
                "headers": {"x-codex-turn-state": [["nested-secret"]]}
            })),
            None
        );
    }
}
