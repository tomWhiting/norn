//! Terminal-outcome projection for spawned sub-agents.
//!
//! Maps a child's [`AgentStepResult`] onto the [`ChildOutcomeSummary`] the
//! spawn result channel carries — terminal status, display output, error
//! text, and the typed [`AgentStopReason`] when the child stopped early —
//! and applies the matching terminal transition on the [`AgentRegistry`].
//! Split from [`super::spawn`] so each file stays inside the per-file
//! 500-line production-code limit.

use parking_lot::RwLock;
use uuid::Uuid;

use super::reclaim::log_terminal_transition_violation;
use crate::agent::output::AgentStopReason;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::error::NornError;
use crate::r#loop::runner::AgentStepResult;
use crate::provider::usage::Usage;

/// Mark the child's terminal registry status without touching the
/// outcome. Split from [`extract_outcome_summary`] (NH-006 R5) so a
/// [`SubagentHook`](crate::integration::hooks::SubagentHook) returning
/// [`HookOutcome::Block`](crate::integration::hooks::HookOutcome::Block)
/// can suppress the registry transition while the outcome summary
/// still surfaces on the result channel.
///
/// The wrapper is the sole owner of a live child's terminal transition
/// (see [`super::reclaim`]), so a transition failure here is an
/// invariant violation: it is logged loudly via
/// [`log_terminal_transition_violation`] but never propagated — the
/// wrapper still owes result delivery.
pub(super) fn mark_terminal_in_registry(
    registry: &RwLock<AgentRegistry>,
    child_id: Uuid,
    terminal_status: AgentStatus,
) {
    let mut reg = registry.write();
    if terminal_status == AgentStatus::Completed {
        if let Err(e) = reg.mark_completing(child_id) {
            log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
        }
        if let Err(e) = reg.mark_completed(child_id) {
            log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
        }
    } else if let Err(e) = reg.mark_failed(child_id) {
        log_terminal_transition_violation(&reg, child_id, "spawn_agent", &e);
    }
}

/// Render an output value as display text for the result envelope.
fn value_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| serde_json::to_string_pretty(value).ok())
}

/// Pure projection of a child's loop outcome, consumed by the spawn
/// result-channel sender. Carries no registry side effects so callers can
/// decide whether to also call [`mark_terminal_in_registry`].
pub(super) struct ChildOutcomeSummary {
    /// Terminal registry status: `Completed` only for
    /// [`AgentStepResult::Completed`], otherwise `Failed`.
    pub(super) status: AgentStatus,
    /// Display text of the child's output on success.
    pub(super) output_text: Option<String>,
    /// Explanatory error (including any partial output) when the child did
    /// not complete.
    pub(super) error: Option<String>,
    /// Typed stop reason when the child's run stopped early; `None` on
    /// completion or hard error.
    pub(super) stop: Option<AgentStopReason>,
    /// Accumulated token usage across every provider call the child
    /// made — populated on every [`AgentStepResult`] arm. On the hard
    /// [`NornError`] arm this is [`Usage::default`] (all zeros): the
    /// runner's error path (`run_agent_step` returning `Err`) carries no
    /// usage, so any tokens consumed before a mid-run hard error are
    /// genuinely unavailable here — zeros mean "unknown", not "none
    /// consumed". See [`extract_outcome_summary`]'s `Err` arm.
    pub(super) usage: Usage,
}

