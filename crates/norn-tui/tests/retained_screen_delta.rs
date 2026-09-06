//! Independent full-paint, changed-cell and geometry-epoch proofs for the PTY oracle.

#[path = "support/retained_screen.rs"]
pub mod retained_screen;

use std::fmt::Write as _;
use std::io;

use retained_screen::{FRAME_END, Screen, latest};
use unicode_width::UnicodeWidthStr as _;

fn synchronized(body: &str, rows: u16, column: u16) -> Vec<u8> {
    format!(
        "\x1b[?2026h\x1b[?25l{body}\x1b[0m\x1b[{};{column}H\x1b[?25h\x1b[?2026l",
        rows - 2
    )
    .into_bytes()
}

fn full(rows: u16, cols: u16, text: &str) -> io::Result<Vec<u8>> {
    let top = format!("───🮠 steer 🮣{}", "─".repeat(usize::from(cols) - 12));
    let metadata = format!("{}🮠 m 🮣───", "─".repeat(usize::from(cols) - 8));
    let mut body = String::new();
    for row in 1..=rows {
        let line = if row == 1 {
            text
        } else if row == rows - 3 {
            &top
        } else if row == rows - 1 {
            &metadata
        } else if row == rows {
            "^C exit"
        } else {
            ""
        };
        let remaining = usize::from(cols).checked_sub(line.width()).ok_or_else(|| {
            io::Error::other(format!("fixture row {row} exceeds requested width {cols}"))
        })?;
        write!(body, "\x1b[{row};1H\x1b[0m{line}{}", " ".repeat(remaining))
            .map_err(io::Error::other)?;
    }
    Ok(synchronized(&body, rows, 1))
}

#[test]
fn initial_full_paint_requires_every_cell_including_explicit_blanks() -> io::Result<()> {
    let raw = full(8, 32, "e\u{301}界👩‍💻!")?;
    assert!(!raw.windows(4).any(|bytes| bytes == b"\x1b[2J"));
    let screen = Screen::from_output(&raw, 8, 32)?;
    screen.assert_composer(1)?;
    assert_eq!(screen.lines()[0], "e\u{301}界👩‍💻!");
    assert_eq!(screen.cell(1, 0), Some("界"));
    assert_eq!(screen.cell(2, 0), Some(""));
    assert_eq!(screen.cell(3, 0), Some("👩‍💻"));
    assert_eq!(screen.cell(4, 0), Some(""));
    let missing = String::from_utf8(raw)
        .map_err(io::Error::other)?
        .replace(&format!("\x1b[2;1H\x1b[0m{}", " ".repeat(32)), "");
    assert!(Screen::from_output(missing.as_bytes(), 8, 32).is_err());
    Ok(())
}

#[test]
fn delta_preserves_untouched_cells_styles_and_full_width_composer() -> io::Result<()> {
    let mut output = full(8, 32, "persistent body")?;
    output.extend(synchronized("\x1b[2;1H\x1b[38;2;80;160;220mblue", 8, 1));
    let before = Screen::from_output(&output, 8, 32)?;
    output.extend(synchronized("\x1b[1;1H\x1b[0mP", 8, 1));
    let screen = Screen::from_output(&output, 8, 32)?;
    assert_eq!(screen.lines()[0], "Persistent body");
    assert_eq!(screen.lines()[1], "blue");
    for column in 0..4 {
        assert_eq!(screen.foreground_at(column, 1), Some([80, 160, 220]));
    }
    assert_eq!(screen.lines()[4..], before.lines()[4..]);
    screen.assert_composer(1)
}

#[test]
fn shortened_text_and_wide_glyphs_need_explicit_tail_erasure() -> io::Result<()> {
    let original = full(8, 32, "界suffix")?;
    let mut missing_tail = original.clone();
    missing_tail.extend(synchronized("\x1b[1;1H\x1b[0mx", 8, 1));
    assert!(Screen::from_output(&missing_tail, 8, 32).is_err());
    let mut output = original;
    output.extend(synchronized("\x1b[1;1H\x1b[0mx       ", 8, 1));
    let screen = Screen::from_output(&output, 8, 32)?;
    assert_eq!(screen.lines()[0], "x");
    for column in 1..8 {
        assert_eq!(screen.cell(column, 0), Some(" "));
    }
    screen.assert_composer(1)
}

#[test]
fn cursor_only_frame_and_partial_frame_preserve_the_committed_body() -> io::Result<()> {
    let mut output = full(8, 32, "unchanged")?;
    let before = Screen::from_output(&output, 8, 32)?;
    output.extend(synchronized("", 8, 4));
    let moved = Screen::from_output(&output, 8, 32)?;
    assert_eq!(moved.lines(), before.lines());
    assert_eq!(moved.cursor, (3, 5));
    assert!(moved.end_offset > before.end_offset);
    let incomplete = synchronized("\x1b[1;1Hpending", 8, 1);
    output.extend_from_slice(&incomplete[..incomplete.len() - FRAME_END.len()]);
    let retained = Screen::from_output(&output, 8, 32)?;
    assert_eq!(retained.lines(), moved.lines());
    assert_eq!(retained.end_offset, moved.end_offset);
    retained.assert_composer(1)
}

