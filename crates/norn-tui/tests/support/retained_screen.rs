//! Strict synchronized-frame and terminal-mode observations shared by real PTY fixtures.

use std::collections::BTreeSet;
use std::io;

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;
use vte::{Params, Parser, Perform};

/// Probe emitted by the actual terminal admission path.
pub const SYNC_QUERY: &[u8] = b"\x1b[?2026$p";
/// Explicit fixture capabilities: synchronized output and ordinary xterm keys.
pub const PROBE_REPLY: &[u8] = b"\x1b[?2026;1$y\x1b[?1;2c";
/// Completed-frame delimiter, also emitted harmlessly during cleanup.
pub const FRAME_END: &[u8] = b"\x1b[?2026l";
const FRAME_START: &[u8] = b"\x1b[?2026h";

/// One completed screen, using its actual requested geometry rather than raw printed history.
#[derive(Clone, Debug)]
pub struct Screen {
    /// Actual rows used for this complete frame.
    pub rows: u16,
    /// Actual columns used for this complete frame.
    pub cols: u16,
    /// Zero-based cursor column and row.
    pub cursor: (usize, usize),
    /// Whether the completed frame shows the cursor.
    pub cursor_visible: bool,
    /// Exclusive offset of this completed frame in the captured byte stream.
    pub end_offset: usize,
    cells: Vec<Vec<String>>,
    backgrounds: Vec<Vec<Option<[u8; 3]>>>,
    background: Option<[u8; 3]>,
    foreground: Option<[u8; 3]>,
    foregrounds: Vec<Vec<Option<[u8; 3]>>>,
    reversed: bool,
    reversals: Vec<Vec<bool>>,
    previous: Option<(usize, usize)>,
    error: Option<String>,
    painted: Vec<Vec<bool>>,
}

