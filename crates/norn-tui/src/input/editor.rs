//! Multi-line input editor for the fixed panel.
//!
//! [`InputEditor`] owns a multi-line text buffer and the editing cursor,
//! reports its logical and visual height, and applies the semantic editing
//! operations dispatched by the keybinding mapper. It
//! never emits DECSTBM escapes itself: its [`height`](InputEditor::height)
//! changes after a newline insertion or a clear, and the fixed-panel
//! compositor (NT-002) plus the event loop (NT-011) act on that height
//! change to reissue the scroll region.
//!
//! History recall is delegated to [`InputHistory`]. A submission records
//! the entry before returning it, so the persisted history always stays
//! in step with what the user has sent.

use std::io;

use super::autocomplete::Acceptance;
use super::history::InputHistory;
use super::keybindings::InputAction;

/// Multi-line input editor rendered in the fixed panel's input area.
///
/// `lines` always holds at least one (possibly empty) entry. `cursor_row`
/// indexes `lines`; `cursor_col` is a *character* offset within the
/// cursor's line — never a byte offset — so the cursor can never land in
/// the middle of a multi-byte codepoint.
#[derive(Debug)]
pub struct InputEditor {
    /// Buffer lines; invariant: never empty.
    pub(super) lines: Vec<String>,
    /// Cursor row — an index into `lines`; invariant: `< lines.len()`.
    pub(super) cursor_row: usize,
    /// Cursor column — a character offset within the cursor's line;
    /// invariant: `<= lines[cursor_row].chars().count()`.
    pub(super) cursor_col: usize,
    /// Recall buffer for previous submissions.
    pub(super) history: InputHistory,
    /// Top visual row of the visible viewport (internal scroll offset).
    pub(super) viewport_top: u16,
}