#[test]
fn queued_old_delta_keeps_old_epoch_until_complete_new_geometry_paint() -> io::Result<()> {
    let mut output = full(8, 32, "old geometry")?;
    // This delta fits either size. It cannot establish the newly requested cells.
    output.extend(synchronized("\x1b[1;1H\x1b[0mO", 8, 1));
    let old = latest(&output, &[(8, 32), (7, 24)])?
        .ok_or_else(|| io::Error::other("old screen missing"))?;
    assert_eq!((old.rows, old.cols), (8, 32));
    assert_eq!(old.lines()[0], "Old geometry");
    let resized = full(7, 24, "new geometry")?;
    output.extend_from_slice(&resized[..resized.len() - FRAME_END.len()]);
    let pending = latest(&output, &[(8, 32), (7, 24)])?
        .ok_or_else(|| io::Error::other("pending screen missing"))?;
    assert_eq!((pending.rows, pending.cols), (8, 32));
    output.extend_from_slice(FRAME_END);
    output.extend(synchronized("\x1b[1;1H\x1b[0mN", 7, 1));
    let current = latest(&output, &[(8, 32), (7, 24)])?
        .ok_or_else(|| io::Error::other("new screen missing"))?;
    assert_eq!((current.rows, current.cols), (7, 24));
    assert_eq!(current.lines()[0], "New geometry");
    assert!(!current.contains("Old geometry"));
    current.assert_composer(1)?;
    assert!(latest(&output, &[(7, 24)]).is_err());
    Ok(())
}

#[test]
fn resized_epoch_does_not_reuse_old_blank_coverage() -> io::Result<()> {
    let mut output = full(7, 24, "old")?;
    let incomplete = String::from_utf8(full(8, 32, "new")?)
        .map_err(io::Error::other)?
        .replace(&format!("\x1b[2;1H\x1b[0m{}", " ".repeat(32)), "");
    output.extend_from_slice(incomplete.as_bytes());
    assert!(latest(&output, &[(7, 24), (8, 32)]).is_err());
    Ok(())
}

#[test]
fn returning_to_previous_geometry_requires_a_new_complete_paint() -> io::Result<()> {
    let epochs = [(8, 32), (7, 24), (8, 32)];
    let mut output = full(8, 32, "initial wide")?;
    output.extend(full(7, 24, "narrow")?);
    output.extend(synchronized("\x1b[1;1H\x1b[0mN", 7, 1));
    let queued = latest(&output, &epochs)?
        .ok_or_else(|| io::Error::other("queued narrow screen missing"))?;
    assert_eq!((queued.rows, queued.cols), (7, 24));
    assert_eq!(queued.lines()[0], "Narrow");
    queued.assert_composer(1)?;

    let returning = full(8, 32, "returned wide")?;
    output.extend_from_slice(&returning[..returning.len() - FRAME_END.len()]);
    let pending = latest(&output, &epochs)?
        .ok_or_else(|| io::Error::other("pending return screen missing"))?;
    assert_eq!((pending.rows, pending.cols), (7, 24));
    assert_eq!(pending.end_offset, queued.end_offset);
    output.extend_from_slice(FRAME_END);
    output.extend(synchronized("\x1b[1;1H\x1b[0mR", 8, 1));
    let current = latest(&output, &epochs)?
        .ok_or_else(|| io::Error::other("returned wide screen missing"))?;
    assert_eq!((current.rows, current.cols), (8, 32));
    assert_eq!(current.lines()[0], "Returned wide");
    assert!(!current.contains("initial wide"));
    assert!(!current.contains("Narrow"));
    current.assert_composer(1)?;
    assert!(latest(&output, &epochs[..2]).is_err());
    Ok(())
}

#[test]
fn malformed_deltas_cannot_discard_the_last_valid_screen_checks() -> io::Result<()> {
    for malformed in [
        "\x1b[9;1Hbad",
        "\x1b[1;33Hbad",
        "\x1b[0;1Hbad",
        "\x1b[1;3r",
        "\x1b]52;c;injection\x07",
        "\u{301}",
        "\x1b[38;2;256;0;0mred",
    ] {
        let mut output = full(8, 32, "valid")?;
        output.extend(synchronized(malformed, 8, 1));
        assert!(
            Screen::from_output(&output, 8, 32).is_err(),
            "accepted malformed delta {malformed:?}"
        );
    }
    let mut filled = full(8, 32, "valid")?;
    filled.extend(synchronized("\x1b[6;1H\x1b[48;2;29;35;43mfilled", 8, 1));
    assert!(
        Screen::from_output(&filled, 8, 32)?
            .assert_composer(1)
            .is_err()
    );
    Ok(())
}
