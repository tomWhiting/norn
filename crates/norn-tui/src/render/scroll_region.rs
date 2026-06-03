//! Scroll-region write helpers.
//!
//! Content written through these helpers lands at the current cursor
//! position inside the DECSTBM scroll region established by the terminal
//! setup. The terminal confines the writes to the region and pushes
//! earlier lines into native scrollback. Writes are append-only — the
//! helpers never reposition the cursor or overwrite earlier content
//! (CO7: scroll region content is immutable once written).

use std::io;

use unicode_width::UnicodeWidthStr as _;

/// Box-drawing character used to pad separator lines.
const SEPARATOR_CHAR: char = '═';

/// Append `content` to the scroll region at the current cursor position.
///
/// In raw mode the kernel no longer translates `\n` into `\r\n`, so bare
/// newlines advance the cursor row without returning to column 0. This
/// helper replaces every `\n` that is not already preceded by `\r` with
/// `\r\n` before writing, ensuring lines start at column 0 in the scroll
/// region.
pub fn write_to_scroll<W: io::Write>(content: &str, writer: &mut W) -> io::Result<()> {
    let translated = content.replace('\n', "\r\n");
    writer.write_all(translated.as_bytes())
}

/// Append a full-width separator line centred on `label`.
///
/// The label is surrounded by a single space on each side and padded out
/// to `width` columns with box-drawing characters, e.g.
/// `════════ switched to: researcher ════════`. Display width is measured
/// with [`unicode_width`] so wide glyphs are accounted for. When `width`
/// is too small to fit the label and its padding, the label is written on
/// its own line. The line is terminated with a newline.
pub fn write_separator<W: io::Write>(label: &str, width: u16, writer: &mut W) -> io::Result<()> {
    let width = usize::from(width);
    let label_width = label.width();
    // Two columns are reserved for the spaces flanking the label.
    if width <= label_width + 2 {
        return write!(writer, "{label}\r\n");
    }
    let fill = width - label_width - 2;
    let left = fill / 2;
    let right = fill - left;
    let left_fill = SEPARATOR_CHAR.to_string().repeat(left);
    let right_fill = SEPARATOR_CHAR.to_string().repeat(right);
    write!(writer, "{left_fill} {label} {right_fill}\r\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn write_to_scroll_contains_content() {
        let mut buf: Vec<u8> = Vec::new();
        write_to_scroll("hello world", &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("hello world"));
    }

    #[test]
    fn write_to_scroll_is_append_only() {
        let mut buf: Vec<u8> = Vec::new();
        write_to_scroll("first\n", &mut buf).unwrap();
        write_to_scroll("second\n", &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "first\r\nsecond\r\n");
    }

    #[test]
    fn write_separator_is_full_width_and_centres_label() {
        let mut buf: Vec<u8> = Vec::new();
        write_separator("switched to: researcher", 50, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("switched to: researcher"));
        assert!(out.contains(SEPARATOR_CHAR));
        let line = out.trim_end_matches("\r\n");
        assert_eq!(line.width(), 50);
    }

    #[test]
    fn write_separator_narrow_width_falls_back_to_plain_label() {
        let mut buf: Vec<u8> = Vec::new();
        write_separator("long label here", 4, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "long label here\r\n");
    }
}
