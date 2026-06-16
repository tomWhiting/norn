//! Width-aware visual line layout for the multi-line input editor.
//!
//! Logical lines (the `Vec<String>` the editor owns) map 1:N onto visual
//! rows when a logical line's display width exceeds the terminal width.
//! [`layout`] computes this mapping on demand from the current buffer and
//! cursor state — no persistent wrap state is cached.

use crate::render::text::input_display_width;

/// One visual row in the wrapped layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualRow {
    /// Index into the editor's `lines` vec.
    pub logical_row: usize,
    /// Inclusive character start within the logical line.
    pub char_start: usize,
    /// Exclusive character end within the logical line.
    pub char_end: usize,
}

/// Cursor position in visual (wrapped) coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualCursor {
    /// Index into [`WrapLayout::rows`].
    pub visual_row: usize,
    /// Display column within the visual row (accounts for wide chars).
    pub display_col: u16,
}

/// Complete wrap layout for a buffer at a given terminal width.
#[derive(Clone, Debug)]
pub struct WrapLayout {
    /// Visual rows in display order.
    pub rows: Vec<VisualRow>,
    /// Cursor position in visual coordinates.
    pub cursor: VisualCursor,
}

/// Compute the visual layout for `lines` at the given terminal `width`.
///
/// Each logical line is split into one or more visual rows such that no
/// visual row exceeds `width` display columns. Wide characters (CJK,
/// emoji) that would straddle the wrap boundary are moved to the next
/// visual row rather than being split.
///
/// The cursor's visual position is computed from `cursor_row` and
/// `cursor_col` (both character indices, not byte offsets).
#[must_use]
pub fn layout(lines: &[String], cursor_row: usize, cursor_col: usize, width: u16) -> WrapLayout {
    let effective_width = width.max(1) as usize;
    let mut rows = Vec::new();
    let mut cursor_visual = VisualCursor {
        visual_row: 0,
        display_col: 0,
    };

    for (logical_idx, line) in lines.iter().enumerate() {
        let visual_start = rows.len();
        let mut char_start = 0;
        let mut col = 0usize;
        let mut char_idx = 0;

        for ch in line.chars() {
            let ch_width = input_display_width(ch);

            if col + ch_width > effective_width && col > 0 {
                rows.push(VisualRow {
                    logical_row: logical_idx,
                    char_start,
                    char_end: char_idx,
                });
                char_start = char_idx;
                col = 0;
            }

            if logical_idx == cursor_row && char_idx == cursor_col {
                cursor_visual = VisualCursor {
                    visual_row: rows.len(),
                    display_col: u16::try_from(col).unwrap_or(u16::MAX),
                };
            }

            col += ch_width;
            char_idx += 1;
        }

        rows.push(VisualRow {
            logical_row: logical_idx,
            char_start,
            char_end: char_idx,
        });

        if logical_idx == cursor_row && cursor_col >= char_idx {
            cursor_visual = VisualCursor {
                visual_row: visual_start + rows.len() - 1 - visual_start,
                display_col: u16::try_from(col).unwrap_or(u16::MAX),
            };
        }
    }

    if rows.is_empty() {
        rows.push(VisualRow {
            logical_row: 0,
            char_start: 0,
            char_end: 0,
        });
    }

    WrapLayout {
        rows,
        cursor: cursor_visual,
    }
}

/// Compute the display width of the character-slice `[char_start..char_end)`
/// within `line`.
#[must_use]
pub fn visual_row_width(line: &str, char_start: usize, char_end: usize) -> u16 {
    let width: usize = line
        .chars()
        .skip(char_start)
        .take(char_end.saturating_sub(char_start))
        .map(input_display_width)
        .sum();
    u16::try_from(width).unwrap_or(u16::MAX)
}

