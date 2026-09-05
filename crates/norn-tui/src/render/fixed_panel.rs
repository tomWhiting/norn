//! Fixed-panel compositor.
//!
//! The fixed panel occupies the bottom rows of the terminal — below the
//! DECSTBM scroll region — and holds, from top to bottom: the input-mode
//! divider, autocomplete/input rows, the session metadata divider, live
//! status rows, optional backing-log rows, and the key-hint row. [`FixedPanel`]
//! tracks which components are active and their row counts, computes the
//! total panel height, and performs a cursor-addressed redraw confined
//! strictly to the panel rows (CO8: the fixed panel is the only
//! cursor-addressed rendering).
//!
//! The compositor does not own the terminal guard. NT-011 reads
//! [`FixedPanel::height_dirty`] and [`FixedPanel::total_height`] to
//! decide when to reissue the DECSTBM scroll region (via the guard from
//! NT-001) before calling [`FixedPanel::render`].

use std::io;
use std::time::Duration;

use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::Intensity;
use unicode_width::UnicodeWidthStr as _;

use super::streaming_indicator::StreamingIndicator;
use super::style::{newline_key_hint, sync_render};
use super::text::{format_count, truncate_with_ellipsis};
use crate::terminal::caps::TerminalCaps;

/// Rows occupied by the always-present key-hint row.
const HELP_BAR_ROWS: u16 = 1;

/// Rows occupied by the always-present input-mode divider.
pub(crate) const SEPARATOR_ROWS: u16 = 1;

/// Rows occupied by the always-present session metadata divider.
const METADATA_SEPARATOR_ROWS: u16 = 1;

/// Box-drawings light horizontal (U+2500) — the separator glyph.
const SEPARATOR_CHAR: char = '\u{2500}';

/// Cursor-position escape targeting the start of a zero-based `row`.
fn cursor_to(row: u16) -> Csi {
    Csi::Cursor(Cursor::Position {
        line: OneBased::from_zero_based(row),
        col: OneBased::from_zero_based(0),
    })
}

/// Escape that erases the entire line the cursor sits on.
fn erase_line() -> Csi {
    Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine))
}

/// Position the cursor at `row` and clear that line.
fn clear_row<W: io::Write>(row: u16, writer: &mut W) -> io::Result<()> {
    write!(writer, "{}{}", cursor_to(row), erase_line())
}

/// Build a horizontal rule of exactly `width` display columns.
fn rule(width: usize) -> String {
    std::iter::repeat_n(SEPARATOR_CHAR, width).collect()
}

fn fit_line_to_width(line: &str, width: u16) -> String {
    let truncated = truncate_with_ellipsis(line, width);
    let width = usize::from(width);
    let padding = width.saturating_sub(truncated.width());
    format!("{truncated}{}", rule(padding))
}

fn left_chip_separator(label: &str, width: u16) -> String {
    let prefix = rule(3);
    let chip = format!("🮠 {label} 🮣");
    let used = prefix.width().saturating_add(chip.width());
    let target = usize::from(width);
    if used >= target {
        return fit_line_to_width(&format!("{prefix}{chip}"), width);
    }
    format!("{prefix}{chip}{}", rule(target - used))
}

fn right_chip_separator(label: &str, width: u16) -> String {
    let suffix = rule(3);
    let chip = format!("🮠 {label} 🮣");
    let used = chip.width().saturating_add(suffix.width());
    let target = usize::from(width);
    if used >= target {
        return truncate_with_ellipsis(&format!("{chip}{suffix}"), width);
    }
    format!("{}{chip}{suffix}", rule(target - used))
}

/// Render a dim separator line at zero-based `row`.
fn render_separator_line<W: io::Write>(row: u16, line: &str, writer: &mut W) -> io::Result<()> {
    write!(
        writer,
        "{}{}{}{line}{}",
        cursor_to(row),
        erase_line(),
        Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
        Csi::Sgr(Sgr::Reset),
    )
}

