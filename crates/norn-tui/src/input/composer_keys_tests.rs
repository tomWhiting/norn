//! Delivered keys preserve kernel editing while Norn retains send and host controls.

use iridium_editor::CommandArgs;
use iridium_editor::cell_layout::CellWrapParameters;
use iridium_editor::editor::CellInputOptions;
use iridium_editor::{EditorKeyResult, KeyCode as EditorCode};

use super::*;
use crate::frontend_preferences::ComposerSendKey;
use crate::input::InputEditor;
use crate::input::history::InputHistory;
use crate::input::keybindings::{InputAction, map_key_event};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn options() -> CellInputOptions {
    CellInputOptions {
        wrap: CellWrapParameters::new(80, 4),
        visible_rows: 10,
    }
}

#[test]
fn delivered_modifiers_and_repeat_survive_the_shared_translator() -> TestResult {
    let mut event = KeyEvent::new(
        KeyCode::Left,
        Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::ALT | Modifiers::META,
    );
    event.kind = KeyEventKind::Repeat;
    let key = to_kernel_key(event).ok_or("modified repeated arrow was omitted")?;
    assert_eq!(key.key, EditorCode::Left);
    assert!(key.modifiers.shift);
    assert!(key.modifiers.ctrl);
    assert!(key.modifiers.alt);
    assert!(key.modifiers.meta);
    assert!(!key.modifiers.alt_graph);
    assert!(key.is_repeat);
    Ok(())
}

#[test]
fn capital_and_backtab_keep_their_delivered_text_and_selection_modifiers() -> TestResult {
    let key = to_kernel_key(KeyEvent::new(KeyCode::Char('A'), Modifiers::NONE))
        .ok_or("capital key was omitted")?;
    assert_eq!(key.key, EditorCode::Char('A'));
    assert!(key.modifiers.shift);
    let mut editor = InputEditor::new(InputHistory::in_memory());
    assert_eq!(
        editor.handle_cell_key(&key, options())?,
        EditorKeyResult::None
    );
    assert_eq!(editor.text(), "A");

    let tab = to_kernel_key(KeyEvent::new(KeyCode::BackTab, Modifiers::NONE))
        .ok_or("backtab key was omitted")?;
    assert_eq!(tab.key, EditorCode::Tab);
    assert!(tab.modifiers.shift);
    Ok(())
}

#[test]
fn meta_stays_distinct_from_command_letter_compatibility() -> TestResult {
    for (modifiers, control, meta) in [
        (Modifiers::SUPER, true, false),
        (Modifiers::META, false, true),
        (Modifiers::SUPER | Modifiers::META, true, true),
    ] {
        let key = to_kernel_key(KeyEvent::new(KeyCode::Char('z'), modifiers))
            .ok_or("modified letter translation absent")?;
        assert_eq!(key.key, EditorCode::Char('z'));
        assert_eq!(key.modifiers.ctrl, control);
        assert_eq!(key.modifiers.meta, meta);
    }
    let arrow = to_kernel_key(KeyEvent::new(KeyCode::Left, Modifiers::SUPER))
        .ok_or("Command arrow translation absent")?;
    assert!(arrow.modifiers.meta);
    assert!(!arrow.modifiers.ctrl);
    Ok(())
}

#[test]
fn backtab_and_shift_tab_execute_the_same_kernel_outdent() -> TestResult {
    for event in [
        KeyEvent::new(KeyCode::BackTab, Modifiers::NONE),
        KeyEvent::new(KeyCode::Tab, Modifiers::SHIFT),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells("    line")?;
        let key = to_kernel_key(event).ok_or("outdent key translation absent")?;
        assert_eq!(
            editor.handle_cell_key(&key, options())?,
            EditorKeyResult::None
        );
        assert_eq!(editor.text(), "line");
        assert_eq!(
            editor.run_cell_command("history.undo", CommandArgs::NONE, options())?,
            EditorKeyResult::None
        );
        assert_eq!(editor.text(), "    line");
    }
    Ok(())
}

#[test]
fn release_hyper_and_unrepresented_function_keys_never_become_edits() {
    let mut release = KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE);
    release.kind = KeyEventKind::Release;
    for event in [
        release,
        KeyEvent::new(KeyCode::Char('x'), Modifiers::HYPER),
        KeyEvent::new(KeyCode::Function(13), Modifiers::NONE),
    ] {
        assert!(to_kernel_key(event).is_none());
        assert_eq!(map_key_event(event, ComposerSendKey::Enter, false), None);
    }
}

#[test]
fn command_undo_and_shift_redo_replay_the_same_original_kernel_transaction() -> TestResult {
    let mut editor = InputEditor::new(InputHistory::in_memory());
    let original = "α\r\n👩‍💻";
    editor.paste_cells(original)?;
    let cursor = editor.kernel().state().cursor.clone();
    let undo = KeyEvent::new(KeyCode::Char('z'), Modifiers::SUPER);
    assert_eq!(
        map_key_event(undo, ComposerSendKey::Enter, false),
        Some(InputAction::KernelKey(undo))
    );
    let key = to_kernel_key(undo).ok_or("Command+Z translation absent")?;
    assert!(key.modifiers.ctrl);
    assert!(!key.modifiers.meta);
    assert_eq!(
        editor.handle_cell_key(&key, options())?,
        EditorKeyResult::None
    );
    assert_eq!(editor.text(), "");

    let redo = KeyEvent::new(KeyCode::Char('z'), Modifiers::SUPER | Modifiers::SHIFT);
    let key = to_kernel_key(redo).ok_or("Command+Shift+Z translation absent")?;
    assert!(key.modifiers.ctrl && key.modifiers.shift);
    assert!(!key.modifiers.meta);
    assert_eq!(
        editor.handle_cell_key(&key, options())?,
        EditorKeyResult::None
    );
    assert_eq!(editor.text(), original);
    assert_eq!(editor.kernel().state().cursor, cursor);
    Ok(())
}