/// Map a display column within a visual row back to a character index
/// within the logical line. Used by sticky-column cursor navigation.
///
/// Walks characters from `char_start`, accumulating display width. Returns
/// the character index of the last character whose start column is `<=
/// target_display_col`. Clamps to `char_end - 1` when the target exceeds
/// the row width.
#[must_use]
pub fn display_col_to_char(
    line: &str,
    char_start: usize,
    char_end: usize,
    target_display_col: u16,
) -> usize {
    let target = target_display_col as usize;
    let mut col = 0usize;

    for (i, ch) in line.chars().enumerate().skip(char_start) {
        if i >= char_end {
            break;
        }
        if col == target {
            return i;
        }
        let ch_width = input_display_width(ch);
        if col + ch_width > target {
            return i;
        }
        col += ch_width;
    }
    char_end.min(line.chars().count())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn single_short_line() {
        let lines = vec!["hello".to_owned()];
        let w = layout(&lines, 0, 3, 80);
        assert_eq!(w.rows.len(), 1);
        assert_eq!(
            w.rows[0],
            VisualRow {
                logical_row: 0,
                char_start: 0,
                char_end: 5
            }
        );
        assert_eq!(w.cursor.visual_row, 0);
        assert_eq!(w.cursor.display_col, 3);
    }

    #[test]
    fn line_wraps_at_width() {
        let lines = vec!["abcdefghij".to_owned()];
        let w = layout(&lines, 0, 7, 5);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(
            w.rows[0],
            VisualRow {
                logical_row: 0,
                char_start: 0,
                char_end: 5
            }
        );
        assert_eq!(
            w.rows[1],
            VisualRow {
                logical_row: 0,
                char_start: 5,
                char_end: 10
            }
        );
        assert_eq!(w.cursor.visual_row, 1);
        assert_eq!(w.cursor.display_col, 2);
    }

    #[test]
    fn cjk_wide_char_wraps_to_next_row() {
        let lines = vec!["abcd\u{4e16}".to_owned()];
        let w = layout(&lines, 0, 4, 5);
        assert_eq!(w.rows.len(), 2, "CJK char should wrap to next row");
        assert_eq!(w.rows[0].char_end, 4);
        assert_eq!(w.rows[1].char_start, 4);
        assert_eq!(w.cursor.visual_row, 1);
        assert_eq!(w.cursor.display_col, 0);
    }

    #[test]
    fn emoji_display_width() {
        let lines = vec!["\u{1f600}\u{1f600}\u{1f600}".to_owned()];
        let w = layout(&lines, 0, 2, 5);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0].char_end, 2);
    }

    #[test]
    fn control_characters_occupy_placeholder_columns() {
        let lines = vec!["a\x1bb\tc".to_owned()];
        let w = layout(&lines, 0, 3, 3);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0].char_end, 3);
        assert_eq!(w.rows[1].char_start, 3);
        assert_eq!(w.cursor.visual_row, 1);
        assert_eq!(w.cursor.display_col, 0);
    }

    #[test]
    fn empty_line_produces_one_visual_row() {
        let lines = vec![String::new()];
        let w = layout(&lines, 0, 0, 80);
        assert_eq!(w.rows.len(), 1);
        assert_eq!(
            w.rows[0],
            VisualRow {
                logical_row: 0,
                char_start: 0,
                char_end: 0
            }
        );
    }

    #[test]
    fn multiple_logical_lines() {
        let lines = vec!["abc".to_owned(), "defgh".to_owned()];
        let w = layout(&lines, 1, 2, 80);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0].logical_row, 0);
        assert_eq!(w.rows[1].logical_row, 1);
        assert_eq!(w.cursor.visual_row, 1);
        assert_eq!(w.cursor.display_col, 2);
    }

    #[test]
    fn cursor_at_end_of_line() {
        let lines = vec!["abc".to_owned()];
        let w = layout(&lines, 0, 3, 80);
        assert_eq!(w.cursor.display_col, 3);
    }

    #[test]
    fn cursor_at_end_of_wrapped_line() {
        let lines = vec!["abcde".to_owned()];
        let w = layout(&lines, 0, 5, 3);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.cursor.visual_row, 1);
        assert_eq!(w.cursor.display_col, 2);
    }

    #[test]
    fn width_one_wraps_every_char() {
        let lines = vec!["abc".to_owned()];
        let w = layout(&lines, 0, 2, 1);
        assert_eq!(w.rows.len(), 3);
        assert_eq!(w.cursor.visual_row, 2);
        assert_eq!(w.cursor.display_col, 0);
    }

    #[test]
    fn display_col_to_char_ascii() {
        let line = "hello world";
        assert_eq!(display_col_to_char(line, 0, 11, 5), 5);
        assert_eq!(display_col_to_char(line, 0, 11, 0), 0);
    }

    #[test]
    fn display_col_to_char_cjk() {
        let line = "a\u{4e16}\u{754c}b";
        assert_eq!(display_col_to_char(line, 0, 4, 1), 1);
        assert_eq!(display_col_to_char(line, 0, 4, 3), 2);
    }

    #[test]
    fn visual_row_width_basic() {
        assert_eq!(visual_row_width("hello", 0, 5), 5);
        assert_eq!(visual_row_width("hello", 2, 5), 3);
    }

    #[test]
    fn visual_row_width_cjk() {
        let line = "\u{4e16}\u{754c}";
        assert_eq!(visual_row_width(line, 0, 2), 4);
        assert_eq!(visual_row_width(line, 1, 2), 2);
    }

    #[test]
    fn empty_input_produces_one_row() {
        let lines: Vec<String> = Vec::new();
        let w = layout(&lines, 0, 0, 80);
        assert_eq!(w.rows.len(), 1);
    }

    #[test]
    fn exact_width_does_not_wrap() {
        let lines = vec!["abcde".to_owned()];
        let w = layout(&lines, 0, 5, 5);
        assert_eq!(w.rows.len(), 1);
    }

    #[test]
    fn triple_width_wrap() {
        let lines = vec!["abcdefghijklmno".to_owned()];
        let w = layout(&lines, 0, 14, 5);
        assert_eq!(w.rows.len(), 3);
        assert_eq!(w.cursor.visual_row, 2);
        assert_eq!(w.cursor.display_col, 4);
    }
}
