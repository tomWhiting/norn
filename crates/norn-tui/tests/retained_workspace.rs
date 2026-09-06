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
    // Physical function keys preserve the same draft and never admit a turn.
    let hidden = app.input(b"\x1b[18~", |screen| screen.cell(50, 0) != Some("│"))?;
    assert_no_split(&hidden)?;
    assert_eq!(hidden.lines()[hidden.cursor.1], draft);
    let diff = app.input(b"\x1b[19~", |screen| {
        screen.contains("Changes · select a tool call")
    })?;
    assert_eq!(diff.cell(50, 0), Some("│"));
    assert_eq!(diff.lines()[diff.cursor.1], draft);
    let agents = app.input(b"\x1b[20~", |screen| screen.contains(" root  "))?;
    assert_eq!(agents.cell(50, 0), Some("│"));
    assert_eq!(agents.lines()[agents.cursor.1], draft);
    agents.assert_composer(1)?;
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

#[test]
fn generated_diff_drag_copies_display_scope_and_keeps_snapshot_after_resize() -> TestResult {
    with_workspace(|app| {
        let before = app.snapshot()?;
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        app.command("/pane diff", "Changes · select a tool call")?;
        let selected = app.input(b"\x1b[<0;52;1M\x1b[<32;59;1M\x1b[<0;59;1m", |screen| {
            screen.selected_at(51, 0) && screen.selected_at(57, 0)
        })?;
        assert!(!selected.selected_at(50, 0), "divider became selected text");
        let copied = app.key(b"\x1bOS", "Sent 7 selected bytes")?;
        assert!(
            copied.contains("displayed-text"),
            "copy scope is not explicit"
        );
        assert_eq!(app.copy_payloads()?, [b"Q2hhbmdlcw==".to_vec()]);
        app.resize(36, 10)?;
        app.key(b"\x1bOS", "Sent 7 selected bytes")?;
        assert_eq!(app.copy_payloads()?.last(), Some(&b"Q2hhbmdlcw==".to_vec()));
        app.resize(100, 24)?;
        app.command("/view focus composer", "Changes")?;
        assert_eq!(app.snapshot()?, before, "display selection admitted work");
        Ok(())
    })
}

fn locate(screen: &retained_screen::Screen, text: &str) -> TestResult<(u16, u16)> {
    use unicode_width::UnicodeWidthStr as _;
    for (row, line) in screen.lines().iter().enumerate() {
        if let Some(byte) = line.find(text) {
            return Ok((u16::try_from(line[..byte].width())?, u16::try_from(row)?));
        }
    }
    Err(std::io::Error::other(format!(
        "visible text {text:?} absent:\n{}",
        screen.debug_text()
    ))
    .into())
}

fn drag_bytes(start: (u16, u16), end: (u16, u16), release: bool) -> Vec<u8> {
    let (x, y) = (u32::from(start.0) + 1, u32::from(start.1) + 1);
    let (end_x, end_y) = (u32::from(end.0) + 1, u32::from(end.1) + 1);
    let mut bytes = format!("\x1b[<0;{x};{y}M\x1b[<32;{end_x};{end_y}M").into_bytes();
    if release {
        bytes.extend_from_slice(format!("\x1b[<0;{end_x};{end_y}m").as_bytes());
    }
    bytes
}

