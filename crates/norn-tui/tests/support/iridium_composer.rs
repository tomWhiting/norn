//! Composer gestures observed through the real App's PTY, never a private editor setter.

use std::io;

use crate::retained_screen::Screen;
use crate::workspace_support::Workspace;

/// Original input rows only; terminal decoration is deliberately excluded.
pub fn draft(screen: &Screen) -> Vec<String> {
    let lines = screen.lines();
    screen
        .composer_rows()
        .iter()
        .map(|row| lines[*row].clone())
        .collect()
}

/// Require original framing, terminal-owned caret and uniform default text colour.
pub fn plain(screen: &Screen, expected: &[&str]) -> io::Result<()> {
    screen.assert_composer(expected.len())?;
    if draft(screen) != expected {
        return Err(io::Error::other(format!(
            "composer bytes differ: expected {expected:?}; screen:\n{}",
            screen.debug_text()
        )));
    }
    for row in screen.composer_rows() {
        for column in 0..usize::from(screen.cols) {
            if screen.foreground_at(column, row).is_some() {
                return Err(io::Error::other(format!(
                    "composer foreground changed at {column},{row}"
                )));
            }
        }
    }
    Ok(())
}

/// A key/paste is acknowledged by its new complete current frame, not historical output.
pub fn edit(app: &mut Workspace, bytes: &[u8], expected: &[&str]) -> io::Result<Screen> {
    let screen = app.input(bytes, |screen| draft(screen) == expected)?;
    plain(&screen, expected)?;
    Ok(screen)
}

/// Bracketed paste preserves one original payload and one undo gesture.
pub fn paste(app: &mut Workspace, original: &str, expected: &[&str]) -> io::Result<Screen> {
    edit(
        app,
        format!("\x1b[200~{original}\x1b[201~").as_bytes(),
        expected,
    )
}

/// The alternate physical key inserts a newline under the selected launch policy.
pub fn newline(send_key: &str) -> &'static [u8] {
    if send_key == "enter" {
        b"\x1b\r"
    } else {
        b"\r"
    }
}

/// Real publication, exact accepted bytes and provider count acknowledge submission.
pub fn submit(app: &mut Workspace, original: &str) -> io::Result<()> {
    let screen = app.input(app.submit_key(), |screen| {
        screen.contains("workspace provider held") && draft(screen) == [""]
    })?;
    plain(&screen, &[""])?;
    let census = app.snapshot()?;
    if census["provider_calls"] != 1 || census["user_events"] != serde_json::json!([original]) {
        return Err(io::Error::other(format!(
            "actual admission differs from original draft: {census}"
        )));
    }
    Ok(())
}
