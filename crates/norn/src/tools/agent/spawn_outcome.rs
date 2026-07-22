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
pub(crate) fn mark_terminal_in_registry(
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
pub(crate) struct ChildOutcomeSummary {
    /// Terminal registry status: `Completed` only for
    /// [`AgentStepResult::Completed`], otherwise `Failed`.
    pub(crate) status: AgentStatus,
    /// Display text of the child's output on success.
    pub(crate) output_text: Option<String>,
    /// Explanatory error (including any partial output) when the child did
    /// not complete.
    pub(crate) error: Option<String>,
    /// Typed stop reason when the child's run stopped early; `None` on
    /// completion or hard error.
    pub(crate) stop: Option<AgentStopReason>,
    /// Accumulated token usage across every provider call the child
    /// made — populated on every [`AgentStepResult`] arm. On the hard
    /// [`NornError`] arm this is [`Usage::default`] (all zeros): the
    /// runner's error path (`run_agent_step` returning `Err`) carries no
    /// usage, so any tokens consumed before a mid-run hard error are
    /// genuinely unavailable here — zeros mean "unknown", not "none
    /// consumed". See [`extract_outcome_summary`]'s `Err` arm.
    pub(crate) usage: Usage,
    /// Summed `subtree_usage` of every grandchild result the child's
    /// loop delivered (W3.6 usage rollup) — from the
    /// [`AgentStepResult`] arm on every loop outcome, and from the
    /// wrapper's shared
    /// [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
    /// handle on the hard-error and panic paths (where no arm exists
    /// but the delivered grandchild spend is still real). Disjoint from
    /// [`Self::usage`]: `usage + children_usage` is the child's subtree
    /// total with each agent counted exactly once.
    pub(crate) children_usage: Usage,
}

impl ChildOutcomeSummary {
    /// Surface a terminal mailbox persistence fault without exposing the
    /// accepted message payload. The run's output stays available as audit
    /// evidence, while status and error prevent it being reported as success.
    /// An existing failure keeps its diagnosis and typed stop reason. Usage is
    /// never rewritten because the provider work still happened.
    pub(super) fn downgrade_terminal_persistence(&mut self) {
        if self.status == AgentStatus::Completed {
            self.error = Some(super::TERMINAL_PERSISTENCE_FAILURE.to_owned());
            self.stop = None;
        } else if let Some(error) = self.error.as_mut() {
            error.push_str("\n\n");
            error.push_str(super::TERMINAL_PERSISTENCE_FAILURE);
        } else {
            self.error = Some(super::TERMINAL_PERSISTENCE_FAILURE.to_owned());
        }
        self.status = AgentStatus::Failed;
    }
}

/// Project an [`AgentStepResult`] outcome into a [`ChildOutcomeSummary`].
///
/// Only [`AgentStepResult::Completed`] maps to success. A child that bailed
/// out — schema budget exhausted, max iterations, timeout, cancellation,
/// truncation — surfaces as `Failed` with an explanatory error (including
/// any partial output) *and* the typed [`AgentStopReason`], so the parent
/// never mistakes an unfinished child for a finished one and embedders can
/// branch on the reason without parsing strings.
///
/// `delivered_children_usage` is the wrapper's snapshot of the child's
/// shared [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
/// accumulator, used only on the hard-error arm where no step result
/// exists to carry `children_usage` out of the loop — the child's own
/// usage is honestly unknown there, but grandchild subtrees its loop had
/// already delivered are real spend (W3.6). On every `Ok` arm the value
/// from the step result is authoritative (the two agree by construction:
/// the arm is a snapshot of the same accumulator).
pub(crate) fn extract_outcome_summary(
    outcome: Result<AgentStepResult, NornError>,
    delivered_children_usage: Usage,
) -> ChildOutcomeSummary {
    let stopped = |error: String, stop: AgentStopReason, usage: Usage, children_usage: Usage| {
        ChildOutcomeSummary {
            status: AgentStatus::Failed,
            output_text: None,
            error: Some(error),
            stop: Some(stop),
            usage,
            children_usage,
        }
    };
    match outcome {
        Ok(AgentStepResult::Completed {
            output,
            usage,
            children_usage,
        }) => ChildOutcomeSummary {
            status: AgentStatus::Completed,
            output_text: value_to_text(&output),
            error: None,
            stop: None,
            usage,
            children_usage,
        },
        Ok(AgentStepResult::Refused {
            refusal,
            iterations,
            usage,
            children_usage,
        }) => stopped(
            format!("sub-agent refused the request: {refusal}"),
            AgentStopReason::Refused { iterations },
            usage,
            children_usage,
        ),
        Ok(AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
            children_usage,
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
                children_usage,
            )
        }
        Ok(AgentStepResult::MaxIterationsReached {
            usage,
            children_usage,
        }) => stopped(
            "sub-agent reached its max-iterations cap before completing its task".to_owned(),
            AgentStopReason::MaxIterationsReached,
            usage,
            children_usage,
        ),
        Ok(AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
            children_usage,
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
                children_usage,
            )
        }
        Ok(AgentStepResult::Cancelled {
            usage,
            children_usage,
        }) => stopped(
            "sub-agent was cancelled before completing its task".to_owned(),
            AgentStopReason::Cancelled,
            usage,
            children_usage,
        ),
        Ok(AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            children_usage,
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
                children_usage,
            )
        }
        // Hard error: `run_agent_step`'s `Err` path returns a bare
        // `NornError` with no usage attached, so the child's *own* usage
        // accumulated across provider calls made *before* the error is
        // unrecoverable here. `Usage::default()` (all zeros) therefore
        // means "unknown", not "no tokens consumed" — the lifecycle
        // `Completed` event and the result channel inherit this
        // limitation (documented on `ChildOutcomeSummary::usage` and
        // `SubagentCompletion::usage`). Delivered grandchild subtrees,
        // by contrast, survive on the shared accumulator and are folded
        // in — partial truth beats silent loss (W3.6). This arm's
        // snapshot selection is pinned at unit level here
        // (`outcome_summary_loop_error_is_failure`) and the equivalent
        // survive-the-loss guarantee has end-to-end coverage on the
        // panic path
        // (`panicked_mid_tree_child_still_rolls_up_delivered_grandchild_usage`
        // in spawn.rs) — the two paths read the identical snapshot.
        Err(err) => ChildOutcomeSummary {
            status: AgentStatus::Failed,
            output_text: None,
            error: Some(err.to_string()),
            stop: None,
            usage: Usage::default(),
            children_usage: delivered_children_usage,
        },
    }
}