impl InputEditor {
    /// Construct an editor with an empty single-line buffer, backed by
    /// the given history.
    #[must_use]
    pub fn new(history: InputHistory) -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            history,
            viewport_top: 0,
        }
    }

    /// Number of logical buffer lines — minimum one.
    #[must_use]
    pub fn height(&self) -> u16 {
        u16::try_from(self.lines.len().max(1)).unwrap_or(u16::MAX)
    }

    /// Number of visual (wrapped) lines at the given terminal width.
    #[must_use]
    pub fn visual_height(&self, width: u16) -> u16 {
        let w = super::wrap::layout(&self.lines, self.cursor_row, self.cursor_col, width);
        u16::try_from(w.rows.len()).unwrap_or(u16::MAX)
    }

    /// Current viewport scroll offset.
    #[must_use]
    pub fn viewport_top(&self) -> u16 {
        self.viewport_top
    }

    /// Whether every line in the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(String::is_empty)
    }

    /// Borrowed view of the buffer lines.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Current cursor position as `(row, col)` in character offsets.
    #[must_use]
    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// The buffer contents, joined with newlines.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Character offset of the cursor within [`text`](Self::text).
    ///
    /// Returns the position the autocomplete trigger scanner uses —
    /// `\n` newlines between lines each count as one character, matching
    /// the flat string [`text`](Self::text) returns.
    #[must_use]
    pub fn cursor_char_index(&self) -> usize {
        let preceding: usize = self.lines[..self.cursor_row]
            .iter()
            .map(|line| line.chars().count() + 1)
            .sum();
        preceding + self.cursor_col
    }

    /// Byte offset of the cursor within [`text`](Self::text).
    fn cursor_byte_index(&self) -> usize {
        let preceding: usize = self.lines[..self.cursor_row]
            .iter()
            .map(|line| line.len() + 1)
            .sum();
        preceding + Self::byte_index(&self.lines[self.cursor_row], self.cursor_col)
    }

    /// Apply an autocomplete [`Acceptance`] to the buffer.
    ///
    /// Replaces the bytes from `acceptance.trigger_start_byte` up to the
    /// current cursor byte position with `acceptance.replacement`. After
    /// the splice the cursor is parked immediately after the inserted
    /// text. History navigation is cancelled so the next Up arrow
    /// captures the new draft.
    ///
    /// A malformed acceptance (start past cursor, or either offset past
    /// the end of the buffer) is a no-op rather than a panic — the
    /// editor never trusts a byte offset against a buffer that may have
    /// shifted since the popup snapshot.
    pub fn apply_acceptance(&mut self, acceptance: &Acceptance) {
        let text = self.text();
        let cursor_byte = self.cursor_byte_index();
        if acceptance.trigger_start_byte > cursor_byte || cursor_byte > text.len() {
            return;
        }
        if !text.is_char_boundary(acceptance.trigger_start_byte)
            || !text.is_char_boundary(cursor_byte)
        {
            return;
        }
        let mut new_text = String::with_capacity(
            text.len() - (cursor_byte - acceptance.trigger_start_byte)
                + acceptance.replacement.len(),
        );
        new_text.push_str(&text[..acceptance.trigger_start_byte]);
        new_text.push_str(&acceptance.replacement);
        new_text.push_str(&text[cursor_byte..]);

        let new_cursor_byte = acceptance.trigger_start_byte + acceptance.replacement.len();
        self.lines = new_text.split('\n').map(str::to_owned).collect();
        let (row, col) = byte_to_row_col(&self.lines, new_cursor_byte);
        self.cursor_row = row;
        self.cursor_col = col;
        self.history.cancel_navigation();
    }

    /// Byte offset of the `col`-th character in `line`, or the line's
    /// length when `col` is at or past the end.
    pub(super) fn byte_index(line: &str, col: usize) -> usize {
        line.char_indices()
            .nth(col)
            .map_or(line.len(), |(idx, _)| idx)
    }

    /// Reset the buffer to a single empty line and park the cursor.
    fn reset_buffer(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Replace the buffer with `text`, placing the cursor at the end of
    /// the last line. Used when a history entry is recalled.
    fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_owned).collect();
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    /// Clear the input buffer (Escape).
    ///
    /// The buffer resets to a single empty line and history navigation is
    /// cancelled so the next Up arrow re-captures the (now empty) draft.
    pub fn clear(&mut self) {
        self.reset_buffer();
        self.history.cancel_navigation();
    }

    /// Submit the buffer contents (Enter).
    ///
    /// Returns the joined buffer text and resets the editor when the
    /// buffer is non-empty. Returns `None` for an empty buffer, making an
    /// Enter on empty input a no-op. The submitted text is appended to
    /// the history before it is returned; a persistence failure
    /// propagates so it is never silently swallowed (CO5).
    pub fn submit(&mut self) -> io::Result<Option<String>> {
        if self.is_empty() {
            return Ok(None);
        }
        let text = self.text();
        self.history.append(&text)?;
        self.reset_buffer();
        self.history.cancel_navigation();
        Ok(Some(text))
    }

    /// Handle Ctrl+C.
    ///
    /// On an empty buffer this returns [`InputAction::Exit`] for the
    /// event loop to act on; otherwise it clears the buffer — matching
    /// Escape — and returns `None`.
    pub fn ctrl_c(&mut self) -> Option<InputAction> {
        if self.is_empty() {
            Some(InputAction::Exit)
        } else {
            self.clear();
            None
        }
    }

    /// Recall the previous (older) history entry into the buffer (Up).
    ///
    /// The live buffer text is offered to the history as the draft to
    /// restore later. An empty history leaves the buffer untouched.
    pub fn history_prev(&mut self) {
        let current = self.text();
        if let Some(entry) = self.history.prev(&current) {
            self.set_text(&entry);
        }
    }

    /// Recall the next (newer) history entry into the buffer (Down).
    ///
    /// Navigating forward past the newest entry restores the saved draft.
    /// When not navigating, the buffer is left untouched.
    pub fn history_next(&mut self) {
        if let Some(entry) = self.history.advance() {
            self.set_text(&entry);
        }
    }
}

