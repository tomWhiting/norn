//! Builder-facing result types: [`RunOutcome`], [`AgentOutput`], and
//! [`AgentStopReason`].
//!
//! [`Agent::run`](super::instance::Agent::run) maps the runner's
//! [`AgentStepResult`](crate::r#loop::config::AgentStepResult) into a
//! [`RunOutcome`]: [`RunOutcome::Completed`] for a finished run, or
//! [`RunOutcome::Stopped`] when the run ended early (schema budget
//! exhausted, max iterations, timeout, cancellation, truncation). The split
//! is structural — a consumer cannot read a stopped run's partial output
//! without going through the [`RunOutcome::Stopped`] arm, so non-completion
//! can never silently masquerade as success.
//!
//! [`AgentOutput`] is the payload carried by *both* arms: the output value,
//! accumulated token usage, and the (optional) session event store for
//! resume. [`AgentStopReason`] describes *why* a stopped run stopped, with
//! the per-reason detail (validation errors, elapsed time, truncation kind).

use std::time::Duration;

use serde_json::Value;

use crate::r#loop::config::{AgentStepResult, TruncationKind};
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Why a run stopped before completing.
///
/// Carried on [`RunOutcome::Stopped`]. There is deliberately no
/// `Completed` variant: completion is [`RunOutcome::Completed`], so a stop
/// reason always means non-completion. Serializable so embedders can
/// persist the reason across process/activity boundaries.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AgentStopReason {
    /// The schema-enforcement budget was exhausted without valid output.
    /// The best attempt the model produced (if any) is on the partial
    /// [`AgentOutput::output`]; the validation errors and attempt count
    /// that explain the failure are here.
    SchemaUnreachable {
        /// Validation errors from the final attempt.
        validation_errors: Vec<String>,
        /// Total schema-budget-consuming attempts made.
        attempts: u32,
    },

    /// The optional max-iterations cap was reached before the model
    /// produced final output.
    MaxIterationsReached,

    /// The configured step timeout elapsed before the loop completed. Any
    /// partial assistant output is on the partial [`AgentOutput::output`].
    TimedOut {
        /// Wall-clock time the loop ran before being cancelled.
        elapsed: Duration,
        /// Completed provider iterations at the moment of the timeout.
        iterations: usize,
    },

    /// Cooperative cancellation fired via the builder's cancellation token.
    Cancelled,

    /// The model stopped deterministically before completing its output —
    /// it hit its maximum output-token limit or the provider's content
    /// filter cut the response off. The partial text (if any) is on the
    /// partial [`AgentOutput::output`]. Deterministic: re-running the
    /// identical request reproduces the same stop.
    Truncated {
        /// Which deterministic stop cut the response off.
        kind: TruncationKind,
        /// Completed provider iterations, including the truncated one.
        iterations: u32,
    },
}

/// The payload of a run: final (or partial) output value, accumulated
/// token usage, and the session event store.
///
/// Carried by both [`RunOutcome`] arms. Not [`Clone`]: it owns the session
/// [`EventStore`], which is single-owner runtime state.
#[derive(Debug)]
pub struct AgentOutput {
    /// Output value.
    ///
    /// On [`RunOutcome::Completed`] this is the schema-validated (or
    /// plain-text) output. On [`RunOutcome::Stopped`] it is whatever the
    /// run produced before stopping — the best schema attempt for
    /// [`AgentStopReason::SchemaUnreachable`], the partial text for
    /// [`AgentStopReason::TimedOut`] / [`AgentStopReason::Truncated`] —
    /// or [`Value::Null`] when nothing was produced.
    pub output: Value,

    /// Accumulated token usage across every provider call in the step,
    /// populated on completed *and* stopped runs (including timeouts,
    /// whose usage is tracked through the loop's shared timeout state).
    pub usage: Usage,

    /// The session event store, returned so callers can resume the session
    /// by passing it back to
    /// [`AgentBuilder::session`](super::builder::AgentBuilder::session).
    /// `None` when the builder was asked not to surface it.
    pub event_store: Option<EventStore>,
}

impl AgentOutput {
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

    /// Accumulated token usage for the step.
    #[must_use]
    pub fn usage(&self) -> &Usage {
        &self.usage
    }
}

/// The outcome of running an agent via
/// [`Agent::run`](super::instance::Agent::run): either the run completed,
/// or it stopped early with a typed reason and whatever partial output it
/// produced.
///
/// The enum forces consumers to confront non-completion: partial output is
/// only reachable through the [`Stopped`](Self::Stopped) arm, alongside the
/// [`AgentStopReason`] that explains it.
#[must_use = "a run can stop early without completing; match on the outcome \
              (or call is_completed) instead of discarding it"]
#[derive(Debug)]
pub enum RunOutcome {
    /// The run completed: the model produced valid structured output (or
    /// text in no-schema mode).
    Completed(AgentOutput),

    /// The run stopped before completing. `partial` carries whatever the
    /// run genuinely produced — partial output value, accumulated usage,
    /// and the session event store with every persisted event.
    Stopped {
        /// Why the run stopped.
        reason: AgentStopReason,
        /// Everything the run produced before stopping.
        partial: AgentOutput,
    },
}

