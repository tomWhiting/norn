//! Actual-App composer keys, original-byte admission, active editing and terminal resizing.

#[path = "support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/iridium_composer.rs"]
mod support;
#[path = "support/retained_workspace.rs"]
pub mod workspace_support;

use serde_json::json;
use support::{draft, edit, newline, paste, plain, submit};
use workspace_support::{TestResult, Workspace, with_composer, with_composer_keyboard};

#[test]
fn retained_workspace_child_entrypoint() -> TestResult {
    workspace_support::child_entrypoint()
}

#[test]
fn enter_send_runs_idle_and_active_kernel_edits_without_duplicate_admission() -> TestResult {
    editing("enter")
}

#[test]
fn alt_enter_launch_preserves_bare_newline_and_exact_original_admission() -> TestResult {
    editing("alt-enter")
}

#[test]
fn shift_enter_launch_preserves_bare_newline_and_exact_original_admission() -> TestResult {
    editing("shift-enter")
}

fn editing(send_key: &str) -> TestResult {
    with_composer(send_key, |app| {
        plain(&app.screen()?, &[""])?;
        let idle = app.snapshot()?;
        assert_eq!(idle["provider_calls"], 0);
        assert_eq!(idle["user_events"], json!([]));

        let pair = edit(app, b"{", &["{}"])?;
        assert_eq!(
            pair.cursor.0, 1,
            "opening brace did not leave caret inside its pair"
        );
        let indented = edit(app, newline(send_key), &["{", "", "}"])?;
        assert_eq!(
            indented.cursor.0, 4,
            "kernel default indentation was not used"
        );
        edit(app, b"work", &["{", "    work", "}"])?;
        edit(app, b"\x1b", &[""])?;
        edit(app, b"\x1a", &["{", "    work", "}"])?;
        edit(app, b"\x19", &[""])?;
        assert_eq!(
            app.snapshot()?,
            idle,
            "newlines/undo admitted a provider turn"
        );

        let original = "e\u{301} 🇦🇺 👩‍💻\r\nsecond";
        paste(app, original, &["e\u{301} 🇦🇺 👩‍💻", "second"])?;
        edit(app, b"\x1a", &[""])?;
        edit(app, b"\x19", &["e\u{301} 🇦🇺 👩‍💻", "second"])?;
        assert_eq!(
            app.snapshot()?,
            idle,
            "Unicode paste/undo changed accepted history"
        );
        submit(app, original)?;
        let admitted = app.snapshot()?;

        edit(app, b"scratch", &["scratch"])?;
        edit(app, b"\x1b[A", &["e\u{301} 🇦🇺 👩‍💻", "second"])?;
        edit(app, b"\x1b[B", &["scratch"])?;
        edit(app, b"\x1b", &[""])?;
        paste(app, "A👩‍💻", &["A👩‍💻"])?;
        edit(app, b"\x7f", &["A"])?;
        edit(app, b"\x1a", &["A👩‍💻"])?;
        // Shift+Left selects one entire cluster; Delete must not leave a ZWJ fragment.
        app.input(b"\x1b[1;2D", |screen| screen.cursor.0 == 1)?;
        edit(app, b"\x1b[3~", &["A"])?;
        app.input(b"\x1a", |screen| draft(screen) == ["A👩‍💻"])?;
        assert_eq!(
            app.snapshot()?,
            admitted,
            "active editor motion/recall resubmitted text"
        );
        Ok(original.to_owned())
    })
}

#[test]
fn active_paste_and_selection_survive_wide_narrow_tiny_wide_resize() -> TestResult {
    with_composer("enter", |app| {
        edit(app, b"resize fixture", &["resize fixture"])?;
        submit(app, "resize fixture")?;
        let admitted = app.snapshot()?;
        let original = "left e\u{301} 🇦🇺 👩‍💻 right\nsecond line";
        paste(app, original, &["left e\u{301} 🇦🇺 👩‍💻 right", "second line"])?;
        let narrow = app.resize(8, 8)?;
        assert!(narrow.cursor_visible && narrow.cursor.0 < 8);
        let tiny = app.resize(1, 4)?;
        tiny.assert_composer(1)?;
        assert_eq!(tiny.cursor.0, 0);
        // Edit the active draft at tiny geometry, then recover all original text at wide geometry.
        app.input(b"!", |screen| screen.cell(0, screen.cursor.1) == Some("!"))?;
        let wide = app.resize(100, 24)?;
        plain(&wide, &["left e\u{301} 🇦🇺 👩‍💻 right", "second line!"])?;
        edit(app, b"\x1a", &["left e\u{301} 🇦🇺 👩‍💻 right", "second line"])?;
        let screen = app.input(b"\x1b[1;2D", |screen| screen.cursor.0 == 10)?;
        let selected = draft(&screen);
        app.resize(8, 8)?;
        app.resize(100, 24)?;
        edit(
            app,
            b"\x1b[3~",
            &["left e\u{301} 🇦🇺 👩‍💻 right", "second lin"],
        )?;
        assert_eq!(selected, ["left e\u{301} 🇦🇺 👩‍💻 right", "second line"]);
        assert_eq!(
            app.snapshot()?,
            admitted,
            "resize/edit/selection admitted another turn"
        );
        Ok("resize fixture".to_owned())
    })
}

