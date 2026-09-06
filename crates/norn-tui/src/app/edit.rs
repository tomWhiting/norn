//! Shared idle/active editing dispatch through the one Iridium composer.
//! Host controls remain local; clipboard and host results return to the event-loop owner.

use iridium_editor::cell_layout::ScreenRow;
use iridium_editor::editor::CellInputOptions;
use iridium_editor::{CommandArgs, EditorKeyResult, KeyCode, KeyEvent};
use termina::event::{KeyCode as TerminalCode, Modifiers};

use crate::error::TuiError;
use crate::input::composer_keys::{motion_command, to_kernel_key};
use crate::input::keybindings::InputAction;

use super::autocomplete::{dismiss as dismiss_autocomplete, refresh_autocomplete};
use super::render::sync_input_area;
use super::state::AppState;

/// Apply one editing action, returning clipboard/host effects to the sole writer.
/// Submit, exit and steer/queue require event-loop admission and are not edited here.
pub(super) fn apply_edit_action(
    action: InputAction,
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) -> Result<EditorKeyResult, TuiError> {
    sync_input_area(state, cols, terminal_rows)?;
    let options = state.composer_geometry.input_options();
    let mut refresh = true;
    let result = match action {
        InputAction::Submit | InputAction::Exit | InputAction::ToggleInFlightSubmitMode => {
            refresh = false;
            EditorKeyResult::None
        }
        InputAction::InsertChar(character) => state
            .input_editor
            .handle_cell_key(&KeyEvent::simple(KeyCode::Char(character)), options)?,
        InputAction::InsertNewline => run(state, "edit.insertNewline", options)?,
        InputAction::Backspace => run(state, "edit.deleteBackward", options)?,
        InputAction::Delete => run(state, "edit.deleteForward", options)?,
        InputAction::CursorLeft => run(state, "cursor.charLeft", options)?,
        InputAction::CursorRight => run(state, "cursor.charRight", options)?,
        InputAction::CursorUp | InputAction::CursorDown => {
            let forward = action == InputAction::CursorDown;
            refresh = !recall_at_edge(state, forward)?;
            if refresh {
                vertical(state, forward, options)?
            } else {
                EditorKeyResult::None
            }
        }
        InputAction::WordLeft => run(state, "cursor.wordLeft", options)?,
        InputAction::WordRight => run(state, "cursor.wordRight", options)?,
        InputAction::LineStart => run(state, "cursor.lineStart", options)?,
        InputAction::LineEnd => run(state, "cursor.lineEnd", options)?,
        InputAction::BufferStart => run(state, "cursor.documentStart", options)?,
        InputAction::BufferEnd => run(state, "cursor.documentEnd", options)?,
        InputAction::DeleteWordBack => run(state, "edit.deleteWordBackward", options)?,
        InputAction::DeleteWordForward => run(state, "edit.deleteWordForward", options)?,
        InputAction::DeleteToLineStart => run(state, "edit.deleteToLineStart", options)?,
        InputAction::DeleteToLineEnd => run(state, "edit.deleteToLineEnd", options)?,
        InputAction::KernelKey(event) => {
            let Some(key) = to_kernel_key(event) else {
                return Ok(EditorKeyResult::None);
            };
            let bare_vertical = event.modifiers == Modifiers::NONE
                && matches!(event.code, TerminalCode::Up | TerminalCode::Down);
            if bare_vertical && recall_at_edge(state, event.code == TerminalCode::Down)? {
                refresh = false;
                EditorKeyResult::None
            } else if let Some(command) = motion_command(event) {
                run(state, command, options)?
            } else {
                state.input_editor.handle_cell_key(&key, options)?
            }
        }
        InputAction::ClearInput => {
            state.input_editor.clear()?;
            dismiss_autocomplete(state);
            refresh = false;
            EditorKeyResult::None
        }
        InputAction::ToggleVerbosity => {
            state.verbosity = state.verbosity.toggle();
            state.transcript.config.expanded_tools = !state.transcript.config.expanded_tools;
            state.screen.allow_body_load = true;
            refresh = false;
            EditorKeyResult::None
        }
        InputAction::ToggleThinking => {
            state.display_toggles.toggle();
            refresh = false;
            EditorKeyResult::None
        }
    };
    if refresh {
        refresh_popup(state)?;
    }
    sync_input_area(state, cols, terminal_rows)?;
    Ok(result)
}

