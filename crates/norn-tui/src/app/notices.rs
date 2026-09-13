//! Local retained notices preserve content and attribution without provider or command authority.

use norn::session_view::{ItemId, ViewItemKind};

use crate::TuiError;

use super::state::AppState;

/// Retain a local control/status notice; large details remain a ranged local body.
pub(super) fn notice(
    state: &mut AppState,
    label: &str,
    detail: Option<&str>,
) -> Result<ItemId, TuiError> {
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    state.transcript.notice(ViewItemKind::Notice, label, detail)
}

/// Retain an explicit frontend/runtime failure with its original approved details.
pub(super) fn error(state: &mut AppState, label: &str, detail: &str) -> Result<ItemId, TuiError> {
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    state
        .transcript
        .notice(ViewItemKind::Error, label, Some(detail))
}

/// Retain human input until its exact producer-owned committed receipt is available.
pub(super) fn input(state: &mut AppState, label: &str, text: &str) -> Result<ItemId, TuiError> {
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    state
        .transcript
        .notice(ViewItemKind::Input, label, Some(text))
}

/// Retain an actual child result with child identity, without relabelling it as a human.
pub(super) fn child_result(
    state: &mut AppState,
    child_id: uuid::Uuid,
    role: &str,
    text: &str,
) -> Result<ItemId, TuiError> {
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    state.transcript.notice(
        ViewItemKind::Child,
        &format!("Child {role} ({child_id}) completed"),
        Some(text),
    )
}

/// Failures stay visible to the parent; routine activity already belongs to the agent panel.
pub(super) fn child_event(
    state: &mut AppState,
    event: &norn::provider::AgentEvent,
) -> Result<(), TuiError> {
    use norn::provider::agent_event::{AgentEventKind, SubagentLifecycle};
    use norn::provider::events::ProviderEvent;
    let kind = match &event.event {
        AgentEventKind::Observed(observed) => observed.event(),
        native => native,
    };
    let detail = match kind {
        AgentEventKind::Provider(ProviderEvent::Error { error }) => Some(error.to_string()),
        AgentEventKind::Subagent(SubagentLifecycle::Completed {
            succeeded: false,
            error,
            stop,
            ..
        }) => Some(format!("Child failed; error: {error:?}; stop: {stop:?}")),
        _ => None,
    };
    if let Some(detail) = detail {
        error(
            state,
            &format!("Child {} ({}) failed", event.agent_role, event.agent_id),
            &detail,
        )?;
    }
    Ok(())
}
