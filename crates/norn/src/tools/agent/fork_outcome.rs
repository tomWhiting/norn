//! Fork outcome projection and completion recording for
//! [`crate::tools::agent::fork_tool::ForkTool`] (R1, R4).
//!
//! Houses [`ForkOutcome`] and the helpers that classify a fork's step
//! result, mark the registry terminal transition, and append the honest
//! [`SessionEvent::ForkComplete`] reference to the parent's timeline
//! (`forked_session_id: None` for ephemeral forks — never a registry-id
//! stand-in). Split from the former `fork_pipeline.rs` per the
//! child-persistence design ruling D-b; [`super::fork_context`] is the
//! sibling cluster, linked only through [`ForkOutcome`].

use std::time::Instant;

use parking_lot::RwLock;
use uuid::Uuid;

use super::reclaim::log_terminal_transition_violation;
use crate::agent::output::AgentStopReason;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::error::NornError;
use crate::r#loop::runner::AgentStepResult;
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

/// Outcome bundle the fork's `tokio::spawn` task hands back to the parent's
/// timeline and result channel.
pub(crate) struct ForkOutcome {
    pub(crate) status: AgentStatus,
    pub(crate) result_summary: serde_json::Value,
    pub(crate) usage: Usage,
    /// Summed `subtree_usage` of every grandchild result the fork's loop
    /// delivered (W3.6 usage rollup) — from the
    /// [`AgentStepResult`] arm on every loop outcome, and from the
    /// wrapper's shared
    /// [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
    /// snapshot on the hard-error and panic paths. Disjoint from
    /// [`Self::usage`]: `usage + children_usage` is the fork's subtree
    /// total with each agent counted exactly once.
    pub(crate) children_usage: Usage,
    pub(crate) duration: std::time::Duration,
    pub(crate) error_message: Option<String>,
    /// Typed stop reason when the fork's run stopped early without
    /// completing; `None` on completion or hard error.
    pub(crate) stop: Option<AgentStopReason>,
}

/// Project the agent loop's result into a transport-friendly payload.
///
/// Only [`AgentStepResult::Completed`] is a success. `SchemaUnreachable`,
/// `MaxIterationsReached`, `TimedOut`, `Cancelled`, and `Truncated`
/// children surface as failures with an explanatory `error_message` and the
/// typed [`AgentStopReason`] — the parent must never read a bailed-out fork
/// as a completed one. Partial output (best schema attempt, pre-timeout
/// text, pre-truncation text) is preserved on `result_summary` for the
/// parent's `ForkComplete` audit event.
///
/// Pure — the registry transition lives in [`mark_fork_terminal`] so the
/// wrapper can fire `SubagentHook::on_subagent_stop` between projection
/// and marking (a hook Block suppresses the transition, mirroring spawn).
///
/// `delivered_children_usage` is the wrapper's snapshot of the fork's
/// shared [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
/// accumulator, used only on the hard-error arm where no step result
/// exists to carry `children_usage` out of the loop (W3.6); every `Ok`
/// arm reads the authoritative value from the step result.
pub(super) fn project_fork_outcome(
    outcome: Result<AgentStepResult, NornError>,
    started: Instant,
    delivered_children_usage: Usage,
) -> ForkOutcome {
    let duration = started.elapsed();
    match outcome {
        Ok(result) => classify_step_result(result, duration),
        // Hard error: `run_agent_step`'s `Err` path carries no usage, so
        // tokens the fork itself consumed before a mid-run error are
        // unrecoverable here — `Usage::default()` means "unknown", not
        // "none consumed" (same limitation as `extract_outcome_summary`
        // on the spawn side; the `ForkComplete` event and lifecycle
        // `Completed` inherit it). Delivered grandchild subtrees survive
        // on the shared accumulator and are folded in (W3.6).
        Err(err) => ForkOutcome {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage: Usage::default(),
            children_usage: delivered_children_usage,
            duration,
            error_message: Some(err.to_string()),
            stop: None,
        },
    }
}

