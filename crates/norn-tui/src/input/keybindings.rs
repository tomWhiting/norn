//! Key event → action mapping.
//!
//! [`map_key_event`] is a pure, stateless translation from a terminal
//! [`KeyEvent`] to an [`InputAction`]. It owns no editor state: the
//! capability-dependent decisions (Shift+Enter vs Alt+Enter for a
//! newline) are resolved here against [`TerminalCaps`], while the
//! state-dependent decisions (Ctrl+C exit vs clear, history vs popup
//! navigation) are left to the editor and event loop that consume the
//! action.
//!
//! On Mac laptops, the Fn key is handled before events reach the
//! terminal: the OS keyboard-driver layer translates Fn+Backspace into a
//! Forward Delete key code. Terminals therefore deliver [`KeyCode::Delete`]
//! rather than a distinct Fn modifier, so this mapper intentionally handles
//! Backspace and Delete as separate key codes instead of looking for Fn.

use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

use crate::terminal::caps::TerminalCaps;

/// An action derived from a terminal key event.
///
/// The editor applies the buffer-mutating variants directly; the event
/// loop interprets [`InputAction::Submit`], [`InputAction::Exit`], and
/// the vertical-cursor variants in the context of the wider TUI state.
///
/// [`InputAction::CursorUp`] / [`InputAction::CursorDown`] fall through
/// to history recall when the cursor is already on the first / last
/// visual line of a wrapped input — the dispatch decision is left to
/// the event loop because only it knows the terminal width and current
/// editor state at action time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Submit the current input buffer to the agent loop.
    Submit,
    /// Insert a newline at the cursor, growing the input area.
    InsertNewline,
    /// Insert a character at the cursor.
    InsertChar(char),
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the character at the cursor.
    Delete,
    /// Move the cursor one column left.
    CursorLeft,
    /// Move the cursor one column right.
    CursorRight,
    /// Move the cursor one visual row up — event loop dispatches to
    /// `visual_cursor_up` in the input area, falls through to
    /// `history_prev` when the cursor is on the first visual line.
    CursorUp,
    /// Move the cursor one visual row down — event loop dispatches to
    /// `visual_cursor_down` in the input area, falls through to
    /// `history_next` when the cursor is on the last visual line.
    CursorDown,
    /// Move the cursor one word left (Option+Left on macOS).
    WordLeft,
    /// Move the cursor one word right (Option+Right on macOS).
    WordRight,
    /// Move the cursor to the start of the current logical line
    /// (Command+Left on macOS; Home or Ctrl+A as fallbacks on non-Kitty
    /// terminals where the SUPER modifier is not delivered).
    LineStart,
    /// Move the cursor to the end of the current logical line
    /// (Command+Right on macOS; End as a fallback). Ctrl+E remains bound
    /// to [`InputAction::ToggleThinking`], so it is not also used here.
    LineEnd,
    /// Move the cursor to the start of the input buffer
    /// (Command+Up on macOS). There is intentionally no non-Kitty
    /// fallback because terminals without Kitty do not deliver Command,
    /// and Ctrl+Up has other common meanings.
    BufferStart,
    /// Move the cursor to the end of the input buffer
    /// (Command+Down on macOS). There is intentionally no non-Kitty
    /// fallback because terminals without Kitty do not deliver Command,
    /// and Ctrl+Down has other common meanings.
    BufferEnd,
    /// Delete the word before the cursor (Option+Backspace on macOS).
    DeleteWordBack,
    /// Delete the word after the cursor (Option+Delete on macOS).
    DeleteWordForward,
    /// Delete from the cursor to the start of the current line
    /// (Command+Backspace on macOS; Ctrl+U as a fallback).
    DeleteToLineStart,
    /// Delete from the cursor to the end of the current line
    /// (Command+Delete on macOS; Ctrl+K as a fallback).
    DeleteToLineEnd,
    /// Clear the input buffer.
    ClearInput,
    /// Signal TUI exit (the editor decides empty-vs-clear on Ctrl+C).
    Exit,
    /// Toggle the global tool-call verbosity (Ctrl+O).
    ToggleVerbosity,
    /// Toggle thinking visibility and secondary structured-output
    /// fields (Ctrl+E).
    ToggleThinking,
    /// Toggle in-flight Enter behavior between steer and queue modes (Ctrl+T).
    ToggleInFlightSubmitMode,
}