#[test]
fn local_command_rejection_keeps_original_draft_and_popup_acceptance_stays_local() -> TestResult {
    with_composer("enter", |app| {
        let idle = app.snapshot()?;
        paste(
            app,
            "/view invalid-composer-fixture",
            &["/view invalid-composer-fixture"],
        )?;
        let refused = app.input(b"\r", |screen| {
            screen.contains("Unknown view action or arguments")
                && draft(screen) == ["/view invalid-composer-fixture"]
        })?;
        plain(&refused, &["/view invalid-composer-fixture"])?;
        assert_eq!(app.snapshot()?, idle);
        edit(app, b"\x1b", &[""])?;
        // A slash popup owns Enter first. Completing /view must not dispatch it or call the provider.
        app.input(b"/vie", |screen| screen.contains("/view"))?;
        let completed = edit(app, b"\r", &["/view"])?;
        assert_eq!(
            completed.cursor.0, 5,
            "popup acceptance caret differs from the completed name"
        );
        assert_eq!(app.snapshot()?, idle);
        edit(app, b"\x1b", &[""])?;
        edit(app, b"single accepted input", &["single accepted input"])?;
        submit(app, "single accepted input")?;
        Ok("single accepted input".to_owned())
    })
}

#[test]
fn actual_composer_mouse_hit_edits_the_displayed_wide_cluster_boundary() -> TestResult {
    with_composer("enter", |app| {
        let idle = app.snapshot()?;
        let screen = paste(app, "a界b", &["a界b"])?;
        let row = screen
            .composer_rows()
            .first()
            .copied()
            .ok_or("composer row missing")?
            + 1;
        // Cell four is the start of b, after the two-cell CJK cluster. Coordinates
        // come from the actual frame; the hit is not a logical character index.
        app.input(format!("\x1b[<0;4;{row}M").as_bytes(), |screen| {
            screen.cursor.0 == 3
        })?;
        edit(app, format!("\x1b[<0;4;{row}mX").as_bytes(), &["a界Xb"])?;
        assert_eq!(
            app.snapshot()?,
            idle,
            "pointer editing admitted runtime input"
        );
        submit(app, "a界Xb")?;
        Ok("a界Xb".to_owned())
    })
}

#[test]
fn shift_send_recovery_preserves_idle_and_active_drafts_with_confirmed_reporting() -> TestResult {
    shift_recovery(true)
}

#[test]
fn shift_send_recovery_preserves_idle_and_active_drafts_when_reporting_is_unconfirmed() -> TestResult
{
    shift_recovery(false)
}

fn shift_label(confirmed: bool) -> &'static str {
    if confirmed {
        "[Shift+Enter sends]"
    } else {
        "[Shift+Enter unconfirmed]"
    }
}

fn shift_recovery(confirmed: bool) -> TestResult {
    with_composer_keyboard("shift-enter", confirmed, |app| {
        let initial = app.screen()?;
        assert!(initial.contains(shift_label(confirmed)));
        assert!(initial.contains("Enter newline"));
        assert!(initial.contains("F10 send key"));
        let idle = app.snapshot()?;
        let original = recover_draft(app, "idle", confirmed)?;
        assert_eq!(
            app.snapshot()?,
            idle,
            "recovery/newline gestures submitted idle input"
        );
        submit(app, &original)?;
        let admitted = app.snapshot()?;
        recover_draft(app, "active", confirmed)?;
        assert_eq!(
            app.snapshot()?,
            admitted,
            "recovery/newline gestures submitted active input"
        );
        edit(app, b"\x1b", &[""])?;
        paste(app, "/view status", &["/view status"])?;
        let status = app.input(b"\x1b[13;2u", |screen| {
            draft(screen) == [""] && screen.contains("Composer send key: shift-enter")
        })?;
        plain(&status, &[""])?;
        assert_eq!(
            app.snapshot()?,
            admitted,
            "active local Shift submission admitted provider input"
        );
        Ok(original)
    })
}

fn recover_draft(app: &mut Workspace, prefix: &str, confirmed: bool) -> TestResult<String> {
    let original = format!("{prefix}🙂");
    paste(app, &original, &[&original])?;
    let selected = app.input(b"\x1b[D", |screen| screen.cursor.0 == prefix.len())?;
    let cursor = selected.cursor;
    let alternate = app.input(b"\x1b[21~", |screen| screen.contains("[Alt+Enter sends]"))?;
    plain(&alternate, &[&original])?;
    assert_eq!(alternate.cursor, cursor, "F10 moved the kernel caret");
    let button_row = alternate
        .lines()
        .iter()
        .position(|line| line.starts_with("[Alt+Enter sends]"))
        .ok_or("actual send button missing")?
        + 1;
    let entered = app.input(
        format!("\x1b[<0;1;{button_row}M\x1b[<0;1;{button_row}m").as_bytes(),
        |screen| screen.contains("[Enter sends]"),
    )?;
    plain(&entered, &[&original])?;
    assert_eq!(
        entered.cursor, cursor,
        "send-policy click moved the kernel caret"
    );
    let restored = app.input(b"\x1b[21~", |screen| {
        screen.contains(shift_label(confirmed))
    })?;
    plain(&restored, &[&original])?;
    assert_eq!(restored.cursor, cursor);
    // The original paste remains the preceding undo transaction after both controls.
    edit(app, b"\x1a", &[""])?;
    edit(app, b"\x19", &[&original])?;
    app.input(b"\x1b[D", |screen| screen.cursor.0 == prefix.len())?;
    let edited = format!("{prefix}X🙂");
    edit(app, b"X", &[&edited])?;
    edit(app, b"\x1a", &[&original])?;
    edit(app, b"\x19", &[&edited])?;
    let first_line = format!("{prefix}X");
    edit(app, b"\r", &[&first_line, "🙂"])?;
    edit(app, b"\x1a", &[&edited])?;
    Ok(edited)
}
