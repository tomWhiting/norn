//! Input-action dispatch — applies an [`InputAction`] to [`AppState`]
//! without triggering a repaint.
//!
//! Used by both the outer-loop `handle_action` path and the mid-turn
//! input handler so keystrokes update the editor while the agent runs.
//! [`InputAction::Submit`] and [`InputAction::Exit`] are no-ops here —
//! those require outer-loop dispatch (slash commands, `run_turn`, exit
//! sequencing) that only the event loop owns.

use crate::input::keybindings::InputAction;

use super::autocomplete::{dismiss as dismiss_autocomplete, refresh_autocomplete};
use super::render::sync_input_area;
use super::state::AppState;

/// Apply an editing action to the input state without triggering a
/// repaint. Used by both the outer-loop handler and the mid-turn input
/// handler so keystrokes update the editor while the agent runs.
///
/// [`InputAction::Submit`] and [`InputAction::Exit`] are no-ops here —
/// those require outer-loop dispatch (slash commands, `run_turn`, exit
/// sequencing) that only the outer loop owns.
pub(super) fn apply_edit_action(
    action: InputAction,
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) {
    match action {
        InputAction::Submit | InputAction::Exit => {}
        InputAction::InsertChar(ch) => {
            state.input_editor.insert_char(ch);
            refresh_popup(state);
        }
        InputAction::InsertNewline => {
            state.input_editor.insert_newline();
            refresh_popup(state);
        }
        InputAction::Backspace => {
            state.input_editor.backspace();
            refresh_popup(state);
        }
        InputAction::Delete => {
            state.input_editor.delete();
            refresh_popup(state);
        }
        InputAction::CursorLeft => {
            state.input_editor.cursor_left();
            refresh_popup(state);
        }
        InputAction::CursorRight => {
            state.input_editor.cursor_right();
            refresh_popup(state);
        }
        InputAction::CursorUp => {
            if state.input_editor.cursor_on_first_visual_line(cols) {
                state.input_editor.history_prev();
                dismiss_autocomplete(state);
            } else {
                state.input_editor.visual_cursor_up(cols);
                refresh_popup(state);
            }
        }
        InputAction::CursorDown => {
            if state.input_editor.cursor_on_last_visual_line(cols) {
                state.input_editor.history_next();
                dismiss_autocomplete(state);
            } else {
                state.input_editor.visual_cursor_down(cols);
                refresh_popup(state);
            }
        }
        InputAction::WordLeft => {
            state.input_editor.word_left();
            refresh_popup(state);
        }
        InputAction::WordRight => {
            state.input_editor.word_right();
            refresh_popup(state);
        }
        InputAction::LineStart => {
            state.input_editor.line_start();
            refresh_popup(state);
        }
        InputAction::LineEnd => {
            state.input_editor.line_end();
            refresh_popup(state);
        }
        InputAction::BufferStart => {
            state.input_editor.buffer_start();
            refresh_popup(state);
        }
        InputAction::BufferEnd => {
            state.input_editor.buffer_end();
            refresh_popup(state);
        }
        InputAction::DeleteWordBack => {
            state.input_editor.delete_word_back();
            refresh_popup(state);
        }
        InputAction::DeleteWordForward => {
            state.input_editor.delete_word_forward();
            refresh_popup(state);
        }
        InputAction::DeleteToLineStart => {
            state.input_editor.delete_to_line_start();
            refresh_popup(state);
        }
        InputAction::DeleteToLineEnd => {
            state.input_editor.delete_to_line_end();
            refresh_popup(state);
        }
        InputAction::ClearInput => {
            state.input_editor.clear();
            dismiss_autocomplete(state);
        }
        InputAction::ToggleVerbosity => state.verbosity = state.verbosity.toggle(),
        InputAction::ToggleThinking => state.display_toggles.toggle(),
    }
    let input_rows = sync_input_area(&mut state.input_editor, cols, terminal_rows);
    state.fixed_panel.set_input_area(input_rows);
}

/// Recompute the autocomplete popup against the editor's current text.
fn refresh_popup(state: &mut AppState) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    refresh_autocomplete(state, &cwd);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::history::InputHistory;
    use crate::input::wrap;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

    fn fresh_state() -> Result<AppState, Box<dyn std::error::Error>> {
        let registry: Arc<RwLock<AgentRegistry>> = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
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
        )?;
        let root_id = guard.id();
        guard.confirm()?;
        Ok(AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            root_id,
            StatusBar::default(),
        ))
    }

    fn type_action_text(state: &mut AppState, text: &str, cols: u16, rows: u16) {
        for ch in text.chars() {
            if ch == '\n' {
                apply_edit_action(InputAction::InsertNewline, state, cols, rows);
            } else {
                apply_edit_action(InputAction::InsertChar(ch), state, cols, rows);
            }
        }
    }

    #[test]
    fn panel_size_tracks_visual_height() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = "a".repeat(60);
        type_action_text(&mut state, &text, 20, 80);
        assert_eq!(state.input_editor.visual_height(20), 3);
        assert_eq!(state.fixed_panel.total_height(), 5);
        Ok(())
    }

    #[test]
    fn input_area_is_capped_and_cursor_visible() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = (0..50).map(|_| "x").collect::<Vec<_>>().join("\n");
        type_action_text(&mut state, &text, 80, 24);
        assert_eq!(state.fixed_panel.total_height(), 14);
        let layout = wrap::layout(
            state.input_editor.lines(),
            state.input_editor.cursor_position().0,
            state.input_editor.cursor_position().1,
            80,
        );
        let cursor_row = u16::try_from(layout.cursor.visual_row)?;
        let viewport_top = state.input_editor.viewport_top();
        assert!(cursor_row >= viewport_top);
        assert!(cursor_row < viewport_top + 12);
        Ok(())
    }

    #[test]
    fn visual_navigation_updates_viewport_with_cap() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = (0..50).map(|_| "x").collect::<Vec<_>>().join("\n");
        type_action_text(&mut state, &text, 80, 24);
        let bottom_viewport = state.input_editor.viewport_top();
        apply_edit_action(InputAction::CursorUp, &mut state, 80, 24);
        assert!(state.input_editor.viewport_top() <= bottom_viewport);
        for _ in 0..20 {
            apply_edit_action(InputAction::CursorUp, &mut state, 80, 24);
        }
        assert!(state.input_editor.viewport_top() < bottom_viewport);
        apply_edit_action(InputAction::BufferStart, &mut state, 80, 24);
        assert_eq!(state.input_editor.viewport_top(), 0);
        for _ in 0..12 {
            apply_edit_action(InputAction::CursorDown, &mut state, 80, 24);
        }
        assert!(state.input_editor.viewport_top() > 0);
        Ok(())
    }

    #[test]
    fn resize_narrower_grows_panel_visual_height() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = "a".repeat(60);
        type_action_text(&mut state, &text, 80, 80);
        assert_eq!(state.fixed_panel.total_height(), 3);
        let input_rows = sync_input_area(&mut state.input_editor, 20, 80);
        state.fixed_panel.set_input_area(input_rows);
        assert_eq!(state.input_editor.visual_height(20), 3);
        assert_eq!(state.fixed_panel.total_height(), 5);
        Ok(())
    }
}