impl RunOutcome {
    /// Build a [`RunOutcome`] from the runner's [`AgentStepResult`] and the
    /// session's event store.
    ///
    /// This is the single conversion point from the loop-internal result
    /// enum to the public run surface. The `event_store` is moved onto the
    /// payload so callers can resume the session; pass `None` when the
    /// store should not be surfaced.
    pub fn from_step_result(result: AgentStepResult, event_store: Option<EventStore>) -> Self {
        let stopped = |reason: AgentStopReason, output: Value, usage: Usage| Self::Stopped {
            reason,
            partial: AgentOutput {
                output,
                usage,
                event_store: None,
            },
        };
        let outcome = match result {
            AgentStepResult::Completed { output, usage } => Self::Completed(AgentOutput {
                output,
                usage,
                event_store: None,
            }),
            AgentStepResult::SchemaUnreachable {
                best_attempt,
                validation_errors,
                attempts,
                usage,
            } => stopped(
                AgentStopReason::SchemaUnreachable {
                    validation_errors,
                    attempts,
                },
                best_attempt.unwrap_or(Value::Null),
                usage,
            ),
            AgentStepResult::MaxIterationsReached { usage } => {
                stopped(AgentStopReason::MaxIterationsReached, Value::Null, usage)
            }
            AgentStepResult::TimedOut {
                elapsed,
                iterations,
                partial_output,
                usage,
            } => stopped(
                AgentStopReason::TimedOut {
                    elapsed,
                    iterations,
                },
                partial_output.unwrap_or(Value::Null),
                usage,
            ),
            AgentStepResult::Cancelled { usage } => {
                stopped(AgentStopReason::Cancelled, Value::Null, usage)
            }
            AgentStepResult::Truncated {
                kind,
                partial_text,
                iterations,
                usage,
            } => stopped(
                AgentStopReason::Truncated { kind, iterations },
                partial_text.map_or(Value::Null, Value::String),
                usage,
            ),
        };
        outcome.with_event_store(event_store)
    }

    /// Attach the event store to whichever arm carries the payload.
    fn with_event_store(mut self, event_store: Option<EventStore>) -> Self {
        match &mut self {
            Self::Completed(output)
            | Self::Stopped {
                partial: output, ..
            } => output.event_store = event_store,
        }
        self
    }

    /// Whether the run completed.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    /// The stop reason when the run stopped early, `None` when it
    /// completed.
    #[must_use]
    pub const fn stop_reason(&self) -> Option<&AgentStopReason> {
        match self {
            Self::Completed(_) => None,
            Self::Stopped { reason, .. } => Some(reason),
        }
    }

    /// The run's payload — final output for a completed run, partial
    /// output for a stopped one.
    #[must_use]
    pub const fn output(&self) -> &AgentOutput {
        match self {
            Self::Completed(output)
            | Self::Stopped {
                partial: output, ..
            } => output,
        }
    }

    /// Consume the outcome, returning the payload regardless of arm.
    ///
    /// Use this only after the completion question has been answered (or
    /// when both arms are genuinely handled the same way, e.g. handing the
    /// event store back for resume).
    #[must_use]
    pub fn into_output(self) -> AgentOutput {
        match self {
            Self::Completed(output)
            | Self::Stopped {
                partial: output, ..
            } => output,
        }
    }