/// Session metadata and key hints shown in the fixed panel.
#[derive(Clone, Debug, Default)]
pub struct StatusBar {
    /// Name of the active model.
    pub model_name: String,
    /// Name of the active session.
    pub session_name: String,
    /// Input tokens for the current root turn shown in the top divider.
    pub input_tokens: u64,
    /// Whether [`Self::input_tokens`] is a live estimate rather than a
    /// provider-reported value.
    pub input_tokens_estimated: bool,
    /// Output tokens for the current root turn shown in the top divider.
    pub output_tokens: u64,
    /// Whether [`Self::output_tokens`] is a live estimate rather than a
    /// provider-reported value.
    pub output_tokens_estimated: bool,
    /// Free-form key-hint text shown alongside the newline-key hint.
    pub key_hints: String,
    /// Active provider service tier, when explicitly set.
    pub service_tier: Option<String>,
    /// Active reasoning effort, when explicitly set.
    pub reasoning_effort: Option<String>,
}

impl StatusBar {
    fn metadata_label(&self) -> String {
        let mut parts = Vec::new();
        if !self.model_name.is_empty() {
            parts.push(self.model_name.clone());
        }
        if let Some(tier) = self.service_tier.as_deref() {
            parts.push(format!("tier:{tier}"));
        }
        if let Some(effort) = self.reasoning_effort.as_deref() {
            parts.push(format!("effort:{effort}"));
        }
        if !self.session_name.is_empty() {
            parts.push(self.session_name.clone());
        }
        if parts.is_empty() {
            "norn".to_string()
        } else {
            parts.join(" • ")
        }
    }

    /// Render the right-aligned metadata divider below the input area.
    pub fn render_metadata_divider<W: io::Write>(
        &self,
        row: u16,
        width: u16,
        writer: &mut W,
    ) -> io::Result<()> {
        let line = right_chip_separator(&self.metadata_label(), width);
        render_separator_line(row, &line, writer)
    }

    /// Render the bottom key-hint row.
    pub fn render<W: io::Write>(
        &self,
        row: u16,
        width: u16,
        writer: &mut W,
        caps: &TerminalCaps,
    ) -> io::Result<()> {
        self.render_help(row, width, writer, caps, "steer")
    }

    fn render_help<W: io::Write>(
        &self,
        row: u16,
        width: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        input_mode: &str,
    ) -> io::Result<()> {
        let toggle_target = match input_mode {
            "steer" => "queue",
            "queue" => "steer",
            _ => "mode",
        };
        let line = format!(
            "{}  ^O verbose  ^E thinking  ^T {toggle_target}  {}",
            self.key_hints,
            newline_key_hint(caps),
        );
        let line = truncate_with_ellipsis(line.trim(), width);
        write!(
            writer,
            "{}{}{}{line}{}",
            cursor_to(row),
            erase_line(),
            Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
            Csi::Sgr(Sgr::Reset),
        )
    }
}

fn live_output_tokens(indicator: &StreamingIndicator, status_bar: &StatusBar) -> u64 {
    match indicator {
        StreamingIndicator::Generating {
            est_output_tokens, ..
        } => status_bar.output_tokens.saturating_add(*est_output_tokens),
        // A retry wait produces nothing: the failed attempt's estimate was
        // discarded with its partial output, so the chip shows the
        // provider-reported total only.
        StreamingIndicator::Idle
        | StreamingIndicator::Retrying { .. }
        | StreamingIndicator::Complete { .. } => status_bar.output_tokens,
    }
}

fn format_live_token_count(tokens: u64, arrow: char) -> String {
    format!("{}{arrow}", format_count(tokens))
}

fn elapsed_duration(indicator: &StreamingIndicator) -> Option<Duration> {
    match indicator {
        // The turn is still in flight during a retry wait, so its elapsed
        // segment stays on the divider rather than blinking out and back.
        StreamingIndicator::Generating { elapsed, .. }
        | StreamingIndicator::Retrying { elapsed, .. } => Some(*elapsed),
        StreamingIndicator::Idle | StreamingIndicator::Complete { .. } => None,
    }
}