#[test]
fn mac_shift_motion_selects_the_kernel_original_byte_range() -> TestResult {
    for (modifiers, expected) in [
        (Modifiers::SUPER | Modifiers::SHIFT, 0..7),
        (Modifiers::ALT | Modifiers::SHIFT, 4..7),
    ] {
        let mut editor = InputEditor::new(InputHistory::in_memory());
        editor.paste_cells("abc def")?;
        let event = KeyEvent::new(KeyCode::Left, modifiers);
        assert_eq!(
            map_key_event(event, ComposerSendKey::Enter, false),
            Some(InputAction::KernelKey(event))
        );
        let command = motion_command(event).ok_or("modified left motion absent")?;
        assert_eq!(
            editor.run_cell_command(command, CommandArgs::NONE, options())?,
            EditorKeyResult::None
        );
        assert_eq!(editor.snapshot()?.selection()?, Some(expected));
        assert_eq!(editor.text(), "abc def");
    }
    Ok(())
}

#[test]
fn repeated_readline_and_mac_motion_keep_their_press_semantics() {
    for (code, modifiers, expected) in [
        (
            KeyCode::Char('a'),
            Modifiers::CONTROL,
            InputAction::LineStart,
        ),
        (
            KeyCode::Char('u'),
            Modifiers::CONTROL,
            InputAction::DeleteToLineStart,
        ),
        (
            KeyCode::Char('k'),
            Modifiers::CONTROL,
            InputAction::DeleteToLineEnd,
        ),
        (KeyCode::Left, Modifiers::ALT, InputAction::WordLeft),
        (KeyCode::Right, Modifiers::SUPER, InputAction::LineEnd),
        (KeyCode::Up, Modifiers::SUPER, InputAction::BufferStart),
        (KeyCode::Down, Modifiers::SUPER, InputAction::BufferEnd),
    ] {
        let mut event = KeyEvent::new(code, modifiers);
        assert_eq!(
            map_key_event(event, ComposerSendKey::Enter, false),
            Some(expected)
        );
        event.kind = KeyEventKind::Repeat;
        assert_eq!(
            map_key_event(event, ComposerSendKey::Enter, false),
            Some(expected)
        );
    }
}

#[test]
fn send_policy_and_host_toggles_never_repeat_or_leak_to_the_kernel() {
    for (send_key, modifiers) in [
        (ComposerSendKey::Enter, Modifiers::NONE),
        (ComposerSendKey::ShiftEnter, Modifiers::SHIFT),
        (ComposerSendKey::AltEnter, Modifiers::ALT),
    ] {
        let mut event = KeyEvent::new(KeyCode::Enter, modifiers);
        assert_eq!(
            map_key_event(event, send_key, false),
            Some(InputAction::Submit)
        );
        event.kind = KeyEventKind::Repeat;
        assert_eq!(map_key_event(event, send_key, false), None);
    }
    for (code, expected) in [
        ('o', InputAction::ToggleVerbosity),
        ('e', InputAction::ToggleThinking),
        ('t', InputAction::ToggleInFlightSubmitMode),
    ] {
        let mut event = KeyEvent::new(KeyCode::Char(code), Modifiers::CONTROL);
        assert_eq!(
            map_key_event(event, ComposerSendKey::Enter, false),
            Some(expected)
        );
        event.kind = KeyEventKind::Repeat;
        assert_eq!(map_key_event(event, ComposerSendKey::Enter, false), None);
    }
    for modifiers in [Modifiers::CONTROL, Modifiers::SUPER] {
        let mut search = KeyEvent::new(KeyCode::Char('f'), modifiers);
        search.kind = KeyEventKind::Repeat;
        assert_eq!(map_key_event(search, ComposerSendKey::Enter, false), None);
    }
}

#[test]
fn modified_vertical_keys_do_not_accidentally_recall_history() {
    for code in [KeyCode::Up, KeyCode::Down] {
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::CONTROL] {
            let event = KeyEvent::new(code, modifiers);
            assert_eq!(
                map_key_event(event, ComposerSendKey::Enter, false),
                Some(InputAction::KernelKey(event))
            );
            assert_eq!(map_key_event(event, ComposerSendKey::Enter, true), None);
        }
    }
}

#[test]
fn shift_send_accepts_only_exact_delivered_shift_press() {
    for modifiers in [
        Modifiers::NONE,
        Modifiers::SHIFT,
        Modifiers::ALT,
        Modifiers::CONTROL,
        Modifiers::META,
        Modifiers::SUPER,
        Modifiers::SHIFT | Modifiers::ALT,
        Modifiers::SHIFT | Modifiers::CONTROL,
        Modifiers::SHIFT | Modifiers::META,
        Modifiers::SHIFT | Modifiers::SUPER,
    ] {
        let mut event = KeyEvent::new(KeyCode::Enter, modifiers);
        assert_eq!(
            map_key_event(event, ComposerSendKey::ShiftEnter, false),
            Some(if modifiers == Modifiers::SHIFT {
                InputAction::Submit
            } else {
                InputAction::InsertNewline
            }),
        );
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            event.kind = kind;
            assert_eq!(
                map_key_event(event, ComposerSendKey::ShiftEnter, false),
                None
            );
        }
    }
}
