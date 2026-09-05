//! Mid-turn handling: terminal events, keystrokes, and steered/queued input
//! delivered while an agent step is in flight.

use std::io::Write as _;

use termina::Event;
use termina::Terminal as _;
use termina::event::{KeyEvent, KeyEventKind};
use tokio_util::sync::CancellationToken;

use norn::agent_loop::{ActiveInputDelivery, ActiveInputError, ActiveInputSender};
use norn::provider::agent_event::{AgentEvent, AgentEventKind};
use norn::provider::events::ProviderEvent;

use crate::TuiError;
use crate::input::keybindings::{InputAction, map_key_event};
use crate::render::MarkdownRenderer;
use crate::terminal::setup::TerminalGuard;

use crate::app::active_input::InFlightSubmitMode;
use crate::app::autocomplete::{
    PopupKeyOutcome, dismiss as dismiss_autocomplete, handle_popup_key,
};
use crate::app::dispatch::handle_agent_event;
use crate::app::edit::apply_edit_action;
use crate::app::event_loop::{insert_paste_text, sync_input_for_current_geometry};
use crate::app::helpers::flush_markdown;
use crate::app::render::{park_input_cursor, redraw_panel, render_input, write_user_message};
use crate::app::state::AppState;
use crate::app::streaming::finish_thinking_block;