/// Translate a terminal key event into an [`InputAction`].
///
/// Returns `None` for events that map to no action: any non-`Press`
/// event kind (so key releases never double-fire), Up/Down while the
/// autocomplete popup owns navigation (`popup_open` is `true`), and any
/// key with no binding.
///
/// Newline discrimination is capability-aware: `Alt+Enter` always
/// inserts a newline, while `Shift+Enter` only does so when the terminal
/// advertises the Kitty keyboard protocol — without it, `Shift+Enter`
/// cannot be reliably distinguished from `Enter` and falls back to
/// submit.
///
/// Modifier precedence for arrow keys mirrors macOS conventions:
/// `SUPER` (Command) is checked first, then `ALT` (Option), then bare.
/// `SUPER`+arrow yields line / buffer movement; `ALT`+arrow yields word
/// movement; bare arrow yields character / visual-line movement. The
/// `SUPER` bit only arrives on Kitty-protocol terminals — `Home` and
/// Ctrl+A are accepted as fallback bindings for line-start, and `End` is
/// accepted as the fallback binding for line-end, so the behaviour is
/// reachable on terminals that swallow Command.
#[must_use]
pub fn map_key_event(
    event: KeyEvent,
    caps: &TerminalCaps,
    popup_open: bool,
) -> Option<InputAction> {
    let action = if event.kind == KeyEventKind::Press {
        let mods = event.modifiers;
        match event.code {
            KeyCode::Enter => Some(map_enter(mods, caps)),
            KeyCode::Char('c') if mods.contains(Modifiers::CONTROL) => Some(InputAction::Exit),
            KeyCode::Char('o' | 'O') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::ToggleVerbosity)
            }
            KeyCode::Char('e' | 'E') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::ToggleThinking)
            }
            KeyCode::Char('t' | 'T') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::ToggleInFlightSubmitMode)
            }
            KeyCode::Char('a' | 'A') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::LineStart)
            }
            KeyCode::Char('u' | 'U') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::DeleteToLineStart)
            }
            KeyCode::Char(ch) if mods == Modifiers::NONE || mods == Modifiers::SHIFT => {
                Some(InputAction::InsertChar(ch))
            }
            KeyCode::Backspace if mods.contains(Modifiers::SUPER) => {
                Some(InputAction::DeleteToLineStart)
            }
            KeyCode::Backspace if mods.contains(Modifiers::ALT) => {
                Some(InputAction::DeleteWordBack)
            }
            KeyCode::Backspace => Some(InputAction::Backspace),
            KeyCode::Delete if mods.contains(Modifiers::SUPER) => {
                Some(InputAction::DeleteToLineEnd)
            }
            KeyCode::Delete if mods.contains(Modifiers::ALT) => {
                Some(InputAction::DeleteWordForward)
            }
            KeyCode::Delete => Some(InputAction::Delete),
            KeyCode::Char('k' | 'K') if mods.contains(Modifiers::CONTROL) => {
                Some(InputAction::DeleteToLineEnd)
            }
            KeyCode::Left if mods.contains(Modifiers::SUPER) => Some(InputAction::LineStart),
            KeyCode::Left if mods.contains(Modifiers::ALT) => Some(InputAction::WordLeft),
            KeyCode::Left => Some(InputAction::CursorLeft),
            KeyCode::Right if mods.contains(Modifiers::SUPER) => Some(InputAction::LineEnd),
            KeyCode::Right if mods.contains(Modifiers::ALT) => Some(InputAction::WordRight),
            KeyCode::Right => Some(InputAction::CursorRight),
            // Popup ownership of vertical navigation takes precedence over
            // every Up/Down binding — the popup uses the keys for candidate
            // navigation even when Command is held.
            KeyCode::Up | KeyCode::Down if popup_open => None,
            KeyCode::Up if mods.contains(Modifiers::SUPER) => Some(InputAction::BufferStart),
            KeyCode::Up => Some(InputAction::CursorUp),
            KeyCode::Down if mods.contains(Modifiers::SUPER) => Some(InputAction::BufferEnd),
            KeyCode::Down => Some(InputAction::CursorDown),
            KeyCode::Home => Some(InputAction::LineStart),
            KeyCode::End => Some(InputAction::LineEnd),
            KeyCode::Escape => Some(InputAction::ClearInput),
            _ => None,
        }
    } else {
        None
    };

    tracing::debug!(
        key = ?event.code,
        mods = ?event.modifiers,
        action = ?action,
        "mapped terminal key event"
    );

    action
}

