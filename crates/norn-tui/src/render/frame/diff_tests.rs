//! Regression checks for exact changed spans, wide-cell ownership and publication failures.

use super::*;

fn frame(text: &str, columns: u16) -> Result<PreparedFrame, TuiError> {
    let mut frame = PreparedFrame::new(columns, 2, Some((0, 1)));
    for (column, byte) in text.bytes().enumerate() {
        let column = u16::try_from(column).map_err(|source| TuiError::FrameCoordinate {
            value: column,
            source,
        })?;
        frame.put(column, 0, 1, &[byte])?;
    }
    Ok(frame)
}

#[test]
fn repeated_frame_writes_nothing_and_single_character_change_stays_local() -> Result<(), TuiError> {
    let old = frame("ABCD", 8)?;
    assert!(old.encode_delta(Some(&old))?.is_empty());
    let new = frame("ABXD", 8)?;
    assert_eq!(
        new.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;3HX\x1b[0m\x1b[2;1H\x1b[?25h"
    );
    let initial = new.encode_delta(None)?;
    assert!(!initial.windows(4).any(|bytes| bytes == b"\x1b[2J"));
    assert!(!initial.contains(&b'\n'));
    Ok(())
}

#[test]
fn removed_tail_is_blanked_without_repainting_unchanged_prefix() -> Result<(), TuiError> {
    let old = frame("ABCDEF", 8)?;
    let new = frame("AB", 8)?;
    assert_eq!(
        new.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;3H\x1b[0m    \x1b[0m\x1b[2;1H\x1b[?25h"
    );
    Ok(())
}

#[test]
fn cursor_only_changes_emit_no_cell_paint() -> Result<(), TuiError> {
    let old = frame("ABCD", 8)?;
    let mut new = frame("ABCD", 8)?;
    new.cursor = Some((3, 1));
    assert_eq!(new.encode_delta(Some(&old))?, b"\x1b[0m\x1b[2;4H\x1b[?25h");
    new.cursor = None;
    assert_eq!(new.encode_delta(Some(&old))?, b"\x1b[0m\x1b[?25l");
    Ok(())
}

#[test]
fn width_and_height_changes_repaint_visible_cells_without_screen_clear() -> Result<(), TuiError> {
    let old = frame("ABCD", 8)?;
    for new in [frame("ABCD", 6)?, PreparedFrame::new(8, 3, None)] {
        let resized = new.encode_delta(Some(&old))?;
        assert_eq!(resized, new.encode_delta(None)?);
        assert!(!resized.windows(4).any(|bytes| bytes == b"\x1b[2J"));
    }
    assert!(
        PreparedFrame::new(0, 0, None)
            .encode_delta(Some(&old))?
            .is_empty()
    );
    Ok(())
}

#[test]
fn wide_glyph_replacement_and_overlays_never_leave_half_a_glyph() -> Result<(), TuiError> {
    let mut old = frame("ABCDE", 8)?;
    old.put(1, 0, 2, "👩‍💻".as_bytes())?;
    let new = frame("AXCDE", 8)?;
    assert_eq!(
        new.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;2HXC\x1b[0m\x1b[2;1H\x1b[?25h"
    );
    let mut overlay = frame("ABCDE", 8)?;
    overlay.put(1, 0, 2, "👩‍💻".as_bytes())?;
    overlay.put(2, 0, 1, b"Z")?;
    assert_eq!(overlay.cells[0][1], Cell::Blank);
    assert_eq!(
        overlay.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;2H\x1b[0m Z\x1b[0m\x1b[2;1H\x1b[?25h"
    );
    Ok(())
}

#[test]
fn changed_style_with_identical_text_is_a_real_cell_update() -> Result<(), TuiError> {
    let old = frame("A", 4)?;
    let mut new = frame("A", 4)?;
    new.put(0, 0, 1, b"\x1b[1mA")?;
    assert_eq!(
        new.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;1H\x1b[1mA\x1b[0m\x1b[2;1H\x1b[?25h"
    );
    Ok(())
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    writes: usize,
    flushes: usize,
    fail_write: bool,
    fail_flush: bool,
}

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.fail_write && self.writes == 1 {
            return Err(io::Error::other("write failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush && self.flushes == 1 {
            return Err(io::Error::other("flush failure"));
        }
        Ok(())
    }
}

#[test]
fn supported_sync_is_one_assembled_write_and_one_flush() -> io::Result<()> {
    let mut writer = Writer::default();
    publish(&mut writer, b"BODY", true)?;
    assert_eq!(writer.bytes, b"\x1b[?2026hBODY\x1b[?2026l");
    assert_eq!(writer.writes, 1);
    assert_eq!(writer.flushes, 1);
    publish(&mut writer, b"", true)?;
    assert_eq!(writer.writes, 1);
    assert_eq!(writer.flushes, 1);
    Ok(())
}

#[test]
fn unsupported_sync_has_no_mode_toggle_and_empty_update_does_no_io() -> io::Result<()> {
    let mut writer = Writer::default();
    publish(&mut writer, b"BODY", false)?;
    assert_eq!(writer.bytes, b"BODY");
    assert_eq!(writer.writes, 1);
    assert_eq!(writer.flushes, 1);
    Ok(())
}

#[test]
fn write_and_flush_failure_recover_sync_and_preserve_original_error()
-> Result<(), Box<dyn std::error::Error>> {
    for (fail_write, fail_flush, message) in [
        (true, false, "write failure"),
        (false, true, "flush failure"),
    ] {
        let mut writer = Writer {
            fail_write,
            fail_flush,
            ..Writer::default()
        };
        let error = publish(&mut writer, b"BODY", true)
            .err()
            .ok_or("publication unexpectedly succeeded")?;
        assert_eq!(error.to_string(), message);
        assert!(writer.bytes.ends_with(b"\x1b[?2026l\x1b[0m\x1b[?25h"));
        assert_eq!(writer.writes, 2);
        assert_eq!(writer.flushes, if fail_flush { 2 } else { 1 });
    }
    Ok(())
}

#[test]
fn arena_offsets_and_unchanged_cells_between_edits_are_not_painted() -> Result<(), TuiError> {
    let old = frame("ABCDE", 8)?;
    let mut same = frame("ABCDE", 8)?;
    same.put(0, 0, 1, b"temporary")?;
    same.put(0, 0, 1, b"A")?;
    assert!(same.encode_delta(Some(&old))?.is_empty());
    let new = frame("XBCDY", 8)?;
    assert_eq!(
        new.encode_delta(Some(&old))?,
        b"\x1b[?25l\x1b[1;1HX\x1b[1;5HY\x1b[0m\x1b[2;1H\x1b[?25h"
    );
    Ok(())
}

#[test]
fn failed_publication_never_commits_prepared_baseline() -> Result<(), Box<dyn std::error::Error>> {
    for fail_flush in [false, true] {
        let mut baseline = Some(frame("OLD", 8)?);
        let mut writer = Writer {
            fail_write: !fail_flush,
            fail_flush,
            ..Writer::default()
        };
        assert!(
            frame("NEW", 8)?
                .publish(&mut baseline, &mut writer, true)
                .is_err()
        );
        assert!(frame("OLD", 8)?.encode_delta(baseline.as_ref())?.is_empty());
        let mut success = Writer::default();
        frame("NEW", 8)?.publish(&mut baseline, &mut success, true)?;
        assert!(frame("NEW", 8)?.encode_delta(baseline.as_ref())?.is_empty());
    }
    Ok(())
}
