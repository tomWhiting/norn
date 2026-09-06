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
    state.screen.feedback = Some(message);
    state.screen.dirty = true;
    Ok(())
}
