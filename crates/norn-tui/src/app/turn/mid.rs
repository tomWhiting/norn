//! Mid-turn handling: terminal events, keystrokes, and steered/queued input
//! delivered while an agent step is in flight.

use termina::Event;
use termina::event::{KeyEvent, KeyEventKind};
use tokio_util::sync::CancellationToken;

use norn::agent_loop::{ActiveInputDelivery, ActiveInputError, ActiveInputSender};
use norn::provider::agent_event::AgentEvent;

use crate::TuiError;
use crate::input::keybindings::{InputAction, map_key_event};
use crate::terminal::setup::TerminalGuard;

use crate::app::active_input::InFlightSubmitMode;
use crate::app::autocomplete::{
    PopupKeyOutcome, dismiss as dismiss_autocomplete, handle_popup_key,
};
use crate::app::dispatch::handle_agent_event;
use crate::app::edit::apply_edit_action;
use crate::app::event_loop::{insert_paste_text, sync_input_for_current_geometry};
use crate::app::render::redraw_all;
use crate::app::state::AppState;

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
            guard.handle_resize(size.cols, size.rows);
            state.screen.allow_body_load = false;
            sync_input_for_current_geometry(state, guard)?;
            redraw_all(state, guard)?;
        }
        Event::Mouse(event) => {
            if crate::app::view_actions::mouse(event, state) {
                redraw_all(state, guard)?;
            }
        }
        Event::Paste(text) => {
            crate::app::view_actions::pin_visible(state)?;
            insert_paste_text(state, &text)?;
            sync_input_for_current_geometry(state, guard)?;
            redraw_all(state, guard)?;
        }
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            handle_mid_turn_key(key, state, guard, active_input_tx, cancel, cancel_requested)?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn handle_mid_turn_agent_event(
    state: &mut AppState,
    event: AgentEvent,
) -> Result<(), TuiError> {
    handle_agent_event(state, event)?;
    state.screen.allow_body_load = true;
    Ok(())
}

fn handle_mid_turn_key(
    key: KeyEvent,
    state: &mut AppState,
    guard: &mut TerminalGuard,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    let cols = guard.terminal_columns();
    if state.autocomplete.is_some()
        && key.kind == KeyEventKind::Press
        && matches!(
            handle_popup_key(key, state, cols, guard.terminal_rows())?,
            PopupKeyOutcome::Consumed
        )
    {
        return Ok(());
    }

    if crate::app::view_actions::key(key, state) {
        redraw_all(state, guard)?;
        return Ok(());
    }
    let popup_open = state.autocomplete.is_some();
    if let Some(action) = map_key_event(key, state.composer_send_key, popup_open) {
        handle_mid_turn_action(
            action,
            state,
            guard,
            active_input_tx,
            cancel,
            cancel_requested,
        )?;
        sync_input_for_current_geometry(state, guard)?;
        redraw_all(state, guard)?;
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
            let cols = guard.terminal_columns();
            let result = apply_edit_action(other, state, cols, guard.terminal_rows())?;
            crate::app::composer_effects::finish(state, guard, result)?;
        }
    }
    crate::app::frontend_preferences::edited(state)?;
    Ok(())
}

fn submit_mid_turn_input(
    state: &mut AppState,
    active_input_tx: &ActiveInputSender,
    cancel: &CancellationToken,
    cancel_requested: &mut bool,
) -> Result<(), TuiError> {
    dismiss_autocomplete(state);
    let Some(snapshot) = crate::app::composer_submission::prepare(state)? else {
        if state.pending_composer_submission.is_none() && state.in_flight_input.has_pending_steers()
        {
            state.in_flight_input.request_interrupt_submit();
            *cancel_requested = true;
            cancel.cancel();
        }
        return Ok(());
    };
    let text = snapshot.text().to_owned();
    if crate::app::view_actions::is_view(&text) {
        let (_, arguments) =
            crate::app::slash_catalog::split_first_word(text.trim().trim_start_matches('/'));
        match crate::app::view_actions::command(arguments, state)? {
            crate::app::slash::LocalCommandOutcome::Accepted => {
                crate::app::composer_submission::accepted_local(state, &snapshot)?;
            }
            crate::app::slash::LocalCommandOutcome::Rejected => {}
            crate::app::slash::LocalCommandOutcome::AcceptedWithError(error) => {
                return Err(crate::app::composer_submission::accepted_with_error(
                    state, &snapshot, error,
                ));
            }
        }
        return Ok(());
    }
    let accepted = match state.in_flight_input.mode() {
        InFlightSubmitMode::Steer => match active_input_tx.send_steer(text.clone()) {
            Ok(id) => {
                state.in_flight_input.push_pending_steer(id, text);
                true
            }
            Err(ActiveInputError::Closed) => {
                state.in_flight_input.queue_followup(text);
                true
            }
            Err(ActiveInputError::Empty) => false,
        },
        InFlightSubmitMode::Queue => {
            state.in_flight_input.queue_followup(text);
            true
        }
    };
    if accepted {
        crate::app::composer_submission::accepted_local(state, &snapshot)?;
    }

    Ok(())
}

pub(super) fn handle_active_input_delivery(
    delivery: &ActiveInputDelivery,
    state: &mut AppState,
    store: &std::sync::Arc<norn::session::store::EventStore>,
) -> Result<(), TuiError> {
    state.in_flight_input.mark_steer_delivered(delivery.id);
    let item = crate::app::notices::input(
        state,
        &format!("You · steer {} delivered", delivery.id),
        &delivery.content,
    )?;
    state
        .transcript
        .read_delivered_input(store, item, delivery.event_id.clone());
    state.screen.allow_body_load = true;
    Ok(())
}