fn format_elapsed_compact(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    let seconds = secs % 60;
    if minutes < 60 {
        if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{seconds}s")
        }
    } else {
        let hours = minutes / 60;
        let rem_minutes = minutes % 60;
        if rem_minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{rem_minutes:02}m")
        }
    }
}

/// Compositor for the bottom fixed panel.
///
/// Tracks the active components and their row counts. NT-002 builds the
/// rendering frame only: agent-line, popup, and input-area contents are
/// drawn as cleared placeholder rows here — NT-004, NT-006, and NT-010
/// wire their live contents in later. The streaming indicator and status
/// bar render their own content.
#[derive(Clone, Debug)]
pub struct FixedPanel {
    /// Number of agent status line rows (NT-006 wires their contents).
    agent_lines: u16,
    /// Number of optional backing-log rows. The normal TUI path keeps
    /// this at zero and folds live work into the agent status tree.
    activity_lines: u16,
    /// Current streaming indicator state.
    streaming_indicator: StreamingIndicator,
    /// Rows reserved for active-turn steer/queue status.
    active_input_status: u16,
    /// Number of autocomplete popup rows (NT-010 wires their contents).
    autocomplete_popup: u16,
    /// Number of input area rows (NT-004 wires their contents); minimum 1.
    input_area: u16,
    /// Current input mode label shown in the top divider.
    input_mode_label: String,
    /// The status bar component.
    status_bar: StatusBar,
    /// Panel height at the last render — used for the dirty check.
    last_height: u16,
}

impl FixedPanel {
    /// Create a panel with a single-row input area and the given status
    /// bar, with no agent lines, no popup, and an idle streaming
    /// indicator. The initial height matches the minimal panel set up by
    /// the terminal guard, so the panel starts un-dirty.
    pub fn new(status_bar: StatusBar) -> Self {
        let mut panel = Self {
            agent_lines: 0,
            activity_lines: 0,
            streaming_indicator: StreamingIndicator::Idle,
            active_input_status: 0,
            autocomplete_popup: 0,
            input_area: 1,
            input_mode_label: "steer".to_string(),
            status_bar,
            last_height: 0,
        };
        panel.last_height = panel.total_height();
        panel
    }

    /// Total panel height — the sum of every active component's rows.
    ///
    /// The input divider, metadata divider, input area, and help row are
    /// always present; the agent status lines, optional backing log, completion
    /// row, and autocomplete popup contribute zero rows when inactive.
    pub fn total_height(&self) -> u16 {
        SEPARATOR_ROWS
            .saturating_add(self.autocomplete_popup)
            .saturating_add(self.input_area.max(1))
            .saturating_add(METADATA_SEPARATOR_ROWS)
            .saturating_add(self.streaming_indicator.height())
            .saturating_add(self.agent_lines)
            .saturating_add(self.activity_lines)
            .saturating_add(self.active_input_status)
            .saturating_add(HELP_BAR_ROWS)
    }

    /// Whether the panel height has changed since the last [`render`].
    ///
    /// NT-011 checks this before each render: when dirty, the DECSTBM
    /// scroll region must be reissued (via the NT-001 terminal guard)
    /// for the new [`total_height`] before drawing.
    ///
    /// [`render`]: FixedPanel::render
    /// [`total_height`]: FixedPanel::total_height
    pub fn height_dirty(&self) -> bool {
        self.total_height() != self.last_height
    }

    /// Zero-based row of the first agent status line.
    #[must_use]
    pub fn agent_rows_top(&self, terminal_rows: u16) -> u16 {
        terminal_rows
            .saturating_sub(self.total_height())
            .saturating_add(SEPARATOR_ROWS)
            .saturating_add(self.autocomplete_popup)
            .saturating_add(self.input_area.max(1))
            .saturating_add(METADATA_SEPARATOR_ROWS)
            .saturating_add(self.streaming_indicator.height())
    }

    /// Set the number of agent status line rows.
    ///
    /// The collapse heuristic in NT-006 keeps this within the design's
    /// 0-5 visible range; the compositor itself imposes no cap (CO6).
    pub fn set_agent_lines(&mut self, rows: u16) {
        self.agent_lines = rows;
    }