fn run(
    state: &mut AppState,
    command: &str,
    options: CellInputOptions,
) -> Result<EditorKeyResult, TuiError> {
    Ok(state
        .input_editor
        .run_cell_command(command, CommandArgs::NONE, options)?)
}

fn vertical(
    state: &mut AppState,
    forward: bool,
    options: CellInputOptions,
) -> Result<EditorKeyResult, TuiError> {
    run(
        state,
        if forward {
            "cursor.lineDown"
        } else {
            "cursor.lineUp"
        },
        options,
    )
}

/// Only a bare single caret at an actual visual boundary enters submission recall.
fn recall_at_edge(state: &mut AppState, forward: bool) -> Result<bool, TuiError> {
    let cursor = &state.input_editor.kernel().state().cursor;
    if cursor.cursor_count() != 1 || !cursor.primary.is_collapsed() {
        return Ok(false);
    }
    let row = state.composer_geometry.cursor_row();
    let at_edge = if forward {
        row.is_some_and(|row| row.0.checked_add(1) == Some(state.composer_geometry.total_rows()))
    } else {
        row == Some(ScreenRow(0))
    };
    if !at_edge {
        return Ok(false);
    }
    if forward {
        state.input_editor.history_next()?;
    } else {
        state.input_editor.history_prev()?;
    }
    dismiss_autocomplete(state);
    Ok(true)
}

#[derive(Debug, thiserror::Error)]
#[error("composer autocomplete current directory: {source}")]
struct CurrentDirectoryError {
    #[source]
    source: std::io::Error,
}