impl Screen {
    fn blank(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            cursor: (0, 0),
            cursor_visible: false,
            end_offset: 0,
            cells: vec![vec![" ".to_owned(); usize::from(cols)]; usize::from(rows)],
            backgrounds: vec![vec![None; usize::from(cols)]; usize::from(rows)],
            background: None,
            foreground: None,
            foregrounds: vec![vec![None; usize::from(cols)]; usize::from(rows)],
            reversed: false,
            reversals: vec![vec![false; usize::from(cols)]; usize::from(rows)],
            previous: None,
            error: None,
            painted: vec![vec![false; usize::from(cols)]; usize::from(rows)],
        }
    }

    /// Last complete frame for fixed geometry; absence is an explicit failure.
    pub fn from_output(output: &[u8], rows: u16, cols: u16) -> io::Result<Self> {
        latest(output, &[(rows, cols)])?
            .ok_or_else(|| io::Error::other("PTY has not published a complete synchronized frame"))
    }

    /// Visible rows, preserving their screen positions.
    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| row.concat().trim_end().to_owned())
            .collect()
    }

    /// Checked cell contents at a zero-based column and row; wide-glyph continuations are empty.
    pub fn cell(&self, column: usize, row: usize) -> Option<&str> {
        self.cells.get(row)?.get(column).map(String::as_str)
    }

    /// Human-readable screen state for a failed assertion.
    pub fn debug_text(&self) -> String {
        self.lines().join("\n")
    }

    /// Whether text appears in the currently displayed rows.
    pub fn contains(&self, text: &str) -> bool {
        self.debug_text().contains(text)
    }

    /// Count visible occurrences, not repeated redraw bytes in native output history.
    pub fn occurrences(&self, text: &str) -> usize {
        self.debug_text().matches(text).count()
    }

    /// Foreground of an observed cell; None is the terminal's ordinary foreground.
    pub fn foreground_at(&self, column: usize, row: usize) -> Option<[u8; 3]> {
        self.foregrounds.get(row)?.get(column).copied().flatten()
    }

    /// Actual reverse-video selection emphasis from the terminal cell stream.
    pub fn selected_at(&self, column: usize, row: usize) -> bool {
        self.reversals
            .get(row)
            .and_then(|line| line.get(column))
            .copied()
            .unwrap_or(false)
    }

    /// Actual input rows between the restored top and metadata chip rules.
    /// Very short screens suppress chrome and retain one bottom input row.
    pub fn composer_rows(&self) -> Vec<usize> {
        let count = usize::from(self.rows);
        if count < 6 {
            return count
                .checked_sub(1)
                .filter(|row| self.cursor_visible && self.cursor.1 == *row)
                .into_iter()
                .collect();
        }
        let metadata = count - 2;
        if !self.chip_rule(metadata, false) || self.lines()[count - 1].is_empty() {
            return Vec::new();
        }
        let Some(top) = (0..metadata).rev().find(|row| self.chip_rule(*row, true)) else {
            return Vec::new();
        };
        (top + 1..metadata).collect()
    }

    fn chip_rule(&self, row: usize, left: bool) -> bool {
        let text = self.cells[row].concat();
        if text.width() != usize::from(self.cols) || !text.contains("🮠 ") {
            return false;
        }
        if left && !text.starts_with("───🮠 ") {
            return false;
        }
        let Some((prefix, label)) = text.split_once("🮠 ") else {
            return false;
        };
        if !prefix.chars().all(|character| character == '─') {
            return false;
        }
        if let Some((label, suffix)) = label.split_once(" 🮣") {
            !label.is_empty() && suffix.chars().all(|character| character == '─')
        } else {
            // Existing narrow metadata chips truncate to the exact terminal width.
            text.ends_with('…')
        }
    }

    /// Exact original-style panel geometry, unfilled input background and caret ownership.
    pub fn assert_composer(&self, height: usize) -> io::Result<()> {
        let rows = usize::from(self.rows);
        let after_input = if rows >= 6 { rows - 2 } else { rows };
        if height == 0 || height > after_input {
            return Err(io::Error::other(
                "fixture composer height is outside screen",
            ));
        }
        let expected: Vec<_> = (after_input - height..after_input).collect();
        if self.composer_rows() != expected
            || !self.cursor_visible
            || !expected.contains(&self.cursor.1)
            || self.cursor.0 >= usize::from(self.cols)
            || expected
                .iter()
                .any(|row| self.backgrounds[*row].iter().any(Option::is_some))
        {
            return Err(io::Error::other(format!(
                "composer/caret mismatch: expected {expected:?}, actual {:?}, cursor {:?}, visible {}; screen:\n{}",
                self.composer_rows(),
                self.cursor,
                self.cursor_visible,
                self.debug_text()
            )));
        }
        Ok(())
    }

    fn apply_frame(mut self, raw: &[u8], end_offset: usize) -> Self {
        self.previous = None;
        for row in &mut self.painted {
            row.fill(false);
        }
        Parser::new().advance(&mut self, raw);
        self.end_offset = end_offset;
        self
    }

    fn fully_painted(&self) -> bool {
        self.rows > 0 && self.cols > 0 && self.painted.iter().flatten().all(|painted| *painted)
    }

    fn valid(&self) -> bool {
        self.error.is_none()
            && !self.composer_rows().is_empty()
            && self
                .cells
                .iter()
                .all(|row| row.concat().width() == usize::from(self.cols))
    }

    fn fail(&mut self, reason: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(reason.into());
        }
    }

    fn position(&mut self, row: usize, col: usize) {
        if row == 0 || col == 0 || row > usize::from(self.rows) || col > usize::from(self.cols) {
            self.fail(format!(
                "frame CUP {row},{col} exceeds {}x{}",
                self.cols, self.rows
            ));
            return;
        }
        self.cursor = (col - 1, row - 1);
        self.previous = None;
    }

    fn sgr(&mut self, params: &Params) {
        let values: Vec<_> = params
            .iter()
            .flat_map(|part| part.iter().copied())
            .collect();
        let mut index = 0;
        while let Some(value) = values.get(index) {
            match value {
                0 => {
                    self.background = None;
                    self.foreground = None;
                    self.reversed = false;
                }
                1 | 2 | 3 | 4 | 9 => {}
                7 => self.reversed = true,
                27 => self.reversed = false,
                39 => self.foreground = None,
                49 => self.background = None,
                38 | 48 => {
                    let Some([2, red, green, blue]) = values.get(index + 1..index + 5) else {
                        self.fail("frame colour is not complete RGB");
                        return;
                    };
                    if let (Ok(red), Ok(green), Ok(blue)) = (
                        u8::try_from(*red),
                        u8::try_from(*green),
                        u8::try_from(*blue),
                    ) {
                        if *value == 48 {
                            self.background = Some([red, green, blue]);
                        } else {
                            self.foreground = Some([red, green, blue]);
                        }
                    } else {
                        self.fail("frame RGB channel exceeds 255");
                        return;
                    }
                    index += 4;
                }
                _ => self.fail(format!("unsupported frame SGR {value}")),
            }
            index += 1;
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, character: char) {
        if self.error.is_some() {
            return;
        }
        if character.is_control() {
            self.fail("unescaped control in frame text");
            return;
        }
        let mut text = character.to_string();
        let mut location = self.cursor;
        if let Some(previous) = self.previous {
            let combined = format!("{}{character}", self.cells[previous.1][previous.0]);
            if combined.graphemes(true).count() == 1 {
                location = previous;
                text = combined;
            }
        }
        let width = text.width();
        if width == 0
            || location.1 >= usize::from(self.rows)
            || location.0 + width > usize::from(self.cols)
        {
            self.fail("frame glyph is unanchored or exceeds its row");
            return;
        }
        self.cells[location.1][location.0].clone_from(&text);
        for column in location.0..location.0 + width {
            self.painted[location.1][column] = true;
            self.backgrounds[location.1][column] = self.background;
            self.foregrounds[location.1][column] = self.foreground;
            self.reversals[location.1][column] = self.reversed;
            if column != location.0 {
                self.cells[location.1][column].clear();
            }
        }
        self.cursor = (location.0 + width, location.1);
        self.previous = Some(location);
    }

    fn execute(&mut self, byte: u8) {
        self.fail(format!("unescaped frame control {byte}"));
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            self.fail("ignored/malformed frame CSI");
            return;
        }
        if intermediates == b"?" && matches!(action, 'h' | 'l') {
            match parameter(params, 0, 0) {
                25 => self.cursor_visible = action == 'h',
                2026 => {}
                mode => self.fail(format!("unexpected frame private mode {mode}")),
            }
        } else if !intermediates.is_empty() {
            self.fail("unexpected frame CSI intermediate");
        } else {
            match action {
                'H' | 'f' => self.position(parameter(params, 0, 1), parameter(params, 1, 1)),
                'J' if parameter(params, 0, 0) == 2 => {
                    for row in &mut self.cells {
                        row.fill(" ".to_owned());
                    }
                    for row in &mut self.backgrounds {
                        row.fill(None);
                    }
                    for row in &mut self.reversals {
                        row.fill(false);
                    }
                    for row in &mut self.foregrounds {
                        row.fill(None);
                    }
                    self.previous = None;
                    for row in &mut self.painted {
                        row.fill(true);
                    }
                }
                'm' => self.sgr(params),
                _ => self.fail(format!("unsupported retained-frame CSI {action}")),
            }
        }
    }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignored: bool, byte: u8) {
        self.fail(format!(
            "unexpected frame escape {byte}, intermediates {intermediates:?}, ignored {ignored}"
        ));
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.fail(format!(
            "unexpected OSC inside painted frame: {} parameters, bell {bell_terminated}",
            params.len()
        ));
    }
}

