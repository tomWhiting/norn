//! Actual retained-App PTY reading controls; emitted clipboard bytes are not clipboard acceptance.
//! No history/body demand counts are claimed: the App currently exposes no such observer.

#[path = "support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/retained_workspace.rs"]
pub mod support;

use support::{TestResult, Workspace, with_composer, with_workspace};

#[test]
fn retained_workspace_child_entrypoint() -> TestResult {
    support::child_entrypoint()
}

#[test]
fn actual_idle_and_active_pane_commands_preserve_original_composer_and_admission() -> TestResult {
    with_composer("enter", |app| {
        let idle = app.snapshot()?;
        assert_eq!(idle["provider_calls"], 0);
        assert_eq!(idle["user_events"], serde_json::json!([]));
        exercise_panes(app)?;
        assert_eq!(app.snapshot()?, idle, "idle pane commands admitted work");

        let prompt = "pane fixture prompt";
        app.input(prompt.as_bytes(), |screen| {
            screen.lines()[screen.cursor.1] == prompt
        })?;
        app.input(app.submit_key(), |screen| {
            screen.contains("workspace provider held")
                && screen
                    .composer_rows()
                    .iter()
                    .all(|row| screen.lines()[*row].is_empty())
        })?;
        let active = app.snapshot()?;
        assert_eq!(active["provider_calls"], 1);
        assert_eq!(active["user_events"], serde_json::json!([prompt]));
        exercise_panes(app)?;
        assert_eq!(
            app.snapshot()?,
            active,
            "active pane commands changed the actual provider or accepted event census"
        );
        Ok(prompt.to_owned())
    })
}

fn exercise_panes(app: &mut Workspace) -> TestResult {
    let changes = app.command("/pane diff", "Changes · select a tool call")?;
    assert_wide_pane(&changes, "Changes · select a tool call")?;
    let agents = app.command("/pane agents", " root  ")?;
    assert_wide_pane(&agents, " root  ")?;
    assert!(!agents.contains("Changes · select a tool call"));

    // The actual root registry entry is enough: this does not fabricate child agents.
    let hidden = app.command("/pane ", "")?;
    assert_no_split(&hidden)?;
    let shown = app.command("/pane ", " root  ")?;
    assert_wide_pane(&shown, " root  ")?;

    // A real draft crosses the old split column in the full-width original input row.
    let draft = "full width composer ".repeat(4);
    let edited = app.input(format!("\x1b[200~{draft}\x1b[201~").as_bytes(), |screen| {
        screen.cursor.0 == draft.len()
    })?;
    edited.assert_composer(1)?;
    let input_row = edited.composer_rows()[0];
    for (column, character) in draft.chars().enumerate() {
        assert_eq!(
            edited.cell(column, input_row),
            Some(character.to_string().as_str())
        );
        assert_eq!(edited.foreground_at(column, input_row), None);
    }
    app.input(b"\x1b", |screen| {
        screen.cursor.0 == 0 && screen.lines()[screen.cursor.1].is_empty()
    })?;

    let narrow = app.resize(48, 10)?;
    narrow.assert_composer(1)?;
    assert!(
        narrow.contains(" root  "),
        "selected Agents pane vanished on narrow resize"
    );
    let narrow_changes = app.command("/pane diff", "Changes · select a tool call")?;
    narrow_changes.assert_composer(1)?;
    assert!(
        narrow_changes
            .lines()
            .iter()
            .any(|line| line.starts_with("Changes"))
    );
    assert!(!narrow_changes.contains(" root  "));
    let narrow_agents = app.command("/pane agents", " root  ")?;
    narrow_agents.assert_composer(1)?;
    assert!(
        narrow_agents
            .lines()
            .iter()
            .any(|line| line.starts_with("Agents"))
    );
    let tiny = app.resize(1, 4)?;
    tiny.assert_composer(1)?;
    assert_eq!(tiny.cursor.0, 0);
    let restored = app.resize(100, 24)?;
    assert_wide_pane(&restored, " root  ")?;
    let closed = app.command("/pane ", "")?;
    assert_no_split(&closed)?;
    Ok(())
}

