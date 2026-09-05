//! Actual retained-App PTY reading controls; emitted clipboard bytes are not clipboard acceptance.
//! No history/body demand counts are claimed: the App currently exposes no such observer.

#[path = "support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/retained_workspace.rs"]
pub mod support;

use support::{TestResult, with_workspace};

#[test]
fn retained_workspace_child_entrypoint() -> TestResult {
    support::child_entrypoint()
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
