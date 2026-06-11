//! Builder-facing result types: [`AgentOutput`] and [`AgentStopReason`].
//!
//! [`Agent::run`](super::instance::Agent::run) maps the runner's
//! [`AgentStepResult`](crate::r#loop::config::AgentStepResult) into an
//! [`AgentOutput`] that bundles the final output value, accumulated token
//! usage, the (optional) event store for session resume, and a public
//! [`AgentStopReason`] describing why the step ended.
//!
//! The runner's `AgentStepResult` carries the per-variant payload the loop
//! needs internally; `AgentOutput` flattens the owned `output`, `usage`, and
//! `event_store` onto the struct so consumers branch on a small
//! [`AgentStopReason`] discriminant rather than destructuring a large enum.

use std::time::Duration;

use serde_json::Value;

use crate::r#loop::config::AgentStepResult;
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Why an agent step stopped.
///
/// Public mirror of the [`AgentStepResult`](crate::r#loop::config::AgentStepResult)
/// outcome discriminant, trimmed to the fields a consumer needs to branch on.
/// The owned output value, token usage, and event store live on
/// [`AgentOutput`] rather than being duplicated per variant.
#[derive(Debug)]
pub enum AgentStopReason {
    /// The model produced valid structured output (or text in no-schema mode).
    Completed,

    /// The schema-enforcement budget was exhausted without valid output. The
    /// best attempt the model produced (if any) is on
    /// [`AgentOutput::output`]; the validation errors and attempt count that
    /// explain the failure are here.
    SchemaUnreachable {
        /// Validation errors from the final attempt.
        validation_errors: Vec<String>,
        /// Total schema-budget-consuming attempts made.
        attempts: u32,
    },

    /// The optional max-iterations cap was reached before the model produced
    /// final output.
    MaxIterationsReached,

    /// The configured step timeout elapsed before the loop completed. Any
    /// partial assistant output is on [`AgentOutput::output`].
    TimedOut {
        /// Wall-clock time the loop ran before being cancelled.
        elapsed: Duration,
        /// Completed provider iterations at the moment of the timeout.
        iterations: usize,
    },

    /// Cooperative cancellation fired via the builder's cancellation token.
    Cancelled,
}

/// The outcome of running an agent step via
/// [`Agent::run`](super::instance::Agent::run).
///
/// Not [`Clone`]: it owns the session [`EventStore`], which is single-owner
/// runtime state.
#[derive(Debug)]
pub struct AgentOutput {
    /// Final output value.
    ///
    /// For [`AgentStopReason::Completed`] this is the schema-validated (or
    /// plain-text) output. For [`AgentStopReason::SchemaUnreachable`] and
    /// [`AgentStopReason::TimedOut`] it is the best / partial attempt the
    /// model produced, or [`Value::Null`] when none was produced. For the
    /// remaining stop reasons it is [`Value::Null`].
    pub output: Value,

    /// Accumulated token usage across every provider call in the step.
    ///
    /// Always [`Usage::default`] (all-zero) for
    /// [`AgentStopReason::TimedOut`]: the timeout fires from the outer
    /// `tokio::time::timeout` wrapper, which does not thread per-call usage
    /// back out.
    pub usage: Usage,

    /// The session event store, returned so callers can resume the session by
    /// passing it back to
    /// [`AgentBuilder::session`](super::builder::AgentBuilder::session).
    /// `None` when the builder was asked not to surface it.
    pub event_store: Option<EventStore>,

    /// Why the step stopped.
    pub stop_reason: AgentStopReason,
}

impl AgentOutput {
    /// Build an [`AgentOutput`] from the runner's [`AgentStepResult`] and the
    /// session's event store.
    ///
    /// This is the single conversion point from the loop-internal result enum
    /// to the public builder surface. The `event_store` is moved onto the
    /// output so callers can resume the session; pass `None` when the store
    /// should not be surfaced.
    #[must_use]
    pub fn from_step_result(result: AgentStepResult, event_store: Option<EventStore>) -> Self {
        match result {
            AgentStepResult::Completed { output, usage } => Self {
                output,
                usage,
                event_store,
                stop_reason: AgentStopReason::Completed,
            },
            AgentStepResult::SchemaUnreachable {
                best_attempt,
                validation_errors,
                attempts,
                usage,
            } => Self {
                output: best_attempt.unwrap_or(Value::Null),
                usage,
                event_store,
                stop_reason: AgentStopReason::SchemaUnreachable {
                    validation_errors,
                    attempts,
                },
            },
            AgentStepResult::MaxIterationsReached { usage } => Self {
                output: Value::Null,
                usage,
                event_store,
                stop_reason: AgentStopReason::MaxIterationsReached,
            },
            AgentStepResult::TimedOut {
                elapsed,
                iterations,
                partial_output,
            } => Self {
                output: partial_output.unwrap_or(Value::Null),
                usage: Usage::default(),
                event_store,
                stop_reason: AgentStopReason::TimedOut {
                    elapsed,
                    iterations,
                },
            },
            AgentStepResult::Cancelled { usage } => Self {
                output: Value::Null,
                usage,
                event_store,
                stop_reason: AgentStopReason::Cancelled,
            },
        }
    }

