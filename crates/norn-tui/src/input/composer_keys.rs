//! Terminal key translation into the shared editor; Norn consumes host actions first.

use iridium_editor::{KeyCode as EditorCode, KeyEvent as EditorKey};
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

/// Preserve delivered modifiers and repeat identity; releases never edit.
pub(crate) fn to_kernel_key(event: KeyEvent) -> Option<EditorKey> {
    if event.kind == KeyEventKind::Release || event.modifiers.contains(Modifiers::HYPER) {
        return None;
    }
    let mut key = match iridium_tui::input::from_termina(termina::Event::Key(event)) {
        Ok(iridium_tui::input::TerminalEvent::Input(iridium_tui::input::TerminalInput::Key(
            key,
        ))) => key,
        Ok(_) => return None,
        Err(error) => {
            tracing::debug!(%error, "terminal key has no composer translation");
            return None;
        }
    };
    // Preserve Norn's Mac Command-letter editing shortcuts after host actions.
    if event.modifiers.contains(Modifiers::SUPER) && matches!(key.key, EditorCode::Char(_)) {
        key.modifiers.ctrl = true;
        key.modifiers.meta = false;
    }
    // terminput-termina 0.3.1 drops META and maps BackTab to Tab without Shift.
    // Restore these delivered facts after shared translation and Command mapping;
    // a separately delivered META remains distinct from Command compatibility.
    key.modifiers.meta |= event.modifiers.contains(Modifiers::META);
    key.modifiers.shift |= event.code == KeyCode::BackTab;
    Some(key)
}

/// The existing Mac movement bindings use kernel verbs, including selection.
pub(crate) fn motion_command(event: KeyEvent) -> Option<&'static str> {
    let shift = event.modifiers.contains(Modifiers::SHIFT);
    let meta = event.modifiers.contains(Modifiers::SUPER);
    let word = event.modifiers.contains(Modifiers::ALT);
    match (event.code, meta, word, shift) {
        (KeyCode::Left, true, _, false) => Some("cursor.lineStart"),
        (KeyCode::Left, true, _, true) => Some("cursor.lineStartSelect"),
        (KeyCode::Right, true, _, false) => Some("cursor.lineEnd"),
        (KeyCode::Right, true, _, true) => Some("cursor.lineEndSelect"),
        (KeyCode::Up, true, _, false) => Some("cursor.documentStart"),
        (KeyCode::Up, true, _, true) => Some("cursor.documentStartSelect"),
        (KeyCode::Down, true, _, false) => Some("cursor.documentEnd"),
        (KeyCode::Down, true, _, true) => Some("cursor.documentEndSelect"),
        (KeyCode::Left, false, true, false) => Some("cursor.wordLeft"),
        (KeyCode::Left, false, true, true) => Some("cursor.wordLeftSelect"),
        (KeyCode::Right, false, true, false) => Some("cursor.wordRight"),
        (KeyCode::Right, false, true, true) => Some("cursor.wordRightSelect"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "composer_keys_tests.rs"]
mod tests;