/// Apply complete synchronized frames in order, retaining untouched cells and styles.
/// Only a complete paint can admit the next explicitly requested geometry epoch.
/// A queued old frame remains on the old dimensions; a fitting delta cannot resize it.
pub fn latest(output: &[u8], geometries: &[(u16, u16)]) -> io::Result<Option<Screen>> {
    let mut epochs = geometries.to_vec();
    epochs.dedup();
    let mut epoch = 0;
    let mut offset = 0;
    let mut selected: Option<Screen> = None;
    while let Some(start) = find(&output[offset..], FRAME_START) {
        let start = offset + start;
        let Some(end) = find(&output[start + FRAME_START.len()..], FRAME_END) else {
            break;
        };
        let end = start + FRAME_START.len() + end + FRAME_END.len();
        let raw = &output[start..end];
        let next_epoch = if selected.is_some() { epoch + 1 } else { epoch };
        let full = epochs.get(next_epoch).and_then(|&(rows, cols)| {
            let candidate = Screen::blank(rows, cols).apply_frame(raw, end);
            (candidate.valid() && candidate.fully_painted()).then_some(candidate)
        });
        if let Some(full) = full {
            selected = Some(full);
            epoch = next_epoch;
        } else if let Some(previous) = selected.take() {
            let candidate = previous.apply_frame(raw, end);
            if !candidate.valid() {
                return Err(io::Error::other(format!(
                    "invalid retained delta at byte {start} for {}x{}: {:?}",
                    candidate.cols, candidate.rows, candidate.error
                )));
            }
            selected = Some(candidate);
        } else {
            return Err(io::Error::other(format!(
                "initial retained frame lacks a complete valid paint of the first requested geometry at byte {start}"
            )));
        }
        offset = end;
    }
    Ok(selected)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|part| part == needle)
}
fn parameter(params: &Params, index: usize, default: usize) -> usize {
    params
        .iter()
        .nth(index)
        .and_then(|part| part.first())
        .copied()
        .map_or(default, usize::from)
}