    /// The schema-enforced output value, available only when the run
    /// [`Completed`](Self::Completed).
    ///
    /// Returns `None` for every stopped run so callers do not mistake a
    /// best-effort or partial attempt for validated output.
    #[must_use]
    pub const fn structured_output(&self) -> Option<&Value> {
        match self {
            Self::Completed(output) => Some(&output.output),
            Self::Stopped { .. } => None,
        }
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
    fn completed_maps_output_and_usage() {
        let result = AgentStepResult::Completed {
            output: serde_json::json!({"answer": 42}),
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, Some(EventStore::new()));
        assert!(outcome.is_completed());
        assert!(outcome.stop_reason().is_none());
        assert_eq!(
            outcome.structured_output(),
            Some(&serde_json::json!({"answer": 42}))
        );
        assert_eq!(outcome.output().usage().output_tokens, 40);
        let output = outcome.into_output();
        assert_eq!(output.output, serde_json::json!({"answer": 42}));
        assert!(output.event_store.is_some(), "store rides the payload");
    }

    #[test]
    fn schema_unreachable_stops_with_best_attempt_and_errors() {
        let result = AgentStepResult::SchemaUnreachable {
            best_attempt: Some(serde_json::json!({"partial": true})),
            validation_errors: vec!["missing field x".to_string()],
            attempts: 3,
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert!(!outcome.is_completed());
        assert!(outcome.structured_output().is_none());
        match outcome {
            RunOutcome::Stopped { reason, partial } => {
                assert_eq!(
                    reason,
                    AgentStopReason::SchemaUnreachable {
                        validation_errors: vec!["missing field x".to_string()],
                        attempts: 3,
                    }
                );
                assert_eq!(partial.output, serde_json::json!({"partial": true}));
                assert_eq!(partial.usage.input_tokens, 100);
            }
            RunOutcome::Completed(_) => panic!("expected Stopped"),
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
        let outcome = RunOutcome::from_step_result(result, None);
        assert_eq!(outcome.output().output, Value::Null);
    }

    #[test]
    fn max_iterations_stops_with_usage() {
        let result = AgentStepResult::MaxIterationsReached {
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert_eq!(
            outcome.stop_reason(),
            Some(&AgentStopReason::MaxIterationsReached)
        );
        assert_eq!(outcome.output().output, Value::Null);
        assert_eq!(outcome.output().usage.input_tokens, 100);
    }

    #[test]
    fn timed_out_stops_with_partial_output_and_usage() {
        let result = AgentStepResult::TimedOut {
            elapsed: Duration::from_secs(5),
            iterations: 2,
            partial_output: Some(serde_json::json!("partial text")),
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert_eq!(
            outcome.stop_reason(),
            Some(&AgentStopReason::TimedOut {
                elapsed: Duration::from_secs(5),
                iterations: 2,
            })
        );
        assert_eq!(outcome.output().output, serde_json::json!("partial text"));
        // Usage now rides the timeout arm (tracked via shared timeout
        // state) instead of being zeroed.
        assert_eq!(outcome.output().usage.input_tokens, 100);
    }

    #[test]
    fn cancelled_stops_with_null_output_and_usage() {
        let result = AgentStepResult::Cancelled {
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert_eq!(outcome.stop_reason(), Some(&AgentStopReason::Cancelled));
        assert_eq!(outcome.output().output, Value::Null);
        assert_eq!(outcome.output().usage.input_tokens, 100);
    }

    #[test]
    fn truncated_stops_with_partial_text_and_usage() {
        let result = AgentStepResult::Truncated {
            kind: TruncationKind::MaxTokens,
            partial_text: Some("partial answ".to_string()),
            iterations: 1,
            usage: sample_usage(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert!(!outcome.is_completed());
        assert!(outcome.structured_output().is_none());
        assert_eq!(
            outcome.stop_reason(),
            Some(&AgentStopReason::Truncated {
                kind: TruncationKind::MaxTokens,
                iterations: 1,
            })
        );
        assert_eq!(
            outcome.output().output,
            Value::String("partial answ".to_string())
        );
        assert_eq!(outcome.output().usage.input_tokens, 100);
    }

    #[test]
    fn truncated_without_text_is_null() {
        let result = AgentStepResult::Truncated {
            kind: TruncationKind::ContentFilter,
            partial_text: None,
            iterations: 1,
            usage: Usage::default(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert_eq!(outcome.output().output, Value::Null);
    }

    #[test]
    fn stopped_arm_carries_event_store() {
        let result = AgentStepResult::Cancelled {
            usage: Usage::default(),
        };
        let store = store_with(&["progress so far"]);
        let outcome = RunOutcome::from_step_result(result, Some(store));
        assert!(
            outcome.output().event_store.is_some(),
            "a stopped run's partial payload must carry the event store"
        );
        assert_eq!(outcome.output().text().as_deref(), Some("progress so far"));
    }

    #[test]
    fn stop_reason_serde_round_trips_every_shape() {
        let cases = vec![
            AgentStopReason::SchemaUnreachable {
                validation_errors: vec!["missing field x".to_string()],
                attempts: 3,
            },
            AgentStopReason::MaxIterationsReached,
            AgentStopReason::TimedOut {
                elapsed: Duration::from_millis(1500),
                iterations: 2,
            },
            AgentStopReason::Cancelled,
            AgentStopReason::Truncated {
                kind: TruncationKind::MaxTokens,
                iterations: 4,
            },
            AgentStopReason::Truncated {
                kind: TruncationKind::ContentFilter,
                iterations: 1,
            },
        ];
        for reason in cases {
            let json = serde_json::to_string(&reason).expect("serialize");
            let back: AgentStopReason = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, reason, "round trip failed for {json}");
        }
    }

    #[test]
    fn text_returns_last_nonempty_assistant_message() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let store = store_with(&["first reply", "second reply"]);
        let outcome = RunOutcome::from_step_result(result, Some(store));
        assert_eq!(outcome.output().text().as_deref(), Some("second reply"));
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
        let outcome = RunOutcome::from_step_result(result, Some(store));
        assert_eq!(outcome.output().text().as_deref(), Some("the real answer"));
    }

    #[test]
    fn text_is_none_without_event_store() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let outcome = RunOutcome::from_step_result(result, None);
        assert!(outcome.output().text().is_none());
    }

    #[test]
    fn text_is_none_when_no_assistant_text() {
        let result = AgentStepResult::Completed {
            output: Value::Null,
            usage: Usage::default(),
        };
        let store = store_with(&[]);
        let outcome = RunOutcome::from_step_result(result, Some(store));
        assert!(outcome.output().text().is_none());
    }
}
