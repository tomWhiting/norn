//! Exact shortcut matching, settings validation and collision tests without terminal side effects.

use super::*;
use serde_json::json;
use termina::event::KeyEventKind;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn declared_option_and_function_aliases_validate_and_round_trip() -> TestResult {
    let bindings = ViewShortcuts::decode(None)?;
    assert_eq!(bindings, ViewShortcuts::default());
    assert_eq!(
        ViewShortcuts::decode(Some(&bindings.projection()))?,
        bindings
    );
    for (action, letter, function) in [
        (ViewAction::PaneToggle, 'p', 7),
        (ViewAction::PaneDiff, 'd', 8),
        (ViewAction::PaneAgents, 'a', 9),
        (ViewAction::SendKeyCycle, 's', 10),
    ] {
        assert_eq!(
            bindings.action(KeyEvent::new(KeyCode::Char(letter), Modifiers::ALT)),
            Some(action)
        );
        assert_eq!(
            bindings.action(KeyEvent::new(KeyCode::Function(function), Modifiers::NONE)),
            Some(action)
        );
    }
    for (action, function, modifiers) in [
        (ViewAction::UpperSwitch, 2, Modifiers::NONE),
        (ViewAction::Search, 3, Modifiers::NONE),
        (ViewAction::Copy, 4, Modifiers::NONE),
        (ViewAction::Export, 5, Modifiers::NONE),
        (ViewAction::FocusNext, 6, Modifiers::NONE),
        (ViewAction::FocusPrevious, 6, Modifiers::SHIFT),
    ] {
        assert_eq!(
            bindings.action(KeyEvent::new(KeyCode::Function(function), modifiers)),
            Some(action)
        );
    }
    assert_eq!(bindings.hint(ViewAction::SendKeyCycle), "Option+s / F10");
    Ok(())
}

#[test]
fn extra_delivered_modifiers_never_alias_option_or_infer_command() {
    let bindings = ViewShortcuts::default();
    for modifiers in [
        Modifiers::NONE,
        Modifiers::SHIFT,
        Modifiers::CONTROL,
        Modifiers::SUPER,
        Modifiers::META,
        Modifiers::ALT | Modifiers::SHIFT,
        Modifiers::ALT | Modifiers::CONTROL,
        Modifiers::ALT | Modifiers::SUPER,
        Modifiers::ALT | Modifiers::META,
        Modifiers::ALT | Modifiers::HYPER,
        Modifiers::ALT | Modifiers::CAPS_LOCK,
    ] {
        assert_eq!(
            bindings.action(KeyEvent::new(KeyCode::Char('p'), modifiers)),
            None
        );
    }
    assert_eq!(
        bindings.action(KeyEvent::new(KeyCode::Char('P'), Modifiers::ALT)),
        Some(ViewAction::PaneToggle)
    );
    for code in [
        KeyCode::Enter,
        KeyCode::Escape,
        KeyCode::Left,
        KeyCode::Function(0),
        KeyCode::Function(13),
    ] {
        assert_eq!(bindings.action(KeyEvent::new(code, Modifiers::ALT)), None);
    }
}

#[test]
fn identity_keeps_repeat_and_release_for_the_input_owner_to_consume() {
    let bindings = ViewShortcuts::default();
    for kind in [
        KeyEventKind::Press,
        KeyEventKind::Repeat,
        KeyEventKind::Release,
    ] {
        let mut event = KeyEvent::new(KeyCode::Char('p'), Modifiers::ALT);
        event.kind = kind;
        assert_eq!(bindings.action(event), Some(ViewAction::PaneToggle));
    }
}

#[test]
fn unknown_types_and_actions_name_the_owned_path() {
    for (value, path) in [
        (Value::Null, "tui.input.bindings"),
        (json!([]), "tui.input.bindings"),
        (json!({"future":[]}), "tui.input.bindings.future"),
        (
            json!({"pane_toggle":"alt+p"}),
            "tui.input.bindings.pane_toggle",
        ),
        (
            json!({"pane_toggle":[false]}),
            "tui.input.bindings.pane_toggle[0]",
        ),
        (
            json!({"pane_toggle":[null]}),
            "tui.input.bindings.pane_toggle[0]",
        ),
    ] {
        let error = ViewShortcuts::decode(Some(&value)).err();
        assert!(
            error.is_some_and(|error| error.to_string().contains(path)),
            "missing {path}"
        );
    }
}

#[test]
fn inherited_alias_and_normalized_same_action_duplicates_are_refused() {
    for value in [
        json!({"pane_toggle":["alt+d"]}),
        json!({"pane_toggle":["f8"]}),
        json!({"pane_toggle":["option+q","ALT+Q"]}),
    ] {
        let result = ViewShortcuts::decode(Some(&value));
        assert!(matches!(result, Err(ShortcutError::Duplicate { .. })));
    }
    let result = ViewShortcuts::decode(Some(&json!({"pane_toggle":["f8"]})));
    let message = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("tui.input.bindings.pane_toggle[0]"));
    assert!(message.contains("tui.input.bindings.pane_diff[1]"));
}

#[test]
fn typing_navigation_wildcards_ambiguous_modifiers_and_sequences_are_refused() {
    for key in [
        "p",
        "shift+p",
        "enter",
        "alt+enter",
        "ctrl+left",
        "escape",
        "tab",
        "alt+{char}",
        "~shift+alt+p",
        "cmd+p",
        "meta+p",
        "super+p",
        "altgraph+p",
        "ctrl+p ctrl+d",
        "",
    ] {
        assert!(
            ViewShortcuts::default()
                .replacement("pane_toggle", &[key])
                .is_err(),
            "accepted {key}"
        );
    }
}

#[test]
fn existing_host_and_editor_actions_are_not_stolen() {
    for (key, owner) in [
        ("ctrl+c", "Norn"),
        ("ctrl+alt+t", "Norn"),
        ("ctrl+z", "history.undo"),
        ("ctrl+x", "clipboard.cut"),
        ("shift+alt+a", "comment.toggleBlock"),
    ] {
        let result = ViewShortcuts::default().replacement("pane_toggle", &[key]);
        let error = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("tui.input.bindings.pane_toggle[0]"),
            "missing binding referent for {key}"
        );
        assert!(
            error.contains(owner),
            "wrong collision owner for {key}: {error}"
        );
    }
}

#[test]
fn complete_replacement_can_move_a_function_key_without_a_partial_registry() -> TestResult {
    let bindings = ViewShortcuts::decode(Some(&json!({"pane_diff":[],"pane_toggle":["f8"]})))?;
    assert_eq!(
        bindings.action(KeyEvent::new(KeyCode::Function(8), Modifiers::NONE)),
        Some(ViewAction::PaneToggle)
    );
    assert_eq!(
        bindings.action(KeyEvent::new(KeyCode::Char('d'), Modifiers::ALT)),
        None
    );
    assert_eq!(bindings.hint(ViewAction::PaneDiff), "unbound");
    assert_eq!(bindings.projection()["pane_diff"], json!([]));
    let before = bindings.clone();
    assert!(bindings.replacement("search", &["f8"]).is_err());
    assert_eq!(bindings, before);
    let edited = bindings.replacement("search", &["option+q"])?;
    assert_eq!(edited.projection()["search"], json!(["alt+q"]));
    assert_eq!(
        edited.action(KeyEvent::new(KeyCode::Function(3), Modifiers::NONE)),
        None
    );
    assert_eq!(
        edited.action(KeyEvent::new(KeyCode::Char('q'), Modifiers::ALT)),
        Some(ViewAction::Search)
    );
    Ok(())
}