    /// Set the number of optional backing-log rows (0..=`MAX_VISIBLE`).
    ///
    /// The main redraw path currently sets this to zero; the compositor
    /// itself still supports callers that need to reserve and clear
    /// backing-log rows.
    pub fn set_activity_lines(&mut self, rows: u16) {
        self.activity_lines = rows;
    }

    /// Zero-based row of the first optional backing-log line.
    ///
    /// Sits immediately below the agent status rows. The agent panel's
    /// overflow summary row (when present) is already counted in
    /// [`Self::agent_lines`] because the event loop sizes the panel
    /// from
    /// [`crate::agents::status_line::height_from_view`], which folds
    /// the overflow row into the returned height. So the math stays a
    /// straight addition.
    #[must_use]
    pub fn activity_rows_top(&self, terminal_rows: u16) -> u16 {
        self.agent_rows_top(terminal_rows)
            .saturating_add(self.agent_lines)
    }

    /// Set the number of autocomplete popup rows (0-8 in practice).
    pub fn set_autocomplete_popup(&mut self, rows: u16) {
        self.autocomplete_popup = rows;
    }

    /// Set active-turn steer/queue status rows.
    pub fn set_active_input_status(&mut self, rows: u16) {
        self.active_input_status = rows;
    }

    /// Set the input mode label shown in the top divider.
    pub fn set_input_mode_label(&mut self, label: impl Into<String>) {
        self.input_mode_label = label.into();
    }

    /// Current active-input status rows.
    #[must_use]
    pub const fn active_input_status_rows(&self) -> u16 {
        self.active_input_status
    }

    /// Zero-based row of the active-input status line.
    #[must_use]
    pub fn active_input_status_top(&self, terminal_rows: u16) -> u16 {
        self.activity_rows_top(terminal_rows)
            .saturating_add(self.activity_lines)
    }

    /// Current number of autocomplete popup rows.
    #[must_use]
    pub fn autocomplete_popup_rows(&self) -> u16 {
        self.autocomplete_popup
    }

    /// Zero-based row of the first autocomplete popup line.
    ///
    /// The popup sits immediately above the input area; this helper
    /// returns the row at which the popup's top line should be painted,
    /// which the event loop forwards into
    /// [`crate::input::autocomplete::AutocompletePopup::render`] after
    /// the fixed panel has cleared its placeholder rows.
    #[must_use]
    pub fn autocomplete_popup_top(&self, terminal_rows: u16) -> u16 {
        terminal_rows
            .saturating_sub(self.total_height())
            .saturating_add(SEPARATOR_ROWS)
    }

    /// Zero-based row of the first input editor line.
    #[must_use]
    pub fn input_area_top(&self, terminal_rows: u16) -> u16 {
        self.autocomplete_popup_top(terminal_rows)
            .saturating_add(self.autocomplete_popup)
    }

    /// Set the number of input area rows. Values below one are clamped
    /// to one — the input area is always present.
    pub fn set_input_area(&mut self, rows: u16) {
        self.input_area = rows.max(1);
    }

    /// Replace the streaming indicator state.
    pub fn set_streaming_indicator(&mut self, indicator: StreamingIndicator) {
        self.streaming_indicator = indicator;
    }

    /// Shared access to the status bar component.
    pub fn status_bar(&self) -> &StatusBar {
        &self.status_bar
    }

    /// Mutable access to the status bar for live data updates (NT-011).
    pub fn status_bar_mut(&mut self) -> &mut StatusBar {
        &mut self.status_bar
    }

