//! Explicit shortcut inspection and atomic local edits through the existing preference owner.

use crate::app::state::AppState;

pub(super) fn command(arguments: &[&str], state: &mut AppState) -> Result<(), String> {
    match arguments {
        [] => {
            let body = state.view_shortcuts.summary();
            let item = crate::app::notices::notice(state, "View shortcuts", Some(&body))
                .map_err(|error| error.to_string())?;
            state
                .screen
                .viewport
                .scroll_to(
                    crate::app::viewport::ViewAnchor {
                        item,
                        position: crate::app::viewport::AnchorPosition::Header,
                    },
                    &state.transcript.projection,
                )
                .map_err(|error| error.to_string())
        }
        ["set", action, keys @ ..] if !keys.is_empty() => {
            let replacement = state
                .view_shortcuts
                .replacement(action, keys)
                .map_err(|error| error.to_string())?;
            state.view_shortcuts = std::sync::Arc::new(replacement);
            state.screen.feedback = Some(format!("View shortcuts updated: {action}"));
            Ok(())
        }
        ["clear", action] => {
            let replacement = state
                .view_shortcuts
                .replacement(action, &[])
                .map_err(|error| error.to_string())?;
            state.view_shortcuts = std::sync::Arc::new(replacement);
            state.screen.feedback = Some(format!("View shortcuts unbound: {action}"));
            Ok(())
        }
        _ => Err(
            "Use /view keys, /view keys set <action> <stroke>..., or /view keys clear <action>"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::view_shortcuts::ViewAction;
    use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn state() -> AppState {
        AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(uuid::Uuid::new_v4()),
            crate::render::fixed_panel::StatusBar::default(),
        )
    }

    #[test]
    fn rejected_binding_edit_retains_exact_registry_and_draft() -> TestResult {
        let mut state = state();
        crate::app::event_loop::insert_paste_text(&mut state, "unfinished🙂")?;
        let original = state.input_editor.snapshot()?;
        let bindings = std::sync::Arc::clone(&state.view_shortcuts);
        assert!(command(&["set", "pane_toggle", "alt+q", "ctrl+z"], &mut state).is_err());
        assert!(std::sync::Arc::ptr_eq(&state.view_shortcuts, &bindings));
        state.input_editor.validate_snapshot(&original)?;
        command(&["set", "pane_toggle", "alt+q"], &mut state)?;
        state.input_editor.validate_snapshot(&original)?;
        assert_eq!(
            state.view_shortcuts.hint(ViewAction::PaneToggle),
            "Option+q"
        );
        command(&["clear", "pane_toggle"], &mut state)?;
        assert_eq!(state.view_shortcuts.hint(ViewAction::PaneToggle), "unbound");
        state.input_editor.validate_snapshot(&original)?;
        Ok(())
    }

    #[test]
    fn configurable_actions_consume_only_their_exact_identity_and_act_once() -> TestResult {
        let mut state = state();
        crate::app::event_loop::insert_paste_text(&mut state, "draft stays")?;
        state.screen.layout = crate::render::layout::Layout::calculate(
            crate::render::layout::LayoutRequest {
                columns: 100,
                rows: 24,
                requested_composer_rows: 1,
                changes_open: state.screen.changes_open,
                split: state.screen.split,
                active_upper_pane: state.screen.upper,
            },
            crate::render::layout::LayoutPolicy::default(),
        )?;
        let original = state.input_editor.snapshot()?;
        let key = KeyEvent::new(KeyCode::Char('p'), Modifiers::ALT);
        assert!(crate::app::view_actions::key(key, &mut state));
        assert!(state.screen.changes_open);
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            let mut repeated = key;
            repeated.kind = kind;
            assert!(crate::app::view_actions::key(repeated, &mut state));
            assert!(state.screen.changes_open);
        }
        let mixed = KeyEvent::new(KeyCode::Char('p'), Modifiers::ALT | Modifiers::SHIFT);
        assert!(!crate::app::view_actions::key(mixed, &mut state));
        assert!(state.screen.changes_open);
        state.input_editor.validate_snapshot(&original)?;
        assert_eq!(state.transcript.projection.items().len(), 0);
        Ok(())
    }
}