/// Resolve the real workspace or surface the original lookup error.
fn refresh_popup(state: &mut AppState) -> Result<(), TuiError> {
    let cwd = std::env::current_dir().map_err(|source| TuiError::ViewInteraction {
        source: Box::new(CurrentDirectoryError { source }),
    })?;
    refresh_autocomplete(state, &cwd)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::history::InputHistory;
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
            crate::app::state::test_view_source(root_id),
            StatusBar::default(),
        ))
    }

    fn type_action_text(
        state: &mut AppState,
        text: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), TuiError> {
        for ch in text.chars() {
            if ch == '\n' {
                apply_edit_action(InputAction::InsertNewline, state, cols, rows)?;
            } else {
                apply_edit_action(InputAction::InsertChar(ch), state, cols, rows)?;
            }
        }
        Ok(())
    }

    #[test]
    fn panel_size_tracks_visual_height() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = "a".repeat(60);
        type_action_text(&mut state, &text, 20, 80)?;
        assert_eq!(state.composer_geometry.total_rows(), 3);
        assert_eq!(state.fixed_panel.total_height(), 6);
        Ok(())
    }

    #[test]
    fn input_area_is_capped_and_cursor_visible() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = (0..50).map(|_| "x").collect::<Vec<_>>().join("\n");
        type_action_text(&mut state, &text, 80, 24)?;
        // The restored three-row framing leaves ten visible input rows at 24 rows.
        assert_eq!(state.fixed_panel.total_height(), 13);
        assert_eq!(state.composer_geometry.input_options().visible_rows, 10);
        let cursor_row = state
            .composer_geometry
            .cursor_row()
            .ok_or("composer cursor row absent")?
            .0;
        let viewport_top = state.composer_geometry.first_row().0;
        assert!(cursor_row >= viewport_top);
        assert!(cursor_row < viewport_top + 10);
        Ok(())
    }

    #[test]
    fn visual_navigation_updates_viewport_with_cap() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = (0..50).map(|_| "x").collect::<Vec<_>>().join("\n");
        type_action_text(&mut state, &text, 80, 24)?;
        let bottom_viewport = state.composer_geometry.first_row().0;
        apply_edit_action(InputAction::CursorUp, &mut state, 80, 24)?;
        assert!(state.composer_geometry.first_row().0 <= bottom_viewport);
        for _ in 0..20 {
            apply_edit_action(InputAction::CursorUp, &mut state, 80, 24)?;
        }
        assert!(state.composer_geometry.first_row().0 < bottom_viewport);
        apply_edit_action(InputAction::BufferStart, &mut state, 80, 24)?;
        assert_eq!(state.composer_geometry.first_row().0, 0);
        for _ in 0..12 {
            apply_edit_action(InputAction::CursorDown, &mut state, 80, 24)?;
        }
        assert!(state.composer_geometry.first_row().0 > 0);
        Ok(())
    }

    #[test]
    fn resize_narrower_grows_panel_visual_height() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let text = "a".repeat(60);
        type_action_text(&mut state, &text, 80, 80)?;
        assert_eq!(state.fixed_panel.total_height(), 4);
        let input_rows = sync_input_area(&mut state, 20, 80)?;
        state.fixed_panel.set_input_area(input_rows);
        assert_eq!(state.composer_geometry.total_rows(), 3);
        assert_eq!(state.fixed_panel.total_height(), 6);
        Ok(())
    }

    #[test]
    fn kernel_shift_selection_and_undo_preserve_original_graphemes()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_action_text(&mut state, "e\u{301}🙂", 80, 24)?;
        let before = state.input_editor.text();
        let mut config = state.input_editor.kernel().get_config().clone();
        config.undo_group_timeout_ms = 0;
        state.input_editor.set_config(config);
        apply_edit_action(
            InputAction::KernelKey(termina::event::KeyEvent::new(
                TerminalCode::Left,
                Modifiers::SHIFT,
            )),
            &mut state,
            80,
            24,
        )?;
        assert_eq!(
            state.input_editor.kernel().state().cursor.primary.range(),
            iridium_editor::Range::new(
                iridium_editor::Position::new(0, 2),
                iridium_editor::Position::new(0, 3)
            )
        );
        apply_edit_action(InputAction::InsertChar('x'), &mut state, 80, 24)?;
        assert_eq!(state.input_editor.text(), "e\u{301}x");
        let options = state.composer_geometry.input_options();
        state
            .input_editor
            .run_cell_command("history.undo", CommandArgs::NONE, options)?;
        assert_eq!(state.input_editor.text(), before);
        assert_eq!(
            state.input_editor.kernel().state().cursor.primary.range(),
            iridium_editor::Range::new(
                iridium_editor::Position::new(0, 2),
                iridium_editor::Position::new(0, 3)
            )
        );
        Ok(())
    }

    #[test]
    fn bare_visual_edge_recalls_but_shifted_motion_and_selection_do_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_action_text(&mut state, "previous", 80, 24)?;
        let accepted = state.input_editor.snapshot()?;
        state.input_editor.record_accepted(&accepted)?;
        state.input_editor.clear()?;
        type_action_text(&mut state, "draft", 80, 24)?;
        apply_edit_action(
            InputAction::KernelKey(termina::event::KeyEvent::new(
                TerminalCode::Up,
                Modifiers::SUPER | Modifiers::SHIFT,
            )),
            &mut state,
            80,
            24,
        )?;
        assert_eq!(state.input_editor.text(), "draft");
        assert!(
            !state
                .input_editor
                .kernel()
                .state()
                .cursor
                .primary
                .is_collapsed()
        );
        apply_edit_action(InputAction::CursorUp, &mut state, 80, 24)?;
        assert_eq!(state.input_editor.text(), "draft");
        let options = state.composer_geometry.input_options();
        state.input_editor.run_cell_command(
            "selection.collapseToPrimary",
            CommandArgs::NONE,
            options,
        )?;
        apply_edit_action(InputAction::CursorUp, &mut state, 80, 24)?;
        assert_eq!(state.input_editor.text(), "previous");
        apply_edit_action(InputAction::CursorDown, &mut state, 80, 24)?;
        assert_eq!(state.input_editor.text(), "draft");
        Ok(())
    }

    #[test]
    fn clipboard_request_returns_to_owner_without_mutating_input()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_action_text(&mut state, "clipboard witness", 80, 24)?;
        let snapshot = state.input_editor.snapshot()?;
        let result = apply_edit_action(
            InputAction::KernelKey(termina::event::KeyEvent::new(
                TerminalCode::Char('v'),
                Modifiers::CONTROL,
            )),
            &mut state,
            80,
            24,
        )?;
        assert!(matches!(
            result,
            EditorKeyResult::Clipboard(iridium_editor::ClipboardOperation::Paste)
        ));
        state.input_editor.validate_snapshot(&snapshot)?;
        assert_eq!(state.input_editor.text(), "clipboard witness");
        Ok(())
    }
}
