//! Composer clipboard and host effects through the existing terminal writer.

use super::state::AppState;
use crate::TuiError;
use crate::terminal::setup::TerminalGuard;

/// Host effects returned by the editor use Norn's one terminal writer.
pub(super) fn finish(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    result: iridium_editor::EditorKeyResult,
) -> Result<(), TuiError> {
    use crate::input::composer_clipboard::{
        ComposerClipboardPreparation, prepare_composer_clipboard,
    };
    let message = match result {
        iridium_editor::EditorKeyResult::None => return Ok(()),
        iridium_editor::EditorKeyResult::Clipboard(operation) => {
            if route_pane_clipboard(state, &operation) {
                return Ok(());
            }
            let snapshot = state.input_editor.snapshot()?;
            match prepare_composer_clipboard(
                &state.input_editor,
                snapshot,
                operation,
                state.transcript.config.clipboard,
            ) {
                Ok(ComposerClipboardPreparation::Ready(prepared)) => {
                    match prepared.send(&mut state.input_editor, guard.terminal_mut()) {
                        Ok(sent) => format!(
                            "Sent {} bytes to the terminal clipboard transport; acceptance unconfirmed{}",
                            sent.original_bytes,
                            if sent.cut_applied {
                                "; cut applied"
                            } else {
                                ""
                            }
                        ),
                        Err(error) => error.to_string(),
                    }
                }
                Ok(ComposerClipboardPreparation::Unavailable(reason)) => format!(
                    "Clipboard unavailable ({reason:?}); permit OSC 52 with /view clipboard osc52, or use your terminal's paste"
                ),
                Ok(ComposerClipboardPreparation::SanitizedCut) => {
                    "Cut refused because clipboard escaping would change the text; draft retained"
                        .to_owned()
                }
                Err(error) => error.to_string(),
            }
        }
        iridium_editor::EditorKeyResult::Search(action) => format!(
            "Composer search action {action:?} has no visible search panel; use /view search for the conversation"
        ),
        iridium_editor::EditorKeyResult::HostCommand { command, .. } => {
            format!("Editor command {command} requires a workspace control")
        }
    };
    state.screen.conversation.feedback = Some(message);
    state.screen.dirty = true;
    Ok(())
}

/// Kernel keymaps may request copy/cut while a read-only pane owns focus.
/// Route that request to its selected display bytes, never the composer's clipboard payload.
fn route_pane_clipboard(
    state: &mut AppState,
    operation: &iridium_editor::ClipboardOperation,
) -> bool {
    if state.screen.focus.requested() == super::focus::Focus::Composer
        || matches!(operation, iridium_editor::ClipboardOperation::Paste)
    {
        return false;
    }
    state.screen.conversation.request_copy = true;
    state.screen.dirty = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_copy_never_consumes_composer_payload_or_changes_draft()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(uuid::Uuid::new_v4()),
            crate::render::fixed_panel::StatusBar::default(),
        );
        crate::app::render::sync_input_area(&mut state, 120, 24)?;
        state.input_editor.paste_cells("private composer draft")?;
        let draft = state.input_editor.snapshot()?;
        state.screen.focus.focus(
            super::super::focus::Focus::Conversation,
            state.screen.availability(),
        )?;
        assert!(route_pane_clipboard(
            &mut state,
            &iridium_editor::ClipboardOperation::Copy("private composer draft".to_owned())
        ));
        assert!(state.screen.conversation.request_copy);
        state.input_editor.validate_snapshot(&draft)?;
        state.screen.conversation.request_copy = false;
        assert!(!route_pane_clipboard(
            &mut state,
            &iridium_editor::ClipboardOperation::Paste
        ));
        assert!(!state.screen.conversation.request_copy);
        state.screen.focus.focus(
            super::super::focus::Focus::Composer,
            state.screen.availability(),
        )?;
        assert!(!route_pane_clipboard(
            &mut state,
            &iridium_editor::ClipboardOperation::Copy("draft".to_owned())
        ));
        state.input_editor.validate_snapshot(&draft)?;
        Ok(())
    }
}
