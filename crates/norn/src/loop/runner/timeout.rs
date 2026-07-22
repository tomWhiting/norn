//! Construction of the typed step-timeout result.

use std::time::Instant;

use serde_json::Value;

use crate::r#loop::children_usage::ChildrenUsage;
use crate::r#loop::compaction::SharedTimeoutState;
use crate::r#loop::config::AgentStepResult;

/// Snapshot the shared runner state after the execution budget elapses.
pub(super) fn step_timeout_result(
    timeout_state: &SharedTimeoutState,
    started: Instant,
    children_usage: &ChildrenUsage,
) -> AgentStepResult {
    let snapshot = timeout_state.lock();
    let in_flight_output = snapshot.in_flight_partial.as_ref().and_then(|partial| {
        if !partial.text.is_empty() {
            return Some(Value::String(partial.text.clone()));
        }
        if let Some(refusal) = partial.refusal.clone() {
            return Some(Value::String(refusal));
        }
        partial
            .response_audio
            .map(|reference| serde_json::json!({"response_audio": reference}))
    });
    AgentStepResult::TimedOut {
        elapsed: started.elapsed(),
        iterations: snapshot.iterations,
        partial_output: in_flight_output
            .or_else(|| snapshot.last_assistant_text.clone().map(Value::String)),
        usage: snapshot.usage.clone(),
        children_usage: children_usage.snapshot(),
    }
}
