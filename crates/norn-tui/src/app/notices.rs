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

/// Retain child lifecycle and compact activity with the actual emitting identity.
pub(super) fn child_event(
    state: &mut AppState,
    event: &norn::provider::AgentEvent,
) -> Result<(), TuiError> {
    use norn::provider::agent_event::AgentEventKind;
    use norn::provider::events::ProviderEvent;
    let label = match &event.event {
        AgentEventKind::Observed(observed) => Some(format!(
            "Unexpected scoped child event from execution {}",
            observed.scope().execution()
        )),
        AgentEventKind::Provider(provider) => match provider {
            ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                ..
            } => {
                let description = crate::app::helpers::extract_tool_use_description(arguments)
                    .unwrap_or_else(|| "description unavailable".to_owned());
                Some(format!("{name}: {description} (call {call_id})"))
            }
            ProviderEvent::ToolResult {
                tool_call_id,
                tool_name,
                duration_ms,
                ..
            } => Some(format!(
                "{tool_name} returned (call {tool_call_id}, {duration_ms} ms)"
            )),
            ProviderEvent::Error { error } => Some(format!("Error: {error}")),
            ProviderEvent::Done { usage, .. } => Some(format!(
                "Provider completed ({} input / {} output tokens)",
                usage.input_tokens, usage.output_tokens
            )),
            ProviderEvent::TextDelta { .. }
            | ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::RefusalDelta { .. }
            | ProviderEvent::RefusalComplete { .. }
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::ReasoningItemDone { .. }
            | ProviderEvent::ResponseItemDone { .. }
            | ProviderEvent::ResponseStreamEvent { .. }
            | ProviderEvent::ResponseAudioFrame { .. }
            | ProviderEvent::Compaction { .. } => None,
        },
        AgentEventKind::Subagent(lifecycle) => Some(match lifecycle {
            norn::provider::agent_event::SubagentLifecycle::Started { descriptor, .. } => {
                format!("Started {} ({})", descriptor.role, descriptor.model)
            }
            norn::provider::agent_event::SubagentLifecycle::Completed {
                succeeded,
                error,
                stop,
                ..
            } => format!("Completed (succeeded: {succeeded}, error: {error:?}, stop: {stop:?})"),
        }),
        AgentEventKind::Message(message) => Some(match message {
            norn::provider::agent_event::AgentMessageLifecycle::Sent {
                message_id,
                to,
                kind,
                ..
            } => format!("Message {message_id} sent to {to} ({})", kind.as_str()),
            norn::provider::agent_event::AgentMessageLifecycle::Delivered {
                message_id,
                seq,
                ..
            } => format!("Message {message_id} delivered (sequence {seq:?})"),
        }),
        AgentEventKind::StreamRetry(retry) => Some(format!(
            "Retry attempt {} in {} ms ({})",
            retry.attempt, retry.delay_ms, retry.error_class
        )),
        AgentEventKind::Compaction(compaction) => Some(format!(
            "Compaction {} ({} → {} tokens)",
            compaction.compaction_id, compaction.tokens_before, compaction.tokens_after
        )),
        AgentEventKind::McpChannel(delivery) => Some(format!(
            "Channel {} event {} delivered",
            delivery.source, delivery.event_id
        )),
        AgentEventKind::UsageEstimate(_) => None,
    };
    if let Some(label) = label {
        state.transcript.notice(
            ViewItemKind::Child,
            &format!("Child {} ({}): {label}", event.agent_role, event.agent_id),
            None,
        )?;
        state.screen.dirty = true;
    }
    Ok(())
}