fn assert_wide_pane(screen: &retained_screen::Screen, expected: &str) -> TestResult {
    screen.assert_composer(1)?;
    assert_eq!((screen.cols, screen.rows), (100, 24));
    // Layout::split gives the indivisible surplus column to the conversation:
    // ceil(99 / 2) = 50 columns left, one divider, then 49 columns right.
    let divider = 50;
    let input_row = screen.composer_rows()[0];
    for row in 0..input_row - 1 {
        assert_eq!(screen.cell(divider, row), Some("│"));
    }
    assert!(
        (0..input_row - 1).any(|row| {
            (divider + 1..usize::from(screen.cols))
                .filter_map(|column| screen.cell(column, row))
                .collect::<String>()
                .contains(expected)
        }),
        "requested pane content is absent from the right pane:\n{}",
        screen.debug_text()
    );
    for column in 0..usize::from(screen.cols) {
        assert_eq!(screen.cell(column, input_row), Some(" "));
    }
    Ok(())
}

fn assert_no_split(screen: &retained_screen::Screen) -> TestResult {
    screen.assert_composer(1)?;
    let input_row = screen.composer_rows()[0];
    for row in 0..input_row - 1 {
        assert_ne!(screen.cell(50, row), Some("│"));
    }
    Ok(())
}

#[test]
fn original_selection_search_export_and_clipboard_survive_resize_without_admission() -> TestResult {
    with_workspace(|app| {
        let before = app.snapshot()?;
        assert_eq!(before["provider_calls"], 1);
        app.command("/view help", "View controls")?;
        app.command("/view select 0 0 14", "Original text selected")?;
        app.key(b"\x1bOS", "Clipboard unavailable")?;
        assert!(
            app.copy_payloads()?.is_empty(),
            "unspecified clipboard emitted OSC 52"
        );
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        app.command("/view copy", "Sent 14 selected bytes")?;
        // Original hard newline included; no screen decoration or soft-wrap newline.
        assert_eq!(app.copy_payloads()?, [b"VmlldyBjb250cm9scwo=".to_vec()]);

        app.resize(32, 9)?;
        app.command("/view copy", "Sent 14 selected bytes")?;
        assert_eq!(app.copy_payloads()?.len(), 2);
        assert_eq!(
            app.copy_payloads()?.last(),
            Some(&b"VmlldyBjb250cm9scwo=".to_vec())
        );
        app.resize(100, 24)?;
        let destination = app.destination("selected original.txt");
        app.command(
            &format!("/view export {}", destination.display()),
            "Exported 14 original bytes",
        )?;
        assert_eq!(std::fs::read(&destination)?, b"View controls\n");
        app.command(
            &format!("/view export {}", destination.display()),
            "Export failed",
        )?;
        assert_eq!(
            std::fs::read(&destination)?,
            b"View controls\n",
            "create-new refusal changed destination"
        );

        app.command("/view select 0 0 4", "Original text selected")?;
        app.command(
            &format!("/view export --replace {}", destination.display()),
            "Exported 4 original bytes",
        )?;
        assert_eq!(std::fs::read(&destination)?, b"View");
        app.command("/view search selected clipboard", "Match 1/")?;
        app.command("/view copy", "Sent 9 selected bytes")?;
        assert_eq!(app.copy_payloads()?.len(), 3);
        assert_eq!(app.copy_payloads()?.last(), Some(&b"Y2xpcGJvYXJk".to_vec()));
        assert_eq!(
            app.snapshot()?,
            before,
            "local reading actions changed actual provider/store census"
        );
        Ok(())
    })
}

#[test]
fn mouse_drag_selects_original_source_and_resize_preserves_it_without_admission() -> TestResult {
    with_workspace(|app| {
        let before = app.snapshot()?;
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        let screen = app.command("/view help", "View controls")?;
        let row = screen
            .lines()
            .iter()
            .position(|line| line.starts_with("View controls"))
            .ok_or_else(|| {
                std::io::Error::other(format!("help body row absent:\n{}", screen.debug_text()))
            })?;
        app.mouse_drag(1, 5, u16::try_from(row + 1)?)?;
        app.key(b"\x1bOS", "Sent 4 selected bytes")?;
        assert_eq!(app.copy_payloads()?, [b"Vmlldw==".to_vec()]);
        app.resize(36, 10)?;
        app.key(b"\x1bOS", "Sent 4 selected bytes")?;
        assert_eq!(app.copy_payloads()?.len(), 2);
        assert_eq!(app.copy_payloads()?.last(), Some(&b"Vmlldw==".to_vec()));
        app.resize(100, 24)?;
        app.command("/view focus composer", "View controls")?;
        app.command("/view selection", "Original selection")?;
        assert_eq!(
            app.snapshot()?,
            before,
            "mouse/resize reading actions admitted work or changed history"
        );
        Ok(())
    })
}