pub(super) fn handle_mid_turn_event(
    event: Event,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    match event {
        Event::WindowResized(size) => {
            guard.restore_scroll_cursor_clamped()?;
            guard.save_scroll_cursor()?;
            guard.handle_resize(size.rows)?;
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            guard.restore_scroll_cursor_clamped()?;
            guard.save_scroll_cursor()?;
            render_input(state, guard)?;
            guard.terminal_mut().flush()?;
        }
        Event::Paste(text) => {
            insert_paste_text(state, &text);
            sync_input_for_current_geometry(state, guard);
            redraw_panel(state, guard)?;
            render_input(state, guard)?;
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            handle_mid_turn_key(key, state, guard, active_input_tx, cancel, cancel_requested)?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn handle_mid_turn_agent_event(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    agent_ev: AgentEvent,
) -> Result<(), TuiError> {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let before_indicator_key = state.streaming_indicator.repaint_key(cols);
    let before_indicator_height = state.streaming_indicator.height();
    let structural_panel_change = agent_event_needs_panel_redraw(state, &agent_ev);

    guard.restore_scroll_cursor_clamped()?;
    handle_agent_event(state, guard, renderer, agent_ev)?;
    guard.save_scroll_cursor()?;

    let indicator_changed = before_indicator_height != state.streaming_indicator.height()
        || before_indicator_key != state.streaming_indicator.repaint_key(cols);
    if structural_panel_change || indicator_changed {
        redraw_panel(state, guard)?;
        render_input(state, guard)
    } else {
        park_input_cursor(state, guard)
    }
}

fn agent_event_needs_panel_redraw(state: &AppState, agent_ev: &AgentEvent) -> bool {
    match &agent_ev.event {
        AgentEventKind::Provider(event) if agent_ev.agent_id == state.tab_state.root_id() => {
            root_provider_event_needs_panel_redraw(event)
        }
        AgentEventKind::Provider(event) => child_provider_event_needs_panel_redraw(event),
        AgentEventKind::Subagent(_)
        | AgentEventKind::Message(_)
        | AgentEventKind::McpChannel(_)
        | AgentEventKind::UsageEstimate(_)
        // Compaction pushes an activity-log row: the panel must repaint.
        | AgentEventKind::Compaction(_)
        // A retry paints the wait onto the agent row and (for the root)
        // onto the status row, which also grows the panel by one row.
        | AgentEventKind::StreamRetry(_) => true,
    }
}

fn root_provider_event_needs_panel_redraw(event: &ProviderEvent) -> bool {
    !matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::RefusalDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::RefusalComplete { .. }
            | ProviderEvent::ReasoningItemDone { .. }
            | ProviderEvent::ResponseItemDone { .. }
            | ProviderEvent::ResponseStreamEvent { .. }
            | ProviderEvent::ResponseAudioFrame { .. }
            | ProviderEvent::Compaction { .. }
    )
}

fn child_provider_event_needs_panel_redraw(event: &ProviderEvent) -> bool {
    !matches!(
        event,
        ProviderEvent::TextDelta { .. }
            | ProviderEvent::RefusalDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::ToolCallDelta { name: None, .. }
            | ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::RefusalComplete { .. }
            | ProviderEvent::ReasoningItemDone { .. }
            | ProviderEvent::ResponseItemDone { .. }
            | ProviderEvent::ResponseStreamEvent { .. }
            | ProviderEvent::ResponseAudioFrame { .. }
            | ProviderEvent::Compaction { .. }
    )
}

fn handle_mid_turn_key(
    key: KeyEvent,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    if state.autocomplete.is_some()
        && matches!(
            handle_popup_key(key, state, cols, guard.terminal_rows()),
            PopupKeyOutcome::Consumed
        )
    {
        return Ok(());
    }

    let caps = state.terminal_caps.clone();
    let popup_open = state.autocomplete.is_some();
    if let Some(action) = map_key_event(key, &caps, popup_open) {
        handle_mid_turn_action(
            action,
            state,
            guard,
            active_input_tx,
            cancel,
            cancel_requested,
        )?;
        sync_input_for_current_geometry(state, guard);
        redraw_panel(state, guard)?;
        render_input(state, guard)?;
    }
    Ok(())
}

fn handle_mid_turn_action(
    action: InputAction,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    match action {
        InputAction::Submit => {
            submit_mid_turn_input(state, active_input_tx, cancel, cancel_requested)?;
        }
        InputAction::ToggleInFlightSubmitMode => state.in_flight_input.toggle_mode(),
        other => {
            let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
            apply_edit_action(other, state, cols, guard.terminal_rows());
        }
    }
    Ok(())
}

fn submit_mid_turn_input(
    state: &mut AppState,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    dismiss_autocomplete(state);
    let Some(text) = state.input_editor.submit()? else {
        if state.in_flight_input.has_pending_steers() {
            state.in_flight_input.request_interrupt_submit();
            *cancel_requested = true;
            cancel.cancel();
        }
        return Ok(());
    };

    match state.in_flight_input.mode() {
        InFlightSubmitMode::Steer => match active_input_tx.send_steer(text.clone()) {
            Ok(id) => state.in_flight_input.push_pending_steer(id, text),
            Err(ActiveInputError::Closed) => state.in_flight_input.queue_followup(text),
            Err(ActiveInputError::Empty) => {}
        },
        InFlightSubmitMode::Queue => state.in_flight_input.queue_followup(text),
    }
    Ok(())
}

pub(super) fn handle_active_input_delivery(
    delivery: &ActiveInputDelivery,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    state.in_flight_input.mark_steer_delivered(delivery.id);
    finish_thinking_block(state, guard, renderer)?;
    flush_markdown(state, guard, renderer)?;
    write_user_message(&delivery.content, state, guard)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use norn::provider::agent_event::AgentStreamRetry;
    use norn::provider::openai::response_stream_event::ResponseStreamEvent;
    use norn::provider::request::ToolCallKind;
    use norn::provider::response_audio::ResponseAudioEvent;

    /// An `AppState` over a registry holding a confirmed root agent —
    /// enough for the redraw-decision tests, which only read `root_id`.
    fn state_with_root() -> AppState {
        let registry = norn::agent::registry::AgentRegistry::shared();
        let guard = norn::agent::registry::AgentRegistry::reserve(
            &registry,
            "/root".to_string(),
            "lead".to_string(),
            "claude".to_string(),
            None,
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 5,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            None,
        )
        .unwrap();
        let root_id = guard.id();
        guard.confirm().unwrap();
        AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            registry,
            root_id,
            crate::render::fixed_panel::StatusBar::default(),
        )
    }

    /// C6: the retry marker paints the retry status row, so the panel MUST
    /// repaint for it. Pre-C6 the arm returned `false` (the event was a
    /// deliberate no-op) and the wait was invisible.
    #[test]
    fn stream_retry_forces_panel_redraw() {
        let state = state_with_root();
        let event = AgentEvent {
            agent_id: state.tab_state.root_id(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::StreamRetry(AgentStreamRetry {
                attempt: 3,
                max_attempts: Some(5),
                delay_ms: 8_000,
                error_class: "server_error".to_string(),
            }),
        };

        assert!(
            agent_event_needs_panel_redraw(&state, &event),
            "a retry wait must repaint the panel that shows it"
        );
    }

    /// A child agent's retry paints its status row, so it repaints too.
    #[test]
    fn child_stream_retry_forces_panel_redraw() {
        let state = state_with_root();
        let event = AgentEvent {
            agent_id: uuid::Uuid::from_u128(77),
            agent_role: Arc::from("spawn/worker"),
            event: AgentEventKind::StreamRetry(AgentStreamRetry {
                attempt: 2,
                max_attempts: None,
                delay_ms: 1_000,
                error_class: "timeout".to_string(),
            }),
        };

        assert!(agent_event_needs_panel_redraw(&state, &event));
    }

    #[test]
    fn root_text_deltas_do_not_force_panel_redraw() {
        let event = ProviderEvent::TextDelta {
            text: "hello".to_string(),
        };

        assert!(!root_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn refusal_complete_and_raw_event_do_not_force_redundant_panel_redraw()
    -> Result<(), Box<dyn std::error::Error>> {
        let refusal_complete = ProviderEvent::RefusalComplete {
            item_id: "msg_refusal".to_owned(),
            output_index: 0,
            content_index: 0,
            refusal: "request declined".to_owned(),
        };
        let raw = ProviderEvent::ResponseStreamEvent {
            event: Box::new(ResponseStreamEvent::from_raw(serde_json::json!({
                "type": "response.output_text.delta",
                "sequence_number": 1
            }))?),
        };

        assert!(!root_provider_event_needs_panel_redraw(&refusal_complete));
        assert!(!root_provider_event_needs_panel_redraw(&raw));
        Ok(())
    }

    #[test]
    fn actionable_audio_projection_does_not_force_panel_redraw()
    -> Result<(), Box<dyn std::error::Error>> {
        let stream_event = ResponseStreamEvent::from_raw(serde_json::json!({
            "type": "response.audio.done",
            "sequence_number": 2
        }))?;
        let event = ResponseAudioEvent::from_stream_event(&stream_event)?
            .ok_or_else(|| std::io::Error::other("audio event had no typed projection"))?;
        let audio = ProviderEvent::ResponseAudioFrame {
            stream_event: Box::new(stream_event),
            event,
        };

        assert!(!root_provider_event_needs_panel_redraw(&audio));
        assert!(!child_provider_event_needs_panel_redraw(&audio));
        Ok(())
    }

    #[test]
    fn root_tool_completion_forces_panel_redraw() {
        let event = ProviderEvent::ToolCallComplete {
            call_id: "call_1".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(root_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn child_nameless_tool_delta_does_not_force_panel_redraw() {
        let event = ProviderEvent::ToolCallDelta {
            item_id: "item_1".to_string(),
            call_id: None,
            name: None,
            arguments_delta: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(!child_provider_event_needs_panel_redraw(&event));
    }

    #[test]
    fn child_named_tool_delta_forces_panel_redraw() {
        let event = ProviderEvent::ToolCallDelta {
            item_id: "item_1".to_string(),
            call_id: None,
            name: Some("read".to_string()),
            arguments_delta: "{}".to_string(),
            kind: ToolCallKind::Function,
        };

        assert!(child_provider_event_needs_panel_redraw(&event));
    }
}