/// Apply the fork's terminal registry transition for `status`
/// (`Completed` walks Completing → Completed; anything else marks
/// `Failed`).
///
/// The wrapper is the sole owner of a live fork's terminal transition
/// (see [`super::reclaim`]), so a transition failure here is an
/// invariant violation: it is logged loudly via
/// [`log_terminal_transition_violation`] but never propagated — the
/// wrapper still owes result delivery.
pub(super) fn mark_fork_terminal(
    registry: &RwLock<AgentRegistry>,
    fork_id: Uuid,
    status: AgentStatus,
) {
    let mut reg = registry.write();
    if status == AgentStatus::Completed {
        if let Err(e) = reg.mark_completing(fork_id) {
            log_terminal_transition_violation(&reg, fork_id, "fork", &e);
        }
        if let Err(e) = reg.mark_completed(fork_id) {
            log_terminal_transition_violation(&reg, fork_id, "fork", &e);
        }
    } else if let Err(e) = reg.mark_failed(fork_id) {
        log_terminal_transition_violation(&reg, fork_id, "fork", &e);
    }
}

/// Project a fork task that never produced an outcome — its inner `tokio`
/// task panicked or was aborted, surfacing as a
/// [`tokio::task::JoinError`] — onto the failure payload the wrapper
/// delivers. Mirrors `panicked_outcome_summary` on the spawn side: the
/// wrapper still appends `ForkComplete`, emits the lifecycle `Completed`,
/// delivers the result, and transitions the registry, so observers never
/// see a dangling `Started`. Own usage is [`Usage::default`] (unknown —
/// the panicked task took its accumulated usage with it), while
/// `delivered_children_usage` — the wrapper's snapshot of the shared
/// [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)
/// accumulator, which survives the unwound task — still carries every
/// grandchild subtree the fork's loop delivered before the panic (W3.6).
pub(super) fn panicked_fork_outcome(
    join_error: &tokio::task::JoinError,
    duration: std::time::Duration,
    delivered_children_usage: Usage,
) -> ForkOutcome {
    ForkOutcome {
        status: AgentStatus::Failed,
        result_summary: serde_json::Value::Null,
        usage: Usage::default(),
        children_usage: delivered_children_usage,
        duration,
        error_message: Some(format!(
            "fork task terminated without an outcome (panicked or aborted): {join_error}"
        )),
        stop: None,
    }
}

/// Map an [`AgentStepResult`] onto the fork's terminal [`ForkOutcome`]
/// projection. Pure — no registry side effects — so every variant's mapping
/// is unit-testable.
fn classify_step_result(result: AgentStepResult, duration: std::time::Duration) -> ForkOutcome {
    struct Projection {
        status: AgentStatus,
        result_summary: serde_json::Value,
        usage: Usage,
        children_usage: Usage,
        error_message: Option<String>,
        stop: Option<AgentStopReason>,
    }
    let project = |p: Projection| ForkOutcome {
        status: p.status,
        result_summary: p.result_summary,
        usage: p.usage,
        children_usage: p.children_usage,
        duration,
        error_message: p.error_message,
        stop: p.stop,
    };
    match result {
        AgentStepResult::Completed {
            output,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Completed,
            result_summary: output,
            usage,
            children_usage,
            error_message: None,
            stop: None,
        }),
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: best_attempt.unwrap_or(serde_json::Value::Null),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork could not produce schema-valid output after {attempts} attempts: {}",
                validation_errors.join("; "),
            )),
            stop: Some(AgentStopReason::SchemaUnreachable {
                validation_errors,
                attempts,
            }),
        }),
        AgentStepResult::MaxIterationsReached {
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage,
            children_usage,
            error_message: Some(
                "fork reached its max-iterations cap before completing its task".to_owned(),
            ),
            stop: Some(AgentStopReason::MaxIterationsReached),
        }),
        AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: partial_output.unwrap_or(serde_json::Value::Null),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork timed out after {:.1}s ({iterations} iterations completed); any partial \
                 output is recorded on the fork's session branch",
                elapsed.as_secs_f64(),
            )),
            stop: Some(AgentStopReason::TimedOut {
                elapsed,
                iterations,
            }),
        }),
        AgentStepResult::Cancelled {
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: serde_json::Value::Null,
            usage,
            children_usage,
            error_message: Some("fork was cancelled before completing its task".to_owned()),
            stop: Some(AgentStopReason::Cancelled),
        }),
        AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            children_usage,
        } => project(Projection {
            status: AgentStatus::Failed,
            result_summary: partial_text.map_or(serde_json::Value::Null, serde_json::Value::String),
            usage,
            children_usage,
            error_message: Some(format!(
                "fork output was truncated ({}) before it completed its task; the partial \
                 output is recorded on the fork's session branch",
                kind.as_str(),
            )),
            stop: Some(AgentStopReason::Truncated { kind, iterations }),
        }),
    }
}