/// Convert a byte offset within `lines.join("\n")` to a `(row, col)`
/// position where `col` is a character offset within `lines[row]`.
///
/// The helper is paired with [`InputEditor::apply_acceptance`]: after
/// splicing a replacement string into the flat buffer, the cursor's
/// final byte offset must be turned back into the `(row, col)`
/// representation `InputEditor` carries. Out-of-range byte offsets
/// saturate at the end of the last line rather than panicking, so a
/// stale acceptance can never crash the editor.
fn byte_to_row_col(lines: &[String], byte_idx: usize) -> (usize, usize) {
    let mut consumed = 0usize;
    for (row, line) in lines.iter().enumerate() {
        let line_end = consumed + line.len();
        if byte_idx <= line_end {
            let within = byte_idx - consumed;
            let prefix = line.get(..within).unwrap_or(line);
            return (row, prefix.chars().count());
        }
        consumed = line_end + 1;
    }
    if let Some((row, last)) = lines.iter().enumerate().next_back() {
        (row, last.chars().count())
    } else {
        (0, 0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn editor() -> InputEditor {
        InputEditor::new(InputHistory::in_memory())
    }

    #[test]
    fn empty_editor_has_height_one() {
        assert_eq!(editor().height(), 1);
    }

    #[test]
    fn three_line_editor_has_height_three() {
        let mut ed = editor();
        ed.insert_newline();
        ed.insert_newline();
        assert_eq!(ed.height(), 3);
    }

    #[test]
    fn insert_newline_grows_one_line_editor_to_height_two() {
        let mut ed = editor();
        ed.insert_newline();
        assert_eq!(ed.height(), 2);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_splits_the_current_line() {
        let mut ed = editor();
        for ch in "helloworld".chars() {
            ed.insert_char(ch);
        }
        ed.cursor_col = 5;
        ed.insert_newline();
        assert_eq!(ed.lines, vec!["hello".to_string(), "world".to_string()]);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn submit_joins_lines_and_resets_to_empty() {
        let mut ed = editor();
        for ch in "hello".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "world".chars() {
            ed.insert_char(ch);
        }
        let submitted = ed.submit().unwrap();
        assert_eq!(submitted, Some("hello\nworld".to_string()));
        assert_eq!(ed.height(), 1);
        assert!(ed.is_empty());
    }

    #[test]
    fn submit_on_empty_buffer_is_a_noop() {
        let mut ed = editor();
        assert_eq!(ed.submit().unwrap(), None);
        assert!(ed.is_empty());
    }

    #[test]
    fn clear_on_multiline_input_returns_to_empty_single_line() {
        let mut ed = editor();
        for ch in "abc".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "def".chars() {
            ed.insert_char(ch);
        }
        ed.clear();
        assert_eq!(ed.height(), 1);
        assert!(ed.is_empty());
        assert_eq!(ed.cursor_row, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn ctrl_c_on_empty_returns_exit() {
        let mut ed = editor();
        assert_eq!(ed.ctrl_c(), Some(InputAction::Exit));
    }

    #[test]
    fn ctrl_c_on_non_empty_clears_and_returns_no_action() {
        let mut ed = editor();
        for ch in "draft".chars() {
            ed.insert_char(ch);
        }
        assert_eq!(ed.ctrl_c(), None);
        assert!(ed.is_empty());
        assert_eq!(ed.height(), 1);
    }

    #[test]
    fn history_prev_twice_recalls_oldest_entry() {
        let mut ed = editor();
        for ch in "a".chars() {
            ed.insert_char(ch);
        }
        ed.submit().unwrap();
        for ch in "b".chars() {
            ed.insert_char(ch);
        }
        ed.submit().unwrap();

        ed.history_prev();
        assert_eq!(ed.text(), "b");
        ed.history_prev();
        assert_eq!(ed.text(), "a");
    }

    #[test]
    fn history_next_restores_the_draft() {
        let mut ed = editor();
        for ch in "a".chars() {
            ed.insert_char(ch);
        }
        ed.submit().unwrap();
        for ch in "draft".chars() {
            ed.insert_char(ch);
        }
        ed.history_prev();
        assert_eq!(ed.text(), "a");
        ed.history_next();
        assert_eq!(ed.text(), "draft");
    }

    #[test]
    fn recalled_entry_places_cursor_at_end_of_last_line() {
        let mut ed = editor();
        for ch in "one".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "two".chars() {
            ed.insert_char(ch);
        }
        ed.submit().unwrap();
        ed.history_prev();
        assert_eq!(ed.lines, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn backspace_joins_lines_at_a_line_start() {
        let mut ed = editor();
        for ch in "ab".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "cd".chars() {
            ed.insert_char(ch);
        }
        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.lines, vec!["abcd".to_string()]);
        assert_eq!(ed.cursor_row, 0);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn delete_removes_the_character_at_the_cursor() {
        let mut ed = editor();
        for ch in "abc".chars() {
            ed.insert_char(ch);
        }
        ed.cursor_col = 1;
        ed.delete();
        assert_eq!(ed.lines, vec!["ac".to_string()]);
    }

    #[test]
    fn insert_char_respects_multibyte_cursor_positions() {
        let mut ed = editor();
        for ch in "héllo".chars() {
            ed.insert_char(ch);
        }
        ed.cursor_col = 1;
        ed.insert_char('x');
        assert_eq!(ed.lines, vec!["hxéllo".to_string()]);
    }

    #[test]
    fn cursor_moves_wrap_across_lines() {
        let mut ed = editor();
        for ch in "ab".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "cd".chars() {
            ed.insert_char(ch);
        }
        // Cursor sits at (1, 2). Left at col 0 wraps up; right at end wraps down.
        ed.cursor_col = 0;
        ed.cursor_left();
        assert_eq!((ed.cursor_row, ed.cursor_col), (0, 2));
        ed.cursor_right();
        assert_eq!((ed.cursor_row, ed.cursor_col), (1, 0));
    }

    #[test]
    fn lines_returns_buffer_reference() {
        let mut ed = editor();
        for ch in "hello".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "world".chars() {
            ed.insert_char(ch);
        }
        assert_eq!(ed.lines(), &["hello", "world"]);
    }

    #[test]
    fn cursor_position_tracks_editing() {
        let mut ed = editor();
        for ch in "abc".chars() {
            ed.insert_char(ch);
        }
        assert_eq!(ed.cursor_position(), (0, 3));
        ed.insert_newline();
        assert_eq!(ed.cursor_position(), (1, 0));
        ed.insert_char('x');
        assert_eq!(ed.cursor_position(), (1, 1));
    }

    #[test]
    fn cursor_char_index_matches_flat_text_position() {
        let mut ed = editor();
        for ch in "hi".chars() {
            ed.insert_char(ch);
        }
        ed.insert_newline();
        for ch in "yo".chars() {
            ed.insert_char(ch);
        }
        // Cursor sits at end of "yo": flat text is "hi\nyo", cursor at char 5.
        assert_eq!(ed.cursor_char_index(), 5);
        // Move to middle of first line — char index should drop.
        ed.cursor_row = 0;
        ed.cursor_col = 1;
        assert_eq!(ed.cursor_char_index(), 1);
    }

    #[test]
    fn cursor_char_index_counts_multibyte_chars_as_one() {
        let mut ed = editor();
        for ch in "héx".chars() {
            ed.insert_char(ch);
        }
        // Cursor at end: 3 characters even though 'é' is two bytes.
        assert_eq!(ed.cursor_char_index(), 3);
    }

    #[test]
    fn apply_acceptance_splices_replacement_for_slash_command() {
        let mut ed = editor();
        for ch in "/he".chars() {
            ed.insert_char(ch);
        }
        let acceptance = Acceptance {
            trigger_start_byte: 0,
            replacement: "/help".to_string(),
        };
        ed.apply_acceptance(&acceptance);
        assert_eq!(ed.text(), "/help");
        // Cursor parked at end of inserted replacement (5 chars).
        assert_eq!(ed.cursor_position(), (0, 5));
    }

    #[test]
    fn apply_acceptance_splices_replacement_for_file_path() {
        let mut ed = editor();
        for ch in "look at @sr".chars() {
            ed.insert_char(ch);
        }
        let acceptance = Acceptance {
            trigger_start_byte: 8,
            replacement: "src/main.rs".to_string(),
        };
        ed.apply_acceptance(&acceptance);
        assert_eq!(ed.text(), "look at src/main.rs");
        // Cursor parked at end of "src/main.rs" — column 19.
        assert_eq!(ed.cursor_position(), (0, 19));
    }

    #[test]
    fn apply_acceptance_preserves_text_after_cursor() {
        let mut ed = editor();
        for ch in "x @sr y".chars() {
            ed.insert_char(ch);
        }
        // Move cursor to right after "@sr", before " y".
        ed.cursor_col = 5;
        let acceptance = Acceptance {
            trigger_start_byte: 2,
            replacement: "src/lib.rs".to_string(),
        };
        ed.apply_acceptance(&acceptance);
        assert_eq!(ed.text(), "x src/lib.rs y");
        // Cursor after "src/lib.rs" — column 12.
        assert_eq!(ed.cursor_position(), (0, 12));
    }

    #[test]
    fn apply_acceptance_with_invalid_offsets_is_noop() {
        let mut ed = editor();
        for ch in "abc".chars() {
            ed.insert_char(ch);
        }
        let acceptance = Acceptance {
            trigger_start_byte: 100,
            replacement: "garbage".to_string(),
        };
        ed.apply_acceptance(&acceptance);
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn apply_acceptance_at_multibyte_boundary_is_safe() {
        let mut ed = editor();
        for ch in "@é".chars() {
            ed.insert_char(ch);
        }
        // 'é' is two bytes; cursor at end is byte 3.
        let acceptance = Acceptance {
            trigger_start_byte: 0,
            replacement: "/help".to_string(),
        };
        ed.apply_acceptance(&acceptance);
        assert_eq!(ed.text(), "/help");
    }

    #[test]
    fn byte_to_row_col_resolves_offsets_across_lines() {
        let lines = vec!["hi".to_string(), "yo".to_string()];
        // Byte 0 → start of first line.
        assert_eq!(byte_to_row_col(&lines, 0), (0, 0));
        // Byte 2 → end of first line.
        assert_eq!(byte_to_row_col(&lines, 2), (0, 2));
        // Byte 3 → start of second line (after the implicit '\n').
        assert_eq!(byte_to_row_col(&lines, 3), (1, 0));
        // Byte 5 → end of second line.
        assert_eq!(byte_to_row_col(&lines, 5), (1, 2));
        // Out-of-range — saturates to end of last line.
        assert_eq!(byte_to_row_col(&lines, 100), (1, 2));
    }
}
