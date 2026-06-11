//! Terminal-outcome projection for spawned sub-agents.
//!
//! Maps a child's [`AgentStepResult`] onto the `(status, output, error)`
//! triple the spawn result channel carries, and applies the matching
//! terminal transition on the [`AgentRegistry`]. Split from
//! [`super::spawn`] so each file stays inside the per-file 500-line
//! production-code limit.

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::error::NornError;
use crate::r#loop::runner::AgentStepResult;

/// Mark the child's terminal registry status without touching the
/// outcome. Split from [`extract_outcome_summary`] (NH-006 R5) so a
/// [`SubagentHook`](crate::integration::hooks::SubagentHook) returning
/// [`HookOutcome::Block`](crate::integration::hooks::HookOutcome::Block)
/// can suppress the registry transition while the outcome summary
/// still surfaces on the result channel.
pub(super) fn mark_terminal_in_registry(
    registry: &RwLock<AgentRegistry>,
    child_id: Uuid,
    terminal_status: AgentStatus,
) {
    if terminal_status == AgentStatus::Completed {
        let mut reg = registry.write();
        if let Err(e) = reg.mark_completing(child_id) {
            tracing::warn!(
                child_id = %child_id,
                error = %e,
                "spawn_agent: mark_completing failed",
            );
        }
        if let Err(e) = reg.mark_completed(child_id) {
            tracing::warn!(
                child_id = %child_id,
                error = %e,
                "spawn_agent: mark_completed failed",
            );
        }
    } else if let Err(mark_err) = registry.write().mark_failed(child_id) {
        tracing::warn!(
            child_id = %child_id,
            error = %mark_err,
            "spawn_agent: mark_failed failed after sub-agent failure",
        );
    }
}

/// Render an output value as display text for the result envelope.
fn value_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| serde_json::to_string_pretty(value).ok())
}

/// Pure projection of an [`AgentStepResult`] outcome into the
/// (`status`, `output_text`, `error`) triple consumed by the spawn
/// result-channel sender. Carries no registry side effects so callers
/// can decide whether to also call [`mark_terminal_in_registry`].
///
/// Only [`AgentStepResult::Completed`] maps to success. A child that bailed
/// out — schema budget exhausted, max iterations, timeout, cancellation —
/// surfaces as `Failed` with an explanatory error (including any partial
/// output) so the parent never mistakes an unfinished child for a finished
/// one.
pub(super) fn extract_outcome_summary(
    outcome: Result<AgentStepResult, NornError>,
) -> (AgentStatus, Option<String>, Option<String>) {
    match outcome {
        Ok(AgentStepResult::Completed { output, .. }) => {
            (AgentStatus::Completed, value_to_text(&output), None)
        }
        Ok(AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            ..
        }) => {
            let mut error = format!(
                "sub-agent could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            );
            if let Some(partial) = best_attempt.as_ref().and_then(value_to_text) {
                error.push_str("\n\nBest attempt before giving up:\n");
                error.push_str(&partial);
            }
            (AgentStatus::Failed, None, Some(error))
        }
        Ok(AgentStepResult::MaxIterationsReached { .. }) => (
            AgentStatus::Failed,
            None,
            Some("sub-agent reached its max-iterations cap before completing its task".to_owned()),
        ),
        Ok(AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
        }) => {
            let mut error = format!(
                "sub-agent timed out after {:.1}s ({iterations} iterations completed)",
                elapsed.as_secs_f64(),
            );
            if let Some(partial) = partial_output.as_ref().and_then(value_to_text) {
                error.push_str("\n\nPartial output before the timeout:\n");
                error.push_str(&partial);
            }
            (AgentStatus::Failed, None, Some(error))
        }
        Ok(AgentStepResult::Cancelled { .. }) => (
            AgentStatus::Failed,
            None,
            Some("sub-agent was cancelled before completing its task".to_owned()),
        ),
        Err(err) => (AgentStatus::Failed, None, Some(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::provider::usage::Usage;

    /// Fix 6: only `Completed` maps to success.
    #[test]
    fn outcome_summary_completed_is_success() {
        let (status, output, error) = extract_outcome_summary(Ok(AgentStepResult::Completed {
            output: serde_json::json!("all done"),
            usage: Usage::default(),
        }));
        assert_eq!(status, AgentStatus::Completed);
        assert_eq!(output.as_deref(), Some("all done"));
        assert!(error.is_none());
    }

    /// Fix 6: `MaxIterationsReached` surfaces as a failure the parent sees.
    #[test]
    fn outcome_summary_max_iterations_is_failure() {
        let (status, _output, error) =
            extract_outcome_summary(Ok(AgentStepResult::MaxIterationsReached {
                usage: Usage::default(),
            }));
        assert_eq!(status, AgentStatus::Failed);
        let error = error.unwrap_or_default();
        assert!(error.contains("max-iterations"), "{error}");
    }

    /// Fix 6: `SchemaUnreachable` surfaces as a failure carrying the
    /// validation errors and the best attempt.
    #[test]
    fn outcome_summary_schema_unreachable_is_failure() {
        let (status, _output, error) =
            extract_outcome_summary(Ok(AgentStepResult::SchemaUnreachable {
                best_attempt: Some(serde_json::json!("nearly valid")),
                validation_errors: vec!["missing field `x`".to_owned()],
                attempts: 5,
                usage: Usage::default(),
            }));
        assert_eq!(status, AgentStatus::Failed);
        let error = error.unwrap_or_default();
        assert!(error.contains("5 attempts"), "{error}");
        assert!(error.contains("missing field `x`"), "{error}");
        assert!(
            error.contains("nearly valid"),
            "best attempt included: {error}"
        );
    }

    /// Fix 6: `TimedOut` surfaces as a failure carrying the partial output.
    #[test]
    fn outcome_summary_timed_out_is_failure() {
        let (status, _output, error) = extract_outcome_summary(Ok(AgentStepResult::TimedOut {
            elapsed: Duration::from_secs(12),
            iterations: 3,
            partial_output: Some(serde_json::json!("half a report")),
        }));
        assert_eq!(status, AgentStatus::Failed);
        let error = error.unwrap_or_default();
        assert!(error.contains("timed out"), "{error}");
        assert!(error.contains("half a report"), "partial included: {error}");
    }

    /// Fix 6: `Cancelled` surfaces as a failure.
    #[test]
    fn outcome_summary_cancelled_is_failure() {
        let (status, _output, error) = extract_outcome_summary(Ok(AgentStepResult::Cancelled {
            usage: Usage::default(),
        }));
        assert_eq!(status, AgentStatus::Failed);
        assert!(error.unwrap_or_default().contains("cancelled"));
    }

    /// A loop error maps to failure with the error text preserved.
    #[test]
    fn outcome_summary_loop_error_is_failure() {
        let (status, output, error) = extract_outcome_summary(Err(NornError::Session(
            crate::error::SessionError::StorageError {
                reason: "disk gone".to_owned(),
            },
        )));
        assert_eq!(status, AgentStatus::Failed);
        assert!(output.is_none());
        assert!(error.unwrap_or_default().contains("disk gone"));
    }
}
