//! Durable records for abnormal root-step stops.
//!
//! A root agent step that ends on the wall-clock timeout, cooperative
//! cancellation, or the max-iterations cap previously left **no trace in
//! the agent's own session log** — the typed [`AgentStepResult`] went to
//! the caller, a *child's* abnormal stop reached the parent's store via
//! `subagent.completed`, but a resumed root session could not tell its
//! previous step was cut off (session-fidelity inventory, Gap 6). The
//! same hard cuts also dropped any mid-stream partial output of the
//! aborted provider call on the floor (Gap 7).
//!
//! [`record_abnormal_step_stop`] closes both: on every abnormal exit it
//! appends a `loop.step_stopped` Custom event carrying the typed stop
//! reason and the step's mechanical facts, preceded — when the cut
//! happened mid-provider-stream — by a `loop.partial_output` Custom event
//! carrying the text/thinking the stream had produced, explicitly marked
//! as a hard-cut partial. Both are record-only: resume replays them as
//! history but takes no action from them.
//!
//! These appends run *after* the step result is decided (the timeout path
//! has already dropped the inner future), so a persist failure here never
//! rewrites the outcome: it is logged at error level, matching the exit
//! path's established convention for post-decision audit appends.

use std::time::Duration;

use crate::error::NornError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::compaction::SharedTimeoutState;
use crate::r#loop::config::AgentStepResult;
use crate::r#loop::helpers::append_and_notify;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Event type of the Custom record appended when a root step ends on a
/// timeout, cancellation, or the max-iterations cap.
pub(crate) const STEP_STOPPED_EVENT_TYPE: &str = "loop.step_stopped";

/// Event type of the Custom record carrying the mid-stream partial output
/// of a hard-cut (timed-out or cancelled) provider call.
pub(crate) const PARTIAL_OUTPUT_EVENT_TYPE: &str = "loop.partial_output";

/// Borrowed step facts for [`record_abnormal_step_stop`].
pub(super) struct StepStopContext<'a> {
    /// The step's session event store.
    pub(super) store: &'a EventStore,
    /// Hook registry notified of the appended events (same registry the
    /// step dispatched).
    pub(super) hooks: Option<&'a HookRegistry>,
    /// Shared timeout state: iteration counter and the in-flight partial
    /// capture surviving the dropped inner future.
    pub(super) timeout_state: &'a SharedTimeoutState,
    /// Wall-clock time the step ran.
    pub(super) elapsed: Duration,
    /// The configured `step_timeout` budget, when one was set.
    pub(super) step_timeout: Option<Duration>,
    /// The configured `max_iterations` cap, when one was set.
    pub(super) max_iterations: Option<u32>,
}

/// Persist the typed stop-reason record (and, for mid-stream hard cuts,
/// the partial-output record) for a step that ended abnormally.
///
/// A no-op for every other outcome: completed, truncated (which already
/// persists `loop.truncated`), schema-unreachable, and error returns.
pub(super) async fn record_abnormal_step_stop(
    ctx: StepStopContext<'_>,
    result: &Result<AgentStepResult, NornError>,
) {
    let Ok(outcome) = result else {
        // Hard errors propagate to the caller typed; they are not a
        // stop reason of the step machine.
        return;
    };
    let (stop_reason, iterations) = match outcome {
        AgentStepResult::TimedOut { iterations, .. } => ("timeout", *iterations),
        AgentStepResult::Cancelled { .. } => ("cancelled", ctx.timeout_state.lock().iterations),
        AgentStepResult::MaxIterationsReached { .. } => {
            ("max_iterations", ctx.timeout_state.lock().iterations)
        }
        AgentStepResult::Completed { .. }
        | AgentStepResult::SchemaUnreachable { .. }
        | AgentStepResult::Truncated { .. } => return,
    };

    // Gap 7: a timeout or cancellation that cut the provider call
    // mid-stream leaves the aborted call's deltas in the shared capture
    // (the assembled-response path clears it). Persist them before the
    // stop record so the log reads chronologically: partial content, then
    // the stop that cut it.
    if matches!(
        outcome,
        AgentStepResult::TimedOut { .. } | AgentStepResult::Cancelled { .. }
    ) {
        let partial = ctx.timeout_state.lock().in_flight_partial.take();
        if let Some(partial) = partial.filter(|p| !p.is_empty()) {
            let event = SessionEvent::Custom {
                base: EventBase::new(ctx.store.last_event_id()),
                event_type: PARTIAL_OUTPUT_EVENT_TYPE.to_string(),
                data: serde_json::json!({
                    "stop_reason": stop_reason,
                    "hard_cut": true,
                    "text": partial.text,
                    "thinking": partial.thinking,
                    "text_chars": partial.text.chars().count(),
                    "thinking_chars": partial.thinking.chars().count(),
                }),
            };
            if let Err(error) = append_and_notify(ctx.store, event, ctx.hooks).await {
                tracing::error!(
                    %error,
                    stop_reason,
                    "failed to persist the hard-cut partial-output record; \
                     the aborted call's partial content is lost from the log",
                );
            }
        }
    }

    // Gap 6: the typed stop-reason record on the agent's own timeline.
    let mut data = serde_json::json!({
        "stop_reason": stop_reason,
        "iterations": iterations,
        "elapsed_ms": u64::try_from(ctx.elapsed.as_millis()).unwrap_or(u64::MAX),
    });
    if let Some(object) = data.as_object_mut() {
        if let (AgentStepResult::TimedOut { .. }, Some(budget)) = (outcome, ctx.step_timeout) {
            object.insert(
                "budget_ms".to_string(),
                serde_json::json!(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX)),
            );
        }
        if let (AgentStepResult::MaxIterationsReached { .. }, Some(max)) =
            (outcome, ctx.max_iterations)
        {
            object.insert("max_iterations".to_string(), serde_json::json!(max));
        }
    }
    let event = SessionEvent::Custom {
        base: EventBase::new(ctx.store.last_event_id()),
        event_type: STEP_STOPPED_EVENT_TYPE.to_string(),
        data,
    };
    if let Err(error) = append_and_notify(ctx.store, event, ctx.hooks).await {
        tracing::error!(
            %error,
            stop_reason,
            "failed to persist the step-stop record; the abnormal stop \
             leaves no trace in the session log",
        );
    }
}