    /// The model's final text response, read from the event store.
    ///
    /// Scans the event store newest-first for the most recent
    /// [`SessionEvent::AssistantMessage`] with non-empty text content and
    /// returns it. Returns `None` when no event store was retained or no
    /// assistant text was produced (e.g. the model only emitted tool calls).
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let store = self.event_store.as_ref()?;
        store
            .events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                SessionEvent::AssistantMessage { content, .. } if !content.is_empty() => {
                    Some(content)
                }
                _ => None,
            })
    }

    /// The schema-enforced output value, available only when the step
    /// [`Completed`](AgentStopReason::Completed).
    ///
    /// Returns `None` for every non-success stop reason so callers do not
    /// mistake a best-effort or partial attempt for validated output.
    #[must_use]
    pub fn structured_output(&self) -> Option<&Value> {
        if matches!(self.stop_reason, AgentStopReason::Completed) {
            Some(&self.output)
        } else {
            None
        }
    }

    /// Whether the step completed successfully.
    ///
    /// `true` only for [`AgentStopReason::Completed`]; every other stop
    /// reason (schema unreachable, max iterations, timeout, cancellation) is
    /// a non-success outcome.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self.stop_reason, AgentStopReason::Completed)
    }

    /// Accumulated token usage for the step.
    #[must_use]
    pub fn usage(&self) -> &Usage {
        &self.usage
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::events::{EventBase, EventUsage};

    fn assistant_message(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_string(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        }
    }

    fn store_with(messages: &[&str]) -> EventStore {
        let store = EventStore::new();
        for m in messages {
            store
                .append(assistant_message(m))
                .expect("append succeeds on in-memory store");
        }
        store
    }

    fn sample_usage() -> Usage {
        Usage {
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: Some(0.01),
        }
    }

    #[test]
    fn output_holds_all_fields() {
        let out = AgentOutput {
            output: serde_json::json!({"k": "v"}),
            usage: sample_usage(),
            event_store: Some(EventStore::new()),
            stop_reason: AgentStopReason::Completed,
        };
        assert_eq!(out.output, serde_json::json!({"k": "v"}));
        assert_eq!(out.usage.input_tokens, 100);
        assert!(out.event_store.is_some());
        assert!(matches!(out.stop_reason, AgentStopReason::Completed));
    }

    #[test]
    fn completed_maps_output_and_usage_and_is_success() {
        let result = AgentStepResult::Completed {
            output: serde_json::json!({"answer": 42}),
            usage: sample_usage(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert!(matches!(out.stop_reason, AgentStopReason::Completed));
        assert_eq!(out.output, serde_json::json!({"answer": 42}));
        assert_eq!(out.usage().output_tokens, 40);
        assert!(out.is_success());
        assert_eq!(
            out.structured_output(),
            Some(&serde_json::json!({"answer": 42}))
        );
    }

    #[test]
    fn schema_unreachable_maps_best_attempt_and_errors() {
        let result = AgentStepResult::SchemaUnreachable {
            best_attempt: Some(serde_json::json!({"partial": true})),
            validation_errors: vec!["missing field x".to_string()],
            attempts: 3,
            usage: sample_usage(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert_eq!(out.output, serde_json::json!({"partial": true}));
        assert!(!out.is_success());
        assert!(out.structured_output().is_none());
        match out.stop_reason {
            AgentStopReason::SchemaUnreachable {
                validation_errors,
                attempts,
            } => {
                assert_eq!(validation_errors, vec!["missing field x".to_string()]);
                assert_eq!(attempts, 3);
            }
            other => panic!("expected SchemaUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn schema_unreachable_without_best_attempt_is_null() {
        let result = AgentStepResult::SchemaUnreachable {
            best_attempt: None,
            validation_errors: Vec::new(),
            attempts: 1,
            usage: Usage::default(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert_eq!(out.output, Value::Null);
    }

    #[test]
    fn max_iterations_maps_to_null_output() {
        let result = AgentStepResult::MaxIterationsReached {
            usage: sample_usage(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert_eq!(out.output, Value::Null);
        assert_eq!(out.usage.input_tokens, 100);
        assert!(matches!(
            out.stop_reason,
            AgentStopReason::MaxIterationsReached
        ));
        assert!(!out.is_success());
    }

    #[test]
    fn timed_out_maps_partial_output_and_zero_usage() {
        let result = AgentStepResult::TimedOut {
            elapsed: Duration::from_secs(5),
            iterations: 2,
            partial_output: Some(serde_json::json!("partial text")),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert_eq!(out.output, serde_json::json!("partial text"));
        // TimedOut carries no usage on the runner side.
        assert_eq!(out.usage.input_tokens, 0);
        match out.stop_reason {
            AgentStopReason::TimedOut {
                elapsed,
                iterations,
            } => {
                assert_eq!(elapsed, Duration::from_secs(5));
                assert_eq!(iterations, 2);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert!(!out.is_success());
    }

    #[test]
    fn cancelled_maps_to_null_output_with_usage() {
        let result = AgentStepResult::Cancelled {
            usage: sample_usage(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert_eq!(out.output, Value::Null);
        assert_eq!(out.usage.input_tokens, 100);
        assert!(matches!(out.stop_reason, AgentStopReason::Cancelled));
        assert!(!out.is_success());
    }

    #[test]
    fn text_returns_last_nonempty_assistant_message() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let store = store_with(&["first reply", "second reply"]);
        let out = AgentOutput::from_step_result(result, Some(store));
        assert_eq!(out.text().as_deref(), Some("second reply"));
    }

    #[test]
    fn text_skips_empty_trailing_assistant_messages() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        // A trailing tool-call-only turn produces an empty-content
        // AssistantMessage; .text() must skip it and surface the prior text.
        let store = store_with(&["the real answer", ""]);
        let out = AgentOutput::from_step_result(result, Some(store));
        assert_eq!(out.text().as_deref(), Some("the real answer"));
    }

    #[test]
    fn text_is_none_without_event_store() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let out = AgentOutput::from_step_result(result, None);
        assert!(out.text().is_none());
    }

    #[test]
    fn text_is_none_when_no_assistant_text() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let store = store_with(&[]);
        let out = AgentOutput::from_step_result(result, Some(store));
        assert!(out.text().is_none());
    }
}