fn visible_range(screen: &retained_screen::Screen, start: (u16, u16), end: (u16, u16)) -> String {
    (start.1..=end.1)
        .map(|row| {
            let left = if row == start.1 { start.0 } else { 0 };
            let right = if row == end.1 { end.0 } else { screen.cols };
            (left..right)
                .filter_map(|column| screen.cell(usize::from(column), usize::from(row)))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn real_user_wide_body_and_cross_entry_drag_preserve_distinct_copy_scopes() -> TestResult {
    with_composer("enter", |app| {
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        let prompt = "a界b 👩‍💻 e\u{301}";
        app.input(format!("\x1b[200~{prompt}\x1b[201~").as_bytes(), |screen| {
            screen.contains(prompt)
        })?;
        app.input(app.submit_key(), |screen| {
            screen.contains("workspace provider held")
        })?;
        let screen = app.command("/view follow", "workspace provider held")?;
        let before = app.snapshot()?;
        let start = locate(&screen, prompt)?;
        // End inside the second cell of 界: highlight and original copy must agree.
        let end = (start.0 + 2, start.1);
        let selected = app.input(&drag_bytes(start, end, true), |screen| {
            screen.selected_at(usize::from(start.0), usize::from(start.1))
                && screen.selected_at(usize::from(start.0 + 2), usize::from(start.1))
        })?;
        assert!(selected.selected_at(usize::from(start.0 + 1), usize::from(start.1)));
        let copied = app.key(b"\x1bOS", "Sent 4 selected bytes")?;
        assert!(copied.contains("original selected bytes"));
        app.assert_last_copy("a界")?;

        let screen = app.screen()?;
        let start = locate(&screen, prompt)?;
        let assistant = locate(&screen, "workspace provider held")?;
        let end = (assistant.0 + 9, assistant.1);
        let expected = visible_range(&screen, start, end);
        assert!(expected.starts_with(prompt));
        assert!(expected.ends_with("workspace"));
        assert!(expected.contains('\n'));
        app.input(&drag_bytes(start, end, true), |screen| {
            screen.selected_at(usize::from(start.0), usize::from(start.1))
                && screen.selected_at(usize::from(end.0 - 1), usize::from(end.1))
        })?;
        let copied = app.key(
            b"\x1bOS",
            &format!("Sent {} selected bytes", expected.len()),
        )?;
        assert!(copied.contains("displayed-text"));
        app.assert_last_copy(&expected)?;
        app.resize(36, 10)?;
        app.key(
            b"\x1bOS",
            &format!("Sent {} selected bytes", expected.len()),
        )?;
        app.assert_last_copy(&expected)?;
        app.resize(100, 24)?;
        app.command("/view focus composer", "workspace")?;
        app.command("/view selection", "No original text selection")?;
        assert_eq!(app.snapshot()?, before, "selection or copy admitted work");
        Ok(prompt.to_owned())
    })
}

#[test]
fn collapsed_recorded_tool_header_and_real_diff_rows_are_selectable_without_expanding() -> TestResult
{
    support::with_recorded_tool(|app| {
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        let screen = app.command("/view follow", support::TOOL_DESCRIPTION)?;
        let before = app.snapshot()?;
        assert_eq!(before["provider_calls"], 1);
        assert_eq!(
            before["tool_results"]
                .as_array()
                .ok_or("tool result census missing")?
                .len(),
            1
        );
        let start = locate(&screen, support::TOOL_DESCRIPTION)?;
        let end = (
            start.0 + u16::try_from(support::TOOL_DESCRIPTION.len())?,
            start.1,
        );
        assert!(
            !screen.contains("old fixture text"),
            "tool started expanded"
        );
        app.input(&drag_bytes(start, end, true), |screen| {
            screen.selected_at(usize::from(start.0), usize::from(start.1))
                && screen.selected_at(usize::from(end.0 - 1), usize::from(end.1))
        })?;
        let copied = app.key(b"\x1bOS", "Sent 23 selected bytes")?;
        assert!(copied.contains("displayed-text"));
        assert!(
            !copied.contains("old fixture text"),
            "header drag expanded the tool"
        );
        app.assert_last_copy(support::TOOL_DESCRIPTION)?;
        app.command("/view focus composer", support::TOOL_DESCRIPTION)?;
        let diff = app.command("/pane diff", "Requested edit fragment")?;
        assert!(diff.contains("Changes · recorded call only"));
        let start = locate(&diff, "old fixture text")?;
        let end = (start.0 + 16, start.1);
        app.input(&drag_bytes(start, end, true), |screen| {
            screen.selected_at(usize::from(start.0), usize::from(start.1))
                && screen.selected_at(usize::from(end.0 - 1), usize::from(end.1))
        })?;
        let copied = app.key(b"\x1bOS", "Sent 16 selected bytes")?;
        assert!(copied.contains("displayed-text"));
        app.assert_last_copy("old fixture text")?;
        app.command("/view focus composer", "recorded call only")?;
        app.command("/view selection", "No original text selection")?;
        assert_eq!(
            app.snapshot()?,
            before,
            "recorded-view interaction executed a tool or admitted work"
        );
        Ok(())
    })
}

#[test]
fn provider_publication_during_drag_keeps_bytes_then_release_reveals_current_content() -> TestResult
{
    with_workspace(|app| {
        app.command("/view clipboard osc52", "Clipboard capability: osc52")?;
        let screen = app.command("/view follow", "workspace provider held")?;
        let start = locate(&screen, "workspace provider held")?;
        let end = (start.0 + 9, start.1);
        app.input(&drag_bytes(start, end, false), |screen| {
            screen.selected_at(usize::from(start.0), usize::from(start.1))
                && screen.selected_at(usize::from(end.0 - 1), usize::from(end.1))
        })?;
        let held = app.release_provider()?;
        assert!(held.contains("workspace provider held"));
        assert!(
            !held.contains("workspace provider released"),
            "stream replaced the dragged snapshot"
        );
        assert!(held.selected_at(usize::from(start.0), usize::from(start.1)));
        let completed = app.snapshot()?;
        assert_eq!(completed["provider_calls"], 1);
        assert_eq!(
            completed["assistant_events"],
            serde_json::json!(["workspace provider held\nworkspace provider released"])
        );
        let released = app.input(
            format!("\x1b[<0;{};{}m", u32::from(end.0) + 1, u32::from(end.1) + 1).as_bytes(),
            |screen| screen.contains("workspace provider released"),
        )?;
        assert!(released.contains("workspace provider held"));
        let copied = app.key(b"\x1bOS", "Sent 9 selected bytes")?;
        assert!(
            copied.contains("displayed-text"),
            "stale provisional reference kept original authority"
        );
        app.assert_last_copy("workspace")?;
        app.resize(36, 10)?;
        app.key(b"\x1bOS", "Sent 9 selected bytes")?;
        app.assert_last_copy("workspace")?;
        app.resize(100, 24)?;
        app.command("/view focus composer", "workspace provider released")?;
        app.command("/view selection", "No original text selection")?;
        assert_eq!(
            app.snapshot()?,
            completed,
            "copy replayed or admitted a provider turn"
        );
        Ok(())
    })
}