    /// Redraw the entire fixed panel.
    ///
    /// The redraw is wrapped in synchronized rendering (or cursor
    /// hide/show) per [`TerminalCaps`] and is cursor-addressed strictly
    /// to rows `(terminal_rows - total_height)..terminal_rows` — scroll
    /// region rows are never touched (CO7). After a successful render the
    /// height-dirty flag clears.
    ///
    /// `terminal_cols` is required to lay out the right-aligned portion
    /// of the status bar; the brief's illustrative signature omitted it.
    pub fn render<W: io::Write>(
        &mut self,
        writer: &mut W,
        caps: &TerminalCaps,
        terminal_rows: u16,
        terminal_cols: u16,
    ) -> io::Result<()> {
        let total = self.total_height();
        let top = terminal_rows.saturating_sub(total);

        let agent_lines = self.agent_lines;
        let activity_lines = self.activity_lines;
        let active_input_status = self.active_input_status;
        let popup = self.autocomplete_popup;
        let input_area = self.input_area.max(1);
        let indicator = &self.streaming_indicator;
        let status_bar = &self.status_bar;
        let input_mode = self.input_mode_label.as_str();

        sync_render(caps, writer, |w| {
            let mut row = top;
            let live_output = live_output_tokens(indicator, status_bar);
            let mut top_parts = vec![
                input_mode.to_string(),
                format!(
                    "{} {}",
                    format_live_token_count(status_bar.input_tokens, '↑'),
                    format_live_token_count(live_output, '↓')
                ),
            ];
            if let Some(elapsed) = elapsed_duration(indicator) {
                top_parts.push(format_elapsed_compact(elapsed));
            }
            let input_divider = left_chip_separator(&top_parts.join(" • "), terminal_cols);
            render_separator_line(row, &input_divider, w)?;
            row = row.saturating_add(1);
            // Autocomplete popup — cleared placeholders (NT-010 wires data).
            for _ in 0..popup {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Input area — cleared placeholders (NT-004 wires data).
            for _ in 0..input_area {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            status_bar.render_metadata_divider(row, terminal_cols, w)?;
            row = row.saturating_add(1);
            if indicator.height() == 1 {
                indicator.render(row, w, caps, terminal_cols)?;
                row = row.saturating_add(1);
            }
            // Agent status lines — cleared placeholders (NT-006 wires data).
            for _ in 0..agent_lines {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Optional backing log — cleared placeholders. The main
            // TUI path keeps this at zero so per-agent rows are the
            // only live multi-agent surface.
            for _ in 0..activity_lines {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Active-input status — cleared placeholder; app/render wires content.
            for _ in 0..active_input_status {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Help row — bottom row, renders its own content.
            status_bar.render_help(row, terminal_cols, w, caps, input_mode)
        })?;

        self.last_height = total;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    use std::time::Instant;

    use super::*;

    /// The wait row takes a body row in the panel: the layout must
    /// account for it exactly like the completion row.
    #[test]
    fn total_height_counts_the_retry_row() {
        let mut panel = FixedPanel::new(StatusBar::default());
        let base = panel.total_height();
        panel.set_streaming_indicator(StreamingIndicator::Retrying {
            attempt: 2,
            max_attempts: None,
            error_class: "timeout".to_string(),
            wait_until: Instant::now() + Duration::from_secs(1),
            remaining: Duration::from_secs(1),
            elapsed: Duration::ZERO,
        });
        assert_eq!(panel.total_height(), base + 1);
    }

    #[test]
    fn total_height_default_is_input_divider_input_metadata_and_help() {
        let panel = FixedPanel::new(StatusBar::default());
        // 1 input divider + 1 input + 1 metadata divider + 1 help row.
        assert_eq!(panel.total_height(), 4);
    }

    #[test]
    fn total_height_sums_active_components() {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_agent_lines(3);
        panel.set_autocomplete_popup(5);
        panel.set_input_area(2);
        panel.set_streaming_indicator(StreamingIndicator::Generating {
            elapsed: Duration::from_secs(1),
            est_output_tokens: 0,
            in_flight: None,
        });
        // 1 input divider + 5 popup + 2 input + 1 metadata divider
        // + 3 agent + 1 help row. Generating lives in the input divider.
        assert_eq!(panel.total_height(), 13);
    }

    #[test]
    fn agent_rows_top_returns_first_row_after_separator() {
        let mut panel = FixedPanel::new(StatusBar::default());
        // Default panel: input divider + input + metadata + help = 4 rows.
        // With 24 terminal rows, panel top is row 20 (zero-based), so
        // an agent slot would start after the metadata divider at row 23.
        assert_eq!(panel.agent_rows_top(24), 23);

        // Adding agent rows grows total_height; the agent slot remains below
        // the input field and metadata divider.
        panel.set_agent_lines(3);
        // total_height = 1 input divider + 1 input + 1 metadata + 3 agent
        // + 1 help = 7. panel top = 17, agent_top = 20.
        assert_eq!(panel.agent_rows_top(24), 20);
    }

    #[test]
    fn activity_rows_top_sits_immediately_after_agent_lines() {
        let mut panel = FixedPanel::new(StatusBar::default());
        // No agent panel — optional backing rows sit in the area below
        // input + metadata. Default panel + 2 activity rows: panel top =
        // 18, agent_top = 21, activity_top = 21.
        panel.set_activity_lines(2);
        assert_eq!(panel.activity_rows_top(24), 21);

        // Agent panel with 3 rows (visible+overflow already folded in
        // by height_from_view), optional backing log with 2 rows.
        // total = 1 input divider + 1 input + 1 metadata + 3 agent
        // + 2 activity + 1 help = 9. panel top = 15, activity_top = 21.
        panel.set_agent_lines(3);
        assert_eq!(panel.activity_rows_top(24), 21);
    }

    #[test]
    fn total_height_includes_activity_lines() {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_agent_lines(2);
        panel.set_activity_lines(3);
        // 1 input divider + 1 input + 1 metadata + 2 agent + 3 activity
        // + 1 help = 9.
        assert_eq!(panel.total_height(), 9);
    }

    #[test]
    fn height_dirty_tracks_component_changes() {
        let mut panel = FixedPanel::new(StatusBar::default());
        assert!(!panel.height_dirty(), "fresh panel must not be dirty");
        panel.set_agent_lines(2);
        assert!(
            panel.height_dirty(),
            "growing the panel sets the dirty flag"
        );
    }

    #[test]
    fn render_clears_dirty_and_updates_height() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_agent_lines(2);
        assert!(panel.height_dirty());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80)?;
        assert!(!panel.height_dirty(), "render clears the dirty flag");
        Ok(())
    }

    #[test]
    fn render_stays_within_panel_rows() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        // 24 rows, panel height 4 → panel occupies zero-based rows 20-23,
        // i.e. one-based rows 21-24.
        panel.render(&mut buf, &caps, 24, 80)?;
        let out = String::from_utf8(buf)?;
        assert!(
            out.contains("\x1b[21;1H"),
            "separator row must be addressed"
        );
        assert!(out.contains("\x1b[22;1H"), "input row must be addressed");
        assert!(
            out.contains("\x1b[23;1H"),
            "metadata divider row must be addressed"
        );
        assert!(out.contains("\x1b[24;1H"), "help row must be addressed");
        // No cursor position may target a scroll region row (1..=20).
        for one_based in 1..=20u16 {
            let escape = format!("\x1b[{one_based};1H");
            assert!(
                !out.contains(&escape),
                "redraw must not address scroll region row {one_based}"
            );
        }
        Ok(())
    }

    #[test]
    fn render_paints_dim_horizontal_separator_as_first_panel_row() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80)?;
        let out = String::from_utf8(buf)?;
        // Separator sits at one-based row 21 with 24 terminal rows and a
        // 4-row default panel.
        assert!(
            out.contains("\x1b[21;1H"),
            "separator row must be addressed: {out:?}"
        );
        assert!(
            out.contains('\u{2500}'),
            "separator must include U+2500 (─): {out:?}"
        );
        assert!(
            out.contains("\x1b[2m"),
            "separator must be wrapped in dim SGR: {out:?}"
        );
        Ok(())
    }

    #[test]
    fn input_divider_shows_mode_and_token_chip() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_input_mode_label("queue");
        panel.status_bar_mut().input_tokens = 12;
        panel.status_bar_mut().output_tokens = 34;
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 40)?;
        let out = String::from_utf8(buf)?;
        assert!(
            out.contains("🮠 queue • 12↑ 34↓ 🮣"),
            "top chip must show mode and token counters: {out:?}"
        );
        Ok(())
    }

    #[test]
    fn input_divider_generating_adds_live_output_and_compact_elapsed() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.status_bar_mut().input_tokens = 1_000;
        panel.status_bar_mut().output_tokens = 2_000;
        panel.set_streaming_indicator(StreamingIndicator::Generating {
            elapsed: Duration::from_secs(80),
            est_output_tokens: 300,
            in_flight: None,
        });
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80)?;
        let out = String::from_utf8(buf)?;
        assert!(
            out.contains("🮠 steer • 1,000↑ 2,300↓ • 1m20s 🮣"),
            "top chip must show root-turn totals plus live output estimate: {out:?}"
        );
        Ok(())
    }

    #[test]
    fn input_divider_omits_estimate_marker() -> TestResult {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.status_bar_mut().input_tokens = 12_345;
        panel.status_bar_mut().input_tokens_estimated = true;
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80)?;
        let out = String::from_utf8(buf)?;
        assert!(
            out.contains("🮠 steer • 12,345↑ 0↓ 🮣"),
            "estimated input must not carry a visual approximation marker: {out:?}"
        );
        Ok(())
    }

    #[test]
    fn compact_elapsed_formats_minutes_and_hours() {
        assert_eq!(format_elapsed_compact(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed_compact(Duration::from_mins(1)), "1m");
        assert_eq!(format_elapsed_compact(Duration::from_secs(80)), "1m20s");
        assert_eq!(format_elapsed_compact(Duration::from_mins(65)), "1h05m");
    }

    #[test]
    fn metadata_divider_shows_model_and_session() -> TestResult {
        let bar = StatusBar {
            model_name: "claude-opus".to_string(),
            session_name: "demo".to_string(),
            input_tokens: 12_345,
            input_tokens_estimated: false,
            output_tokens: 678,
            output_tokens_estimated: false,
            key_hints: "^C exit".to_string(),
            service_tier: None,
            reasoning_effort: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        bar.render_metadata_divider(0, 80, &mut buf)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("claude-opus"));
        assert!(out.contains("demo"));
        assert!(out.contains("🮠"), "metadata chip left cap: {out:?}");
        assert!(out.contains("🮣"), "metadata chip right cap: {out:?}");
        Ok(())
    }

    #[test]
    fn status_bar_render_shows_key_hints() -> TestResult {
        let bar = StatusBar {
            model_name: "claude-opus".to_string(),
            session_name: "demo".to_string(),
            input_tokens: 12_345,
            input_tokens_estimated: false,
            output_tokens: 678,
            output_tokens_estimated: false,
            key_hints: "^C exit".to_string(),
            service_tier: None,
            reasoning_effort: None,
        };
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        bar.render(0, 80, &mut buf, &caps)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("Alt+Enter"), "newline key hint must appear");
        assert!(out.contains("^O verbose"), "verbosity hint: {out:?}");
        assert!(out.contains("^E thinking"), "thinking hint: {out:?}");
        assert!(out.contains("^T queue"), "mode-toggle hint: {out:?}");
        Ok(())
    }

    #[test]
    fn status_bar_render_shows_runtime_mode_badges() -> TestResult {
        let bar = StatusBar {
            model_name: "gpt-5.5".to_string(),
            session_name: "demo".to_string(),
            input_tokens: 1,
            input_tokens_estimated: false,
            output_tokens: 2,
            output_tokens_estimated: false,
            key_hints: "^C exit".to_string(),
            service_tier: Some("fast".to_string()),
            reasoning_effort: Some("high".to_string()),
        };
        let mut buf: Vec<u8> = Vec::new();
        bar.render_metadata_divider(0, 120, &mut buf)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("tier:fast"), "service tier badge: {out:?}");
        assert!(out.contains("effort:high"), "effort badge: {out:?}");
        Ok(())
    }
}
