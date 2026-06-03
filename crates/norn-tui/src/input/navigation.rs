//! Cursor movement and editing mutations for [`super::editor::InputEditor`].
//!
//! Split out of editor.rs to keep both files under the 500-line production
//! code limit (CO3). Methods here mutate the editor's buffer and cursor
//! state; render and state-query methods stay in editor.rs.

use super::editor::InputEditor;
use super::wrap;

impl InputEditor {
    /// Insert a newline at the cursor (Shift+Enter / Alt+Enter).
    pub fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let split_at = Self::byte_index(line, self.cursor_col);
        let tail = line.split_off(split_at);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// Insert `ch` at the cursor and advance the cursor one column.
    pub fn insert_char(&mut self, ch: char) {
        let line = &mut self.lines[self.cursor_row];
        let at = Self::byte_index(line, self.cursor_col);
        line.insert(at, ch);
        self.cursor_col += 1;
    }

    /// Delete the character before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let at = Self::byte_index(line, self.cursor_col - 1);
            line.remove(at);
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&line);
        }
    }

    /// Delete the character at the cursor (Delete).
    pub fn delete(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let at = Self::byte_index(line, self.cursor_col);
            line.remove(at);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    /// Move the cursor one column left, wrapping to the previous line.
    pub fn cursor_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    /// Move the cursor one column right, wrapping to the next line.
    pub fn cursor_right(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// Whether the cursor is on the first visual line at the given width.
    #[must_use]
    pub fn cursor_on_first_visual_line(&self, width: u16) -> bool {
        let w = wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        w.cursor.visual_row == 0
    }

    /// Whether the cursor is on the last visual line at the given width.
    #[must_use]
    pub fn cursor_on_last_visual_line(&self, width: u16) -> bool {
        let w = wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        w.cursor.visual_row + 1 >= w.rows.len()
    }

    /// Move the cursor one visual line up, preserving the display column.
    pub fn visual_cursor_up(&mut self, width: u16) {
        let w = wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        if w.cursor.visual_row == 0 {
            return;
        }
        let target_row = &w.rows[w.cursor.visual_row - 1];
        let target_char = wrap::display_col_to_char(
            &self.lines[target_row.logical_row],
            target_row.char_start,
            target_row.char_end,
            w.cursor.display_col,
        );
        self.cursor_row = target_row.logical_row;
        self.cursor_col = target_char;
    }

    /// Move the cursor one visual line down, preserving the display column.
    pub fn visual_cursor_down(&mut self, width: u16) {
        let w = wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        if w.cursor.visual_row + 1 >= w.rows.len() {
            return;
        }
        let target_row = &w.rows[w.cursor.visual_row + 1];
        let target_char = wrap::display_col_to_char(
            &self.lines[target_row.logical_row],
            target_row.char_start,
            target_row.char_end,
            w.cursor.display_col,
        );
        self.cursor_row = target_row.logical_row;
        self.cursor_col = target_char;
    }

    /// Move the cursor left by one word (`is_alphanumeric` + `_`).
    pub fn word_left(&mut self) {
        if self.cursor_col == 0 && self.cursor_row == 0 {
            return;
        }
        if self.cursor_col == 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            return;
        }
        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();
        let mut pos = self.cursor_col;
        while pos > 0 && !is_word_char(chars[pos - 1]) {
            pos -= 1;
        }
        while pos > 0 && is_word_char(chars[pos - 1]) {
            pos -= 1;
        }
        self.cursor_col = pos;
    }

    /// Move the cursor right by one word (`is_alphanumeric` + `_`).
    pub fn word_right(&mut self) {
        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        if self.cursor_col >= len {
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            }
            return;
        }
        let mut pos = self.cursor_col;
        while pos < len && is_word_char(chars[pos]) {
            pos += 1;
        }
        while pos < len && !is_word_char(chars[pos]) {
            pos += 1;
        }
        self.cursor_col = pos;
    }

    /// Move cursor to the start of the current logical line.
    pub fn line_start(&mut self) {
        self.cursor_col = 0;
    }

    /// Move cursor to the end of the current logical line.
    pub fn line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    /// Move cursor to the start of the buffer.
    pub fn buffer_start(&mut self) {
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Move cursor to the end of the buffer.
    pub fn buffer_end(&mut self) {
        self.cursor_row = self.lines.len().saturating_sub(1);
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    /// Delete the word before the cursor (Option+Backspace).
    pub fn delete_word_back(&mut self) {
        if self.cursor_col == 0 && self.cursor_row == 0 {
            return;
        }
        if self.cursor_col == 0 {
            self.backspace();
            return;
        }
        let save_col = self.cursor_col;
        self.word_left();
        let delete_from = self.cursor_col;
        let line = &mut self.lines[self.cursor_row];
        let byte_start = Self::byte_index(line, delete_from);
        let byte_end = Self::byte_index(line, save_col);
        line.replace_range(byte_start..byte_end, "");
    }

    /// Delete the word after the cursor (Option+Delete).
    pub fn delete_word_forward(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col >= line_len && self.cursor_row + 1 >= self.lines.len() {
            return;
        }
        if self.cursor_col >= line_len {
            self.delete();
            return;
        }
        let save_row = self.cursor_row;
        let save_col = self.cursor_col;
        self.word_right();
        let end_col = self.cursor_col;
        self.cursor_row = save_row;
        self.cursor_col = save_col;
        let line = &mut self.lines[self.cursor_row];
        let byte_start = Self::byte_index(line, save_col);
        let byte_end = Self::byte_index(line, end_col);
        line.replace_range(byte_start..byte_end, "");
    }

    /// Delete from cursor to start of current line (Command+Backspace).
    pub fn delete_to_line_start(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        let line = &mut self.lines[self.cursor_row];
        let byte_end = Self::byte_index(line, self.cursor_col);
        line.replace_range(..byte_end, "");
        self.cursor_col = 0;
    }

    /// Delete from cursor to end of current line (Command+Delete / Ctrl+K).
    pub fn delete_to_line_end(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let line_len = line.chars().count();
        if self.cursor_col >= line_len {
            return;
        }
        let byte_start = Self::byte_index(line, self.cursor_col);
        line.truncate(byte_start);
    }

    /// Adjust `viewport_top` so the cursor's visual row is within the
    /// visible window `[viewport_top, viewport_top + visible_height)`.
    pub fn scroll_to_cursor(&mut self, width: u16, visible_height: u16) {
        let w = wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        let cursor_vr = u16::try_from(w.cursor.visual_row).unwrap_or(u16::MAX);
        if cursor_vr < self.viewport_top {
            self.viewport_top = cursor_vr;
        }
        if cursor_vr >= self.viewport_top.saturating_add(visible_height) {
            self.viewport_top = cursor_vr.saturating_sub(visible_height.saturating_sub(1));
        }
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::input::history::InputHistory;

    fn editor_with(text: &str) -> InputEditor {
        let mut e = InputEditor::new(InputHistory::in_memory());
        for ch in text.chars() {
            if ch == '\n' {
                e.insert_newline();
            } else {
                e.insert_char(ch);
            }
        }
        e
    }

    #[test]
    fn word_left_skips_non_word_then_word() {
        let mut e = editor_with("hello world");
        e.cursor_col = 11;
        e.word_left();
        assert_eq!(e.cursor_col, 6);
        e.word_left();
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn word_right_skips_word_then_non_word() {
        let mut e = editor_with("hello world");
        e.cursor_col = 0;
        e.word_right();
        assert_eq!(e.cursor_col, 6);
        e.word_right();
        assert_eq!(e.cursor_col, 11);
    }

    #[test]
    fn word_left_with_underscores() {
        let mut e = editor_with("foo_bar baz");
        e.cursor_col = 8;
        e.word_left();
        assert_eq!(e.cursor_col, 0, "underscore is a word char");
    }

    #[test]
    fn visual_cursor_up_across_wrap() {
        let mut e = editor_with("abcdefghij");
        e.cursor_col = 7;
        e.visual_cursor_up(5);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn visual_cursor_down_across_wrap() {
        let mut e = editor_with("abcdefghij");
        e.cursor_col = 2;
        e.visual_cursor_down(5);
        assert_eq!(e.cursor_col, 7);
    }

    #[test]
    fn cursor_on_first_visual_line_true() {
        let e = editor_with("abc");
        assert!(e.cursor_on_first_visual_line(80));
    }

    #[test]
    fn cursor_on_last_visual_line_with_wrap() {
        let mut e = editor_with("abcdefghij");
        e.cursor_col = 7;
        assert!(e.cursor_on_last_visual_line(5));
    }

    #[test]
    fn line_start_end() {
        let mut e = editor_with("hello");
        e.line_start();
        assert_eq!(e.cursor_col, 0);
        e.line_end();
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn buffer_start_end() {
        let mut e = editor_with("line1\nline2\nline3");
        e.buffer_start();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
        e.buffer_end();
        assert_eq!((e.cursor_row, e.cursor_col), (2, 5));
    }

    #[test]
    fn scroll_to_cursor_adjusts_viewport() {
        let mut e = editor_with("abcdefghijklmno");
        e.cursor_col = 14;
        e.viewport_top = 0;
        e.scroll_to_cursor(5, 2);
        assert!(e.viewport_top > 0);
    }

    #[test]
    fn delete_word_back_removes_preceding_word() {
        let mut e = editor_with("hello world");
        e.cursor_col = 11;
        e.delete_word_back();
        assert_eq!(e.text(), "hello ");
        assert_eq!(e.cursor_col, 6);
    }

    #[test]
    fn delete_word_back_at_col_zero_joins_lines() {
        let mut e = editor_with("ab\ncd");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.delete_word_back();
        assert_eq!(e.text(), "abcd");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn delete_word_forward_removes_following_word() {
        let mut e = editor_with("hello world");
        e.cursor_col = 0;
        e.delete_word_forward();
        assert_eq!(e.text(), "world");
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn delete_word_forward_at_end_joins_lines() {
        let mut e = editor_with("ab\ncd");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.delete_word_forward();
        assert_eq!(e.text(), "abcd");
    }

    #[test]
    fn delete_to_line_start_clears_before_cursor() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.delete_to_line_start();
        assert_eq!(e.text(), "world");
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn delete_to_line_start_at_col_zero_is_noop() {
        let mut e = editor_with("hello");
        e.cursor_col = 0;
        e.delete_to_line_start();
        assert_eq!(e.text(), "hello");
    }

    #[test]
    fn delete_to_line_end_clears_after_cursor() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.delete_to_line_end();
        assert_eq!(e.text(), "hello");
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn delete_to_line_end_at_end_is_noop() {
        let mut e = editor_with("hello");
        e.cursor_col = 5;
        e.delete_to_line_end();
        assert_eq!(e.text(), "hello");
    }
}