/// Project a caught panic message onto the same failure summary as a
/// panicked inner task.
pub(super) fn panic_outcome_summary(
    message: String,
    delivered_children_usage: Usage,
) -> ChildOutcomeSummary {
    ChildOutcomeSummary {
        status: AgentStatus::Failed,
        output_text: None,
        error: Some(message),
        stop: None,
        usage: Usage::default(),
        children_usage: delivered_children_usage,
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

    /// Distinct from [`usage_fixture`] so a swap or double count between
    /// own usage and children usage always trips an assertion.
    fn children_fixture() -> Usage {
        Usage {
            input_tokens: 7,
            output_tokens: 3,
            ..Usage::default()
        }
    }

    /// Fix 6: only `Completed` maps to success — and carries no stop
    /// reason. The child's accumulated usage surfaces on the summary,
    /// and the step result's `children_usage` rides alongside it (W3.6).
    #[test]
    fn outcome_summary_completed_is_success() {
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::Completed {
                output: serde_json::json!("all done"),
                usage: usage_fixture(),
                children_usage: children_fixture(),
            }),
            Usage::default(),
        );
        assert_eq!(summary.status, AgentStatus::Completed);
        assert_eq!(summary.output_text.as_deref(), Some("all done"));
        assert!(summary.error.is_none());
        assert!(summary.stop.is_none());
        assert_eq!(summary.usage.input_tokens, 11);
        assert_eq!(summary.usage.output_tokens, 4);
        assert_eq!(
            summary.children_usage.input_tokens, 7,
            "children_usage must come from the step result, not the fallback",
        );
        assert_eq!(summary.children_usage.output_tokens, 3);
    }

    #[test]
    fn terminal_persistence_downgrades_success_without_losing_usage() {
        let mut summary = extract_outcome_summary(
            Ok(AgentStepResult::Completed {
                output: serde_json::json!("completed result"),
                usage: usage_fixture(),
                children_usage: children_fixture(),
            }),
            Usage::default(),
        );

        summary.downgrade_terminal_persistence();

        assert_eq!(summary.status, AgentStatus::Failed);
        assert_eq!(summary.output_text.as_deref(), Some("completed result"));
        assert_eq!(
            summary.error.as_deref(),
            Some(super::super::TERMINAL_PERSISTENCE_FAILURE),
        );
        assert!(summary.stop.is_none());
        assert_eq!(summary.usage.input_tokens, 11);
        assert_eq!(summary.children_usage.input_tokens, 7);
    }

    #[test]
    fn terminal_persistence_appends_to_existing_failure() {
        let mut summary = extract_outcome_summary(
            Ok(AgentStepResult::TimedOut {
                elapsed: Duration::from_secs(12),
                iterations: 3,
                partial_output: None,
                usage: usage_fixture(),
                children_usage: children_fixture(),
            }),
            Usage::default(),
        );
        let original_error = summary.error.clone().unwrap_or_default();
        let original_stop = summary.stop.clone();

        summary.downgrade_terminal_persistence();

        let error = summary.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with(&original_error),
            "original diagnosis: {error}"
        );
        assert!(error.ends_with(super::super::TERMINAL_PERSISTENCE_FAILURE));
        assert_eq!(summary.stop, original_stop);
        assert_eq!(summary.usage.output_tokens, 4);
        assert_eq!(summary.children_usage.output_tokens, 3);
    }

    /// Fix 6: `MaxIterationsReached` surfaces as a failure the parent sees,
    /// with the typed stop reason and the accumulated usage on the summary.
    #[test]
    fn outcome_summary_max_iterations_is_failure() {
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::MaxIterationsReached {
                usage: usage_fixture(),
                children_usage: children_fixture(),
            }),
            Usage::default(),
        );
        assert_eq!(summary.status, AgentStatus::Failed);
        let error = summary.error.unwrap_or_default();
        assert!(error.contains("max-iterations"), "{error}");
        assert_eq!(summary.stop, Some(AgentStopReason::MaxIterationsReached));
        assert_eq!(summary.usage.input_tokens, 11, "usage must be preserved");
        assert_eq!(
            summary.children_usage.input_tokens, 7,
            "children_usage must be preserved on early stops too",
        );
    }

    /// Fix 6: `SchemaUnreachable` surfaces as a failure carrying the
    /// validation errors, the best attempt, and the typed stop reason.
    #[test]
    fn outcome_summary_schema_unreachable_is_failure() {
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::SchemaUnreachable {
                best_attempt: Some(serde_json::json!("nearly valid")),
                validation_errors: vec!["missing field `x`".to_owned()],
                attempts: 5,
                usage: Usage::default(),
                children_usage: Usage::default(),
            }),
            Usage::default(),
        );
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
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::TimedOut {
                elapsed: Duration::from_secs(12),
                iterations: 3,
                partial_output: Some(serde_json::json!("half a report")),
                usage: usage_fixture(),
                children_usage: Usage::default(),
            }),
            Usage::default(),
        );
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
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::Cancelled {
                usage: Usage::default(),
                children_usage: Usage::default(),
            }),
            Usage::default(),
        );
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.error.unwrap_or_default().contains("cancelled"));
        assert_eq!(summary.stop, Some(AgentStopReason::Cancelled));
    }

    /// A truncated child (max-tokens / content-filter stop) surfaces as a
    /// failure carrying the partial text and the typed stop reason — never
    /// as a completed child.
    #[test]
    fn outcome_summary_truncated_is_failure() {
        let summary = extract_outcome_summary(
            Ok(AgentStepResult::Truncated {
                kind: TruncationKind::MaxTokens,
                partial_text: Some("partial answ".to_owned()),
                iterations: 2,
                usage: Usage::default(),
                children_usage: Usage::default(),
            }),
            Usage::default(),
        );
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

    /// A caught child panic projects onto an honest failure
    /// summary: Failed status, an error naming the panic, no stop reason,
    /// unknown (zero) own usage — and the delivered grandchild subtrees
    /// still present from the shared accumulator (W3.6).
    #[test]
    #[allow(clippy::panic, clippy::expect_used)]
    fn panicked_outcome_summary_reports_honest_failure() {
        let summary = panic_outcome_summary(
            "sub-agent task panicked before completing: dependency exploded".to_owned(),
            children_fixture(),
        );
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.output_text.is_none());
        let error = summary.error.unwrap_or_default();
        assert!(
            error.contains("panicked before completing"),
            "error must say the task panicked before completion: {error}"
        );
        assert!(
            error.contains("dependency exploded"),
            "error must surface the panic detail: {error}"
        );
        assert!(summary.stop.is_none());
        assert_eq!(
            summary.usage.input_tokens, 0,
            "usage is unknown after a panic — zeros, never invented numbers"
        );
        assert_eq!(
            summary.children_usage.input_tokens, 7,
            "delivered grandchild subtrees survive the panic via the shared accumulator",
        );
        assert_eq!(summary.children_usage.output_tokens, 3);
    }

    /// A loop error maps to failure with the error text preserved and no
    /// stop reason (the run errored; it did not stop early). Own usage
    /// is unknown-zeros, but delivered grandchild subtrees still fold in
    /// from the wrapper's accumulator snapshot (W3.6).
    #[test]
    fn outcome_summary_loop_error_is_failure() {
        let summary = extract_outcome_summary(
            Err(NornError::Session(
                crate::error::SessionError::StorageError {
                    reason: "disk gone".to_owned(),
                },
            )),
            children_fixture(),
        );
        assert_eq!(summary.status, AgentStatus::Failed);
        assert!(summary.output_text.is_none());
        assert!(summary.error.unwrap_or_default().contains("disk gone"));
        assert!(summary.stop.is_none());
        assert_eq!(
            summary.usage.input_tokens, 0,
            "hard errors carry no usage on the runner's Err path — zeros mean unknown"
        );
        assert_eq!(
            summary.children_usage.input_tokens, 7,
            "the hard-error arm must take children_usage from the delivered snapshot",
        );
    }
}