/// Append a [`SessionEvent::ForkComplete`] to the parent's store (R4).
///
/// Best-effort: a failure here is logged but does not propagate. The fork's
/// own audit trail already lives on its own session file — this event is
/// the completion reference on the parent's timeline.
///
/// `forked_session_id` is `Some` only when the fork has a real session
/// file; an ephemeral fork records honest `None` — the old fallback to the
/// registry id (a durable pointer to a session existing nowhere on disk)
/// is gone.
pub(super) fn append_fork_complete(
    parent_store: &EventStore,
    forked_session_id: Option<String>,
    outcome: &ForkOutcome,
    fork_id: Uuid,
) {
    let event = SessionEvent::ForkComplete {
        base: EventBase::new(parent_store.last_event_id()),
        forked_session_id,
        result_summary: outcome.result_summary.clone(),
        usage: EventUsage {
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cache_read_tokens: outcome.usage.cache_read_tokens,
            cache_write_tokens: outcome.usage.cache_write_tokens,
            cost_usd: outcome.usage.cost_usd,
        },
        duration_ms: u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
    };
    if let Err(e) = parent_store.append(event) {
        tracing::warn!(
            fork_id = %fork_id,
            error = %e,
            "fork: failed to append ForkComplete event to parent store",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::child_policy::ChildPolicy;
    use crate::agent::fork::format_fork_outcome;
    use crate::error::SessionError;
    use std::sync::Arc;

    /// Documented-proposal policy used by tests — a deliberate test-caller
    /// choice, never a library default.
    fn test_policy() -> ChildPolicy {
        use crate::agent::child_policy::{DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        }
    }

    /// Reserve and confirm a fork entry, returning the shared registry and id.
    fn registry_with_fork() -> Result<(Arc<RwLock<AgentRegistry>>, Uuid), String> {
        let registry = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
            &registry,
            "/fork/test".to_owned(),
            "fork".to_owned(),
            "haiku".to_owned(),
            None,
            test_policy(),
            None,
        )
        .map_err(|e| format!("reserve: {e}"))?;
        let id = guard.id();
        guard.confirm().map_err(|e| format!("confirm: {e}"))?;
        Ok((registry, id))
    }

    fn finish(result: AgentStepResult) -> Result<ForkOutcome, String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = project_fork_outcome(Ok(result), Instant::now(), Usage::default());
        mark_fork_terminal(&registry, fork_id, outcome.status);
        // Terminal transitions free the path and leave the entry observable
        // (terminal status) until an observer reclaims it (fix 10).
        let status = registry
            .read()
            .get(fork_id)
            .ok_or("terminal fork entry must stay observable until reclaimed")?
            .status;
        if !status.is_terminal() {
            return Err(format!("fork entry must be terminal, got {status:?}"));
        }
        if !registry.write().remove_terminal(fork_id) {
            return Err("terminal fork entry must be reclaimable".to_owned());
        }
        Ok(outcome)
    }

    /// Assert the outcome maps to a non-success the parent can see.
    fn assert_failure(outcome: &ForkOutcome, expected_fragment: &str) -> Result<(), String> {
        if outcome.status != AgentStatus::Failed {
            return Err(format!("expected Failed status, got {:?}", outcome.status));
        }
        let error = outcome
            .error_message
            .as_deref()
            .ok_or("failure outcome must carry an error message")?;
        if !error.contains(expected_fragment) {
            return Err(format!(
                "error '{error}' must mention '{expected_fragment}'"
            ));
        }
        let (succeeded, message, channel_error) = format_fork_outcome(Uuid::new_v4(), outcome, &[]);
        if succeeded {
            return Err("the result-channel projection must report non-success".to_owned());
        }
        if channel_error.is_none() {
            return Err("the result-channel projection must carry the error".to_owned());
        }
        if !message.contains("FORK FAILED") {
            return Err(format!(
                "parent-visible message must say FORK FAILED: {message}"
            ));
        }
        Ok(())
    }

    /// Fix 6: `Completed` is the only success.
    #[test]
    fn finish_fork_completed_is_success() -> Result<(), String> {
        let outcome = finish(AgentStepResult::Completed {
            output: serde_json::json!({"response": "done", "requirements": {}}),
            usage: Usage::default(),
            children_usage: Usage::default(),
        })?;
        if outcome.status != AgentStatus::Completed {
            return Err(format!("expected Completed, got {:?}", outcome.status));
        }
        if outcome.error_message.is_some() {
            return Err("success must not carry an error message".to_owned());
        }
        let (succeeded, _, error) = format_fork_outcome(Uuid::new_v4(), &outcome, &[]);
        if !succeeded || error.is_some() {
            return Err("completed fork must project as success".to_owned());
        }
        Ok(())
    }

    /// Fix 6: `MaxIterationsReached` surfaces as non-success.
    #[test]
    fn finish_fork_max_iterations_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
            children_usage: Usage::default(),
        })?;
        assert_failure(&outcome, "max-iterations")
    }

    /// Fix 6: `SchemaUnreachable` surfaces as non-success while preserving
    /// the best attempt for the parent's audit event.
    #[test]
    fn finish_fork_schema_unreachable_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::SchemaUnreachable {
            best_attempt: Some(serde_json::json!({"response": "almost"})),
            validation_errors: vec!["missing field `requirements`".to_owned()],
            attempts: 3,
            usage: Usage::default(),
            children_usage: Usage::default(),
        })?;
        assert_failure(&outcome, "schema-valid")?;
        if outcome.result_summary.get("response").is_none() {
            return Err("best attempt must be preserved on the result summary".to_owned());
        }
        let error = outcome.error_message.as_deref().unwrap_or_default();
        if !error.contains("missing field `requirements`") {
            return Err(format!("validation errors must surface: {error}"));
        }
        Ok(())
    }

    /// Fix 6: `TimedOut` surfaces as non-success while preserving partial
    /// output, accumulated usage, and the typed stop reason.
    #[test]
    fn finish_fork_timed_out_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::TimedOut {
            elapsed: std::time::Duration::from_secs(30),
            iterations: 4,
            partial_output: Some(serde_json::json!("partial text")),
            usage: Usage {
                input_tokens: 50,
                ..Usage::default()
            },
            children_usage: Usage {
                input_tokens: 6,
                ..Usage::default()
            },
        })?;
        assert_failure(&outcome, "timed out")?;
        if outcome.result_summary != serde_json::json!("partial text") {
            return Err("partial output must be preserved on the result summary".to_owned());
        }
        if outcome.usage.input_tokens != 50 {
            return Err("timed-out usage must be preserved on the fork outcome".to_owned());
        }
        if outcome.stop
            != Some(AgentStopReason::TimedOut {
                elapsed: std::time::Duration::from_secs(30),
                iterations: 4,
            })
        {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// Fix 6: `Cancelled` surfaces as non-success with the typed stop
    /// reason.
    #[test]
    fn finish_fork_cancelled_is_failure() -> Result<(), String> {
        let outcome = finish(AgentStepResult::Cancelled {
            usage: Usage::default(),
            children_usage: Usage::default(),
        })?;
        assert_failure(&outcome, "cancelled")?;
        if outcome.stop != Some(AgentStopReason::Cancelled) {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// A truncated fork (max-tokens / content-filter stop) surfaces as
    /// non-success while preserving the partial text, usage, and the typed
    /// stop reason — never as a completed fork.
    #[test]
    fn finish_fork_truncated_is_failure() -> Result<(), String> {
        use crate::r#loop::config::TruncationKind;
        let outcome = finish(AgentStepResult::Truncated {
            kind: TruncationKind::ContentFilter,
            partial_text: Some("cut short".to_owned()),
            iterations: 2,
            usage: Usage {
                output_tokens: 9,
                ..Usage::default()
            },
            children_usage: Usage::default(),
        })?;
        assert_failure(&outcome, "truncated")?;
        if outcome.result_summary != serde_json::json!("cut short") {
            return Err("partial text must be preserved on the result summary".to_owned());
        }
        if outcome.usage.output_tokens != 9 {
            return Err("truncated usage must be preserved on the fork outcome".to_owned());
        }
        if outcome.stop
            != Some(AgentStopReason::Truncated {
                kind: TruncationKind::ContentFilter,
                iterations: 2,
            })
        {
            return Err(format!(
                "typed stop reason must surface, got {:?}",
                outcome.stop
            ));
        }
        Ok(())
    }

    /// A loop error keeps the pre-existing failure mapping.
    #[test]
    fn finish_fork_loop_error_is_failure() -> Result<(), String> {
        let (registry, fork_id) = registry_with_fork()?;
        let outcome = project_fork_outcome(
            Err(NornError::Session(SessionError::StorageError {
                reason: "disk gone".to_owned(),
            })),
            Instant::now(),
            Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
        );
        mark_fork_terminal(&registry, fork_id, outcome.status);
        let status = registry
            .read()
            .get(fork_id)
            .ok_or("failed fork entry must stay observable until reclaimed")?
            .status;
        if status != AgentStatus::Failed {
            return Err(format!("fork entry must be Failed, got {status:?}"));
        }
        assert_failure(&outcome, "disk gone")
    }

    /// A panicked/aborted fork task projects onto an honest failure
    /// payload: Failed status, an error naming the missing outcome, no
    /// stop reason, and unknown (zero) usage — so the wrapper's
    /// `ForkComplete` / lifecycle / result-channel obligations are all
    /// satisfiable after a dependency panic.
    #[tokio::test]
    #[allow(clippy::panic, clippy::expect_used)]
    async fn panicked_fork_outcome_reports_honest_failure() -> Result<(), String> {
        let join_error = tokio::spawn(async { panic!("dependency exploded") })
            .await
            .expect_err("task must panic");
        let outcome = panicked_fork_outcome(
            &join_error,
            std::time::Duration::from_millis(7),
            Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
        );
        if outcome.status != AgentStatus::Failed {
            return Err(format!("expected Failed, got {:?}", outcome.status));
        }
        let error = outcome
            .error_message
            .as_deref()
            .ok_or("panic outcome must carry an error message")?;
        if !error.contains("terminated without an outcome") {
            return Err(format!("error must name the missing outcome: {error}"));
        }
        if outcome.stop.is_some() {
            return Err("a panic is not a typed early stop".to_owned());
        }
        if outcome.usage.input_tokens != 0 || outcome.usage.output_tokens != 0 {
            return Err("usage is unknown after a panic — must be zeros".to_owned());
        }
        if outcome.result_summary != serde_json::Value::Null {
            return Err("no result summary exists after a panic".to_owned());
        }
        assert_failure(&outcome, "terminated without an outcome")
    }
}