/// Project an [`AgentStepResult`] outcome into a [`ChildOutcomeSummary`].
///
/// Only [`AgentStepResult::Completed`] maps to success. A child that bailed
/// out — schema budget exhausted, max iterations, timeout, cancellation,
/// truncation — surfaces as `Failed` with an explanatory error (including
/// any partial output) *and* the typed [`AgentStopReason`], so the parent
/// never mistakes an unfinished child for a finished one and embedders can
/// branch on the reason without parsing strings.
pub(super) fn extract_outcome_summary(
    outcome: Result<AgentStepResult, NornError>,
) -> ChildOutcomeSummary {
    let stopped = |error: String, stop: AgentStopReason, usage: Usage| ChildOutcomeSummary {
        status: AgentStatus::Failed,
        output_text: None,
        error: Some(error),
        stop: Some(stop),
        usage,
    };
    match outcome {
        Ok(AgentStepResult::Completed { output, usage }) => ChildOutcomeSummary {
            status: AgentStatus::Completed,
            output_text: value_to_text(&output),
            error: None,
            stop: None,
            usage,
        },
        Ok(AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
        }) => {
            let mut error = format!(
                "sub-agent could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            );
            if let Some(partial) = best_attempt.as_ref().and_then(value_to_text) {
                error.push_str("\n\nBest attempt before giving up:\n");
                error.push_str(&partial);
            }
            stopped(
                error,
                AgentStopReason::SchemaUnreachable {
                    validation_errors,
                    attempts,
                },
                usage,
            )
        }
        Ok(AgentStepResult::MaxIterationsReached { usage }) => stopped(
            "sub-agent reached its max-iterations cap before completing its task".to_owned(),
            AgentStopReason::MaxIterationsReached,
            usage,
        ),
        Ok(AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
        }) => {
            let mut error = format!(
                "sub-agent timed out after {:.1}s ({iterations} iterations completed)",
                elapsed.as_secs_f64(),
            );
            if let Some(partial) = partial_output.as_ref().and_then(value_to_text) {
                error.push_str("\n\nPartial output before the timeout:\n");
                error.push_str(&partial);
            }
            stopped(
                error,
                AgentStopReason::TimedOut {
                    elapsed,
                    iterations,
                },
                usage,
            )
        }
        Ok(AgentStepResult::Cancelled { usage }) => stopped(
            "sub-agent was cancelled before completing its task".to_owned(),
            AgentStopReason::Cancelled,
            usage,
        ),
        Ok(AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
        }) => {
            let mut error = format!(
                "sub-agent output was truncated ({}) before it completed its task",
                kind.as_str(),
            );
            if let Some(partial) = partial_text.as_deref() {
                error.push_str("\n\nPartial output before the cut:\n");
                error.push_str(partial);
            }
            stopped(
                error,
                AgentStopReason::Truncated { kind, iterations },
                usage,
            )
        }
        // Hard error: `run_agent_step`'s `Err` path returns a bare
        // `NornError` with no usage attached, so usage accumulated across
        // provider calls made *before* the error is unrecoverable here.
        // `Usage::default()` (all zeros) therefore means "unknown", not
        // "no tokens consumed" — the lifecycle `Completed` event and the
        // result channel inherit this limitation (documented on
        // `ChildOutcomeSummary::usage` and `SubagentCompletion::usage`).
        Err(err) => ChildOutcomeSummary {
            status: AgentStatus::Failed,
            output_text: None,
            error: Some(err.to_string()),
            stop: None,
            usage: Usage::default(),
        },
    }
}