/// Mode and outer-screen state outside synchronized frames, with real main-buffer erasure.
#[derive(Debug)]
pub struct Lifecycle {
    modes: BTreeSet<u16>,
    alternate: bool,
    entries: usize,
    leaves: usize,
    main: Vec<Vec<char>>,
    row: usize,
    col: usize,
    forbidden_scroll_region: bool,
}
impl Lifecycle {
    /// Observe all emitted bytes, including admission and cleanup.
    pub fn from_output(output: &[u8], rows: u16, cols: u16) -> Self {
        let mut value = Self {
            modes: BTreeSet::from([7, 25]),
            alternate: false,
            entries: 0,
            leaves: 0,
            main: vec![vec![' '; usize::from(cols)]; usize::from(rows)],
            row: 0,
            col: 0,
            forbidden_scroll_region: false,
        };
        Parser::new().advance(&mut value, output);
        value
    }
    /// Full main/alternate mode restoration; terminal termios is checked separately by the PTY owner.
    pub fn assert_restored(&self) -> io::Result<()> {
        if self.alternate
            || self.entries != 1
            || self.leaves != 1
            || self.modes != BTreeSet::from([7, 25])
            || self.forbidden_scroll_region
        {
            return Err(io::Error::other(format!(
                "terminal modes were not restored: {self:?}"
            )));
        }
        Ok(())
    }
    /// Visible outer-screen rows, excluding alternate-screen content.
    pub fn main_text(&self) -> String {
        self.main
            .iter()
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn newline(&mut self) {
        self.row += 1;
        if self.row >= self.main.len() && !self.main.is_empty() {
            self.main.rotate_left(1);
            if let Some(last) = self.main.last_mut() {
                last.fill(' ');
            }
            self.row = self.main.len() - 1;
        }
    }
}
impl Perform for Lifecycle {
    fn print(&mut self, character: char) {
        if self.alternate || self.main.is_empty() || self.main[0].is_empty() {
            return;
        }
        if self.col >= self.main[0].len() {
            self.col = 0;
            self.newline();
        }
        self.main[self.row][self.col] = character;
        self.col += 1;
    }
    fn execute(&mut self, byte: u8) {
        if self.alternate {
            return;
        }
        match byte {
            b'\r' => self.col = 0,
            b'\n' => self.newline(),
            _ => {}
        }
    }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        if intermediates == b"?" && matches!(action, 'h' | 'l') {
            for mode in params.iter().filter_map(|part| part.first()).copied() {
                if action == 'h' {
                    self.modes.insert(mode);
                } else {
                    self.modes.remove(&mode);
                }
                if mode == 1049 {
                    self.alternate = action == 'h';
                    if self.alternate {
                        self.entries += 1;
                    } else {
                        self.leaves += 1;
                    }
                }
            }
        } else if intermediates.is_empty() && action == 'r' {
            self.forbidden_scroll_region = true;
        } else if !self.alternate && intermediates.is_empty() {
            match action {
                'H' | 'f' => {
                    self.row = parameter(params, 0, 1)
                        .saturating_sub(1)
                        .min(self.main.len().saturating_sub(1));
                    self.col = parameter(params, 1, 1)
                        .saturating_sub(1)
                        .min(self.main.first().map_or(0, Vec::len).saturating_sub(1));
                }
                'J' if parameter(params, 0, 0) == 2 => {
                    for row in &mut self.main {
                        row.fill(' ');
                    }
                }
                'K' => {
                    if let Some(row) = self.main.get_mut(self.row) {
                        row.fill(' ');
                    }
                }
                _ => {}
            }
        }
    }
}