/// Resolve the action for an `Enter` key press given its modifiers.
///
/// `Alt+Enter` (in any modifier combination containing `ALT`) always
/// inserts a newline. A bare `Shift+Enter` inserts a newline only with
/// the Kitty keyboard protocol; otherwise it — like a plain `Enter` —
/// submits.
fn map_enter(mods: Modifiers, caps: &TerminalCaps) -> InputAction {
    if mods.contains(Modifiers::ALT) || (mods.contains(Modifiers::SHIFT) && caps.kitty_keyboard) {
        InputAction::InsertNewline
    } else {
        InputAction::Submit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty_caps() -> TerminalCaps {
        let mut caps = TerminalCaps::baseline();
        caps.kitty_keyboard = true;
        caps
    }

    #[test]
    fn enter_maps_to_submit() {
        let event = KeyEvent::new(KeyCode::Enter, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::Submit)
        );
    }

    #[test]
    fn alt_enter_maps_to_insert_newline() {
        let event = KeyEvent::new(KeyCode::Enter, Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::InsertNewline)
        );
    }

    #[test]
    fn shift_enter_with_kitty_maps_to_insert_newline() {
        let event = KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT);
        assert_eq!(
            map_key_event(event, &kitty_caps(), false),
            Some(InputAction::InsertNewline)
        );
    }

    #[test]
    fn shift_enter_without_kitty_falls_back_to_submit() {
        let event = KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::Submit)
        );
    }

    #[test]
    fn ctrl_c_maps_to_exit() {
        let event = KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::Exit)
        );
    }

    #[test]
    fn plain_char_maps_to_insert_char() {
        let event = KeyEvent::new(KeyCode::Char('a'), Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::InsertChar('a'))
        );
    }

    #[test]
    fn shifted_char_maps_to_insert_char() {
        let event = KeyEvent::new(KeyCode::Char('A'), Modifiers::SHIFT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::InsertChar('A'))
        );
    }

    #[test]
    fn up_without_popup_maps_to_cursor_up() {
        // Bare Up emits CursorUp now — the event loop falls through to
        // history_prev when the cursor is on the first visual line, but
        // the mapper itself is state-free.
        let event = KeyEvent::new(KeyCode::Up, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::CursorUp)
        );
    }

    #[test]
    fn up_with_popup_open_maps_to_nothing() {
        let event = KeyEvent::new(KeyCode::Up, Modifiers::NONE);
        assert_eq!(map_key_event(event, &TerminalCaps::baseline(), true), None);
    }

    #[test]
    fn down_without_popup_maps_to_cursor_down() {
        let event = KeyEvent::new(KeyCode::Down, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::CursorDown)
        );
    }

    #[test]
    fn down_with_popup_open_maps_to_nothing() {
        let event = KeyEvent::new(KeyCode::Down, Modifiers::NONE);
        assert_eq!(map_key_event(event, &TerminalCaps::baseline(), true), None);
    }

    #[test]
    fn alt_left_maps_to_word_left() {
        let event = KeyEvent::new(KeyCode::Left, Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::WordLeft)
        );
    }

    #[test]
    fn alt_right_maps_to_word_right() {
        let event = KeyEvent::new(KeyCode::Right, Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::WordRight)
        );
    }

    #[test]
    fn super_left_maps_to_line_start() {
        let event = KeyEvent::new(KeyCode::Left, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineStart)
        );
    }

    #[test]
    fn super_right_maps_to_line_end() {
        let event = KeyEvent::new(KeyCode::Right, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineEnd)
        );
    }

    #[test]
    fn super_up_maps_to_buffer_start() {
        let event = KeyEvent::new(KeyCode::Up, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::BufferStart)
        );
    }

    #[test]
    fn super_down_maps_to_buffer_end() {
        let event = KeyEvent::new(KeyCode::Down, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::BufferEnd)
        );
    }

    #[test]
    fn super_up_with_popup_open_is_suppressed() {
        // Popup ownership of Up/Down takes precedence over SUPER —
        // otherwise Cmd+Up while the autocomplete is open would jump
        // the cursor instead of navigating candidates.
        let event = KeyEvent::new(KeyCode::Up, Modifiers::SUPER);
        assert_eq!(map_key_event(event, &TerminalCaps::baseline(), true), None);
    }

    #[test]
    fn super_takes_precedence_over_alt_on_left() {
        // Cmd+Option+Left — both bits set — picks LineStart (Cmd wins),
        // mirroring macOS where the OS-level shortcut hierarchy puts
        // Command above Option.
        let event = KeyEvent::new(KeyCode::Left, Modifiers::SUPER | Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineStart)
        );
    }

    #[test]
    fn super_takes_precedence_over_alt_on_backspace() {
        let event = KeyEvent::new(KeyCode::Backspace, Modifiers::SUPER | Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteToLineStart)
        );
    }

    #[test]
    fn non_kitty_fallback_reaches_line_start_via_home() {
        let event = KeyEvent::new(KeyCode::Home, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineStart)
        );
    }

    #[test]
    fn end_maps_to_line_end_as_non_kitty_fallback() {
        let event = KeyEvent::new(KeyCode::End, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineEnd)
        );
    }

    #[test]
    fn ctrl_a_maps_to_line_start_as_readline_fallback() {
        let event = KeyEvent::new(KeyCode::Char('a'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::LineStart)
        );
    }

    #[test]
    fn ctrl_u_maps_to_delete_to_line_start_as_readline_fallback() {
        let event = KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteToLineStart)
        );
    }

    #[test]
    fn escape_maps_to_clear_input() {
        let event = KeyEvent::new(KeyCode::Escape, Modifiers::NONE);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::ClearInput)
        );
    }

    #[test]
    fn backspace_and_delete_map_to_their_actions() {
        assert_eq!(
            map_key_event(
                KeyEvent::new(KeyCode::Backspace, Modifiers::NONE),
                &TerminalCaps::baseline(),
                false,
            ),
            Some(InputAction::Backspace)
        );
        assert_eq!(
            map_key_event(
                KeyEvent::new(KeyCode::Delete, Modifiers::NONE),
                &TerminalCaps::baseline(),
                false,
            ),
            Some(InputAction::Delete)
        );
    }

    #[test]
    fn alt_backspace_maps_to_delete_word_back() {
        let event = KeyEvent::new(KeyCode::Backspace, Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteWordBack)
        );
    }

    #[test]
    fn alt_delete_maps_to_delete_word_forward() {
        let event = KeyEvent::new(KeyCode::Delete, Modifiers::ALT);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteWordForward)
        );
    }

    #[test]
    fn super_backspace_maps_to_delete_to_line_start() {
        let event = KeyEvent::new(KeyCode::Backspace, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteToLineStart)
        );
    }

    #[test]
    fn super_delete_maps_to_delete_to_line_end() {
        let event = KeyEvent::new(KeyCode::Delete, Modifiers::SUPER);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteToLineEnd)
        );
    }

    #[test]
    fn ctrl_k_maps_to_delete_to_line_end() {
        let event = KeyEvent::new(KeyCode::Char('k'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::DeleteToLineEnd)
        );
    }

    #[test]
    fn cursor_keys_map_to_their_actions() {
        assert_eq!(
            map_key_event(
                KeyEvent::new(KeyCode::Left, Modifiers::NONE),
                &TerminalCaps::baseline(),
                false,
            ),
            Some(InputAction::CursorLeft)
        );
        assert_eq!(
            map_key_event(
                KeyEvent::new(KeyCode::Right, Modifiers::NONE),
                &TerminalCaps::baseline(),
                false,
            ),
            Some(InputAction::CursorRight)
        );
    }

    #[test]
    fn release_kind_events_return_none() {
        let event = KeyEvent {
            code: KeyCode::Enter,
            kind: KeyEventKind::Release,
            modifiers: Modifiers::NONE,
            state: termina::event::KeyEventState::NONE,
        };
        assert_eq!(map_key_event(event, &TerminalCaps::baseline(), false), None);
    }

    #[test]
    fn unmapped_key_returns_none() {
        let event = KeyEvent::new(KeyCode::Insert, Modifiers::NONE);
        assert_eq!(map_key_event(event, &TerminalCaps::baseline(), false), None);
    }

    #[test]
    fn ctrl_o_maps_to_toggle_verbosity() {
        let event = KeyEvent::new(KeyCode::Char('o'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::ToggleVerbosity)
        );
    }

    #[test]
    fn ctrl_e_maps_to_toggle_thinking() {
        let event = KeyEvent::new(KeyCode::Char('e'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::ToggleThinking)
        );
    }

    #[test]
    fn ctrl_t_maps_to_toggle_in_flight_submit_mode() {
        let event = KeyEvent::new(KeyCode::Char('t'), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, &TerminalCaps::baseline(), false),
            Some(InputAction::ToggleInFlightSubmitMode),
        );
    }
}