/// Project a child task that never produced an outcome — its inner
/// `tokio` task panicked or was aborted, surfacing as a
/// [`tokio::task::JoinError`] — onto the failure summary the wrapper
/// delivers.
///
/// Workspace code denies panics, but a tool or provider dependency inside
/// the child's task can still unwind; this keeps that defense loud: the
/// wrapper emits the lifecycle `Completed` event, the result-channel
/// failure, and the registry transition from this summary instead of
/// leaving observers a dangling `Started`. Usage is [`Usage::default`]
/// (unknown — the panicked task took its accumulated usage with it; see
/// [`ChildOutcomeSummary::usage`]).
pub(super) fn panicked_outcome_summary(join_error: &tokio::task::JoinError) -> ChildOutcomeSummary {
    ChildOutcomeSummary {
        status: AgentStatus::Failed,
        output_text: None,
        error: Some(format!(
            "sub-agent task terminated without an outcome (panicked or aborted): {join_error}"
        )),
        stop: None,
        usage: Usage::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::r#loop::config::TruncationKind;
    use crate::provider::usage::Usage;

    fn usage_fixture() -> Usage {
        Usage {
            input_tokens: 11,
            output_tokens: 4,
            ..Usage::default()
        }
    }

    /// Fix 6: only `Completed` maps to success — and carries no stop
    /// reason. The child's accumulated usage surfaces on the summary.
    #[test]
    fn outcome_summary_completed_is_success() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::Completed {
            output: serde_json::json!("all done"),
            usage: usage_fixture(),
        }));
        assert_eq!(summary.status, AgentStatus::Completed);
        assert_eq!(summary.output_text.as_deref(), Some("all done"));
        assert!(summary.error.is_none());
        assert!(summary.stop.is_none());
        assert_eq!(summary.usage.input_tokens, 11);
        assert_eq!(summary.usage.output_tokens, 4);
    }

    /// Fix 6: `MaxIterationsReached` surfaces as a failure the parent sees,
    /// with the typed stop reason and the accumulated usage on the summary.
    #[test]
    fn outcome_summary_max_iterations_is_failure() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::MaxIterationsReached {
            usage: usage_fixture(),
        }));
        assert_eq!(summary.status, AgentStatus::Failed);
        let error = summary.error.unwrap_or_default();
        assert!(error.contains("max-iterations"), "{error}");
        assert_eq!(summary.stop, Some(AgentStopReason::MaxIterationsReached));
        assert_eq!(summary.usage.input_tokens, 11, "usage must be preserved");
    }

    /// Fix 6: `SchemaUnreachable` surfaces as a failure carrying the
    /// validation errors, the best attempt, and the typed stop reason.
    #[test]
    fn outcome_summary_schema_unreachable_is_failure() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::SchemaUnreachable {
            best_attempt: Some(serde_json::json!("nearly valid")),
            validation_errors: vec!["missing field `x`".to_owned()],
            attempts: 5,
            usage: Usage::default(),
        }));
        assert_eq!(summary.status, AgentStatus::Failed);
        let error = summary.error.unwrap_or_default();
        assert!(error.contains("5 attempts"), "{error}");
        assert!(error.contains("missing field `x`"), "{error}");
        assert!(
            error.contains("nearly valid"),
            "best attempt included: {error}"
        );
        assert_eq!(
            summary.stop,
            Some(AgentStopReason::SchemaUnreachable {
                validation_errors: vec!["missing field `x`".to_owned()],
                attempts: 5,
            })
        );
    }

    /// Fix 6: `TimedOut` surfaces as a failure carrying the partial output
    /// and the typed stop reason.
    #[test]
    fn outcome_summary_timed_out_is_failure() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::TimedOut {
            elapsed: Duration::from_secs(12),
            iterations: 3,
            partial_output: Some(serde_json::json!("half a report")),
            usage: usage_fixture(),
        }));
        assert_eq!(summary.status, AgentStatus::Failed);
        let error = summary.error.unwrap_or_default();
        assert!(error.contains("timed out"), "{error}");
        assert!(error.contains("half a report"), "partial included: {error}");
        assert_eq!(
            summary.stop,
            Some(AgentStopReason::TimedOut {
                elapsed: Duration::from_secs(12),
                iterations: 3,
            })
        );
        assert_eq!(summary.usage.output_tokens, 4, "usage must be preserved");
    }

    /// Fix 6: `Cancelled` surfaces as a failure with the typed stop reason.
    #[test]
    fn outcome_summary_cancelled_is_failure() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::Cancelled {
            usage: Usage::default(),
        }));
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.error.unwrap_or_default().contains("cancelled"));
        assert_eq!(summary.stop, Some(AgentStopReason::Cancelled));
    }

    /// A truncated child (max-tokens / content-filter stop) surfaces as a
    /// failure carrying the partial text and the typed stop reason — never
    /// as a completed child.
    #[test]
    fn outcome_summary_truncated_is_failure() {
        let summary = extract_outcome_summary(Ok(AgentStepResult::Truncated {
            kind: TruncationKind::MaxTokens,
            partial_text: Some("partial answ".to_owned()),
            iterations: 2,
            usage: Usage::default(),
        }));
        assert_eq!(summary.status, AgentStatus::Failed);
        let error = summary.error.unwrap_or_default();
        assert!(error.contains("truncated"), "{error}");
        assert!(error.contains("max_tokens"), "{error}");
        assert!(error.contains("partial answ"), "partial included: {error}");
        assert_eq!(
            summary.stop,
            Some(AgentStopReason::Truncated {
                kind: TruncationKind::MaxTokens,
                iterations: 2,
            })
        );
    }

    /// A panicked/aborted child task projects onto an honest failure
    /// summary: Failed status, an error naming the panic, no stop reason,
    /// and unknown (zero) usage.
    #[tokio::test]
    #[allow(clippy::panic, clippy::expect_used)]
    async fn panicked_outcome_summary_reports_honest_failure() {
        let join_error = tokio::spawn(async { panic!("dependency exploded") })
            .await
            .expect_err("task must panic");
        let summary = panicked_outcome_summary(&join_error);
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.output_text.is_none());
        let error = summary.error.unwrap_or_default();
        assert!(
            error.contains("terminated without an outcome"),
            "error must say the task never produced an outcome: {error}"
        );
        assert!(
            error.contains("panic"),
            "error must surface the JoinError detail: {error}"
        );
        assert!(summary.stop.is_none());
        assert_eq!(
            summary.usage.input_tokens, 0,
            "usage is unknown after a panic — zeros, never invented numbers"
        );
    }

    /// A loop error maps to failure with the error text preserved and no
    /// stop reason (the run errored; it did not stop early).
    #[test]
    fn outcome_summary_loop_error_is_failure() {
        let summary = extract_outcome_summary(Err(NornError::Session(
            crate::error::SessionError::StorageError {
                reason: "disk gone".to_owned(),
            },
        )));
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.output_text.is_none());
        assert!(summary.error.unwrap_or_default().contains("disk gone"));
        assert!(summary.stop.is_none());
        assert_eq!(
            summary.usage.input_tokens, 0,
            "hard errors carry no usage on the runner's Err path — zeros mean unknown"
        );
    }
}
