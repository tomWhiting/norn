//! Typed final usage extraction and retained frontend errors.

use super::AppState;
use crate::TuiError;
use norn::agent_loop::config::AgentStepResult;
use norn::provider::usage::Usage;

/// Extract the usage field from any completed agent-step outcome.
pub fn extract_usage(result: &AgentStepResult) -> Usage {
    match result {
        AgentStepResult::Completed { usage, .. }
        | AgentStepResult::Refused { usage, .. }
        | AgentStepResult::SchemaUnreachable { usage, .. }
        | AgentStepResult::MaxIterationsReached { usage, .. }
        | AgentStepResult::Cancelled { usage, .. }
        | AgentStepResult::TimedOut { usage, .. }
        | AgentStepResult::Truncated { usage, .. } => usage.clone(),
    }
}

/// Retain an explicit error with its approved original details.
pub(crate) fn write_error_line(state: &mut AppState, message: &str) -> Result<(), TuiError> {
    crate::app::notices::error(state, "Error", message)?;
    Ok(())
}

/// Explain an actual incomplete channel wake without changing inbox ownership.
pub(crate) fn channel_wake_pause_reason(
    result: Option<&Result<AgentStepResult, norn::error::NornError>>,
    cancelled: bool,
) -> Option<String> {
    if cancelled {
        return Some("turn cancelled".to_owned());
    }
    let reason = match result {
        Some(Ok(AgentStepResult::Completed { .. } | AgentStepResult::Refused { .. })) => {
            return None;
        }
        Some(Ok(AgentStepResult::Cancelled { .. })) => "turn cancelled",
        Some(Ok(AgentStepResult::MaxIterationsReached { .. })) => "iteration limit reached",
        Some(Ok(AgentStepResult::TimedOut { .. })) => "turn deadline elapsed",
        Some(Ok(AgentStepResult::Truncated { .. })) => "model output stopped early",
        Some(Ok(AgentStepResult::SchemaUnreachable { .. })) => "output contract was not satisfied",
        Some(Err(error)) => return Some(error.to_string()),
        None => "agent event stream closed before the turn returned",
    };
    Some(reason.to_owned())
}
