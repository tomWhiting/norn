//! Focus-aware view key dispatch after popup ownership and before composer admission.

use super::{browse, expand, pin_visible, prepare_command, resize_split, select_row};
use crate::TuiError;
use crate::app::focus::{Focus, FocusDirection};
use crate::app::render::interaction;
use crate::app::state::AppState;
use crate::render::layout::UpperPane;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

/// Popup handling runs first; composer input and cancellation retain ownership.
pub(in crate::app) fn key(key: KeyEvent, state: &mut AppState) -> bool {
    match apply_key(key, state) {
        Ok(handled) => handled,
        Err(error) => {
            state.screen.feedback = Some(error.to_string());
            state.screen.dirty = true;
            true
        }
    }
}

fn apply_key(key: KeyEvent, state: &mut AppState) -> Result<bool, TuiError> {
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }
    state.screen.feedback = None;
    let available = state.screen.availability();
    if !available.composer {
        return Ok(false);
    }
    if matches!(
        key.code,
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
    ) && !key.modifiers.contains(Modifiers::CONTROL)
    {
        pin_visible(state)?;
    }
    let focus = state.screen.focus.visible(available).map_err(interaction)?;
    let handled = match key.code {
        KeyCode::Function(3) => {
            prepare_command(state, "/view search ")?;
            true
        }
        KeyCode::Function(4) => {
            state.screen.request_copy = true;
            true
        }
        KeyCode::Function(5) => {
            prepare_command(state, "/view export ")?;
            true
        }
        KeyCode::Function(6) => {
            crate::app::autocomplete::dismiss(state);
            let direction = if key.modifiers.contains(Modifiers::SHIFT) {
                FocusDirection::Backward
            } else {
                FocusDirection::Forward
            };
            state
                .screen
                .focus
                .cycle(direction, available)
                .map_err(interaction)?;
            if state.screen.focus.requested() != Focus::Composer {
                pin_visible(state)?;
            }
            true
        }
        KeyCode::Function(2) => {
            state.screen.changes_open = true;
            state.screen.upper = match state.screen.upper {
                UpperPane::Conversation => UpperPane::Changes,
                UpperPane::Changes => UpperPane::Conversation,
            };
            true
        }
        KeyCode::PageUp => {
            browse(state, true)?;
            true
        }
        KeyCode::PageDown => {
            browse(state, false)?;
            true
        }
        KeyCode::Enter if focus != Focus::Composer => {
            expand(state, None)?;
            true
        }
        KeyCode::Up if focus != Focus::Composer => {
            select_row(state, true)?;
            true
        }
        KeyCode::Down if focus != Focus::Composer => {
            select_row(state, false)?;
            true
        }
        KeyCode::Left if focus == Focus::Divider => {
            resize_split(state, true)?;
            true
        }
        KeyCode::Right if focus == Focus::Divider => {
            resize_split(state, false)?;
            true
        }
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
            if focus != Focus::Composer && !key.modifiers.contains(Modifiers::CONTROL) =>
        {
            // Typing addresses the composer without changing the pinned transcript.
            state
                .screen
                .focus
                .focus(Focus::Composer, available)
                .map_err(interaction)?;
            false
        }
        _ => false,
    };
    if handled {
        crate::app::frontend_preferences::edited(state)?;
        state.screen.dirty = true;
        state.screen.allow_body_load = true;
    }
    Ok(handled)
}
