//! Fixed-panel compositor.
//!
//! The fixed panel occupies the bottom rows of the terminal — below the
//! DECSTBM scroll region — and holds, from top to bottom: agent status
//! lines, the streaming indicator, the autocomplete popup, the input
//! area, and the status bar. [`FixedPanel`] tracks which components are
//! active and their row counts, computes the total panel height, and
//! performs a cursor-addressed redraw confined strictly to the panel
//! rows (CO8: the fixed panel is the only cursor-addressed rendering).
//!
//! The compositor does not own the terminal guard. NT-011 reads
//! [`FixedPanel::height_dirty`] and [`FixedPanel::total_height`] to
//! decide when to reissue the DECSTBM scroll region (via the guard from
//! NT-001) before calling [`FixedPanel::render`].

use std::io;
use std::time::Duration;

use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::{Intensity, RgbColor};
use unicode_width::UnicodeWidthStr as _;

use super::style::{colour_for, newline_key_hint, sync_render};
use super::text::{format_count, truncate_with_ellipsis};
use crate::terminal::caps::TerminalCaps;

/// Rows occupied by the always-present status bar.
const STATUS_BAR_ROWS: u16 = 1;

/// Rows occupied by the always-present scroll-region/panel separator.
pub(crate) const SEPARATOR_ROWS: u16 = 1;

/// Box-drawings light horizontal (U+2500) — the separator glyph.
const SEPARATOR_CHAR: char = '\u{2500}';

/// Foreground colour for the streaming indicator's `generating` text.
const GENERATING_COLOUR: RgbColor = RgbColor::new(215, 175, 0);

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

/// Render the panel separator at zero-based `row` spanning `width` columns.
///
/// Writes a dim, full-width horizontal line using [`SEPARATOR_CHAR`] —
/// the visual boundary between the scroll region above and the rest of
/// the fixed panel below.
fn render_separator<W: io::Write>(row: u16, width: u16, writer: &mut W) -> io::Result<()> {
    let separator: String = std::iter::repeat_n(SEPARATOR_CHAR, usize::from(width)).collect();
    write!(
        writer,
        "{}{}{}{separator}{}",
        cursor_to(row),
        erase_line(),
        Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
        Csi::Sgr(Sgr::Reset),
    )
}

/// Compose a single line with `left` left-aligned and `right`
/// right-aligned, padded with spaces to `width` display columns.
///
/// When the two segments cannot both fit within `width`, the left side
/// is truncated first because the right side contains live usage and
/// key hints. If the right side itself exceeds the available width, it
/// is truncated explicitly. The returned line never exceeds `width`
/// display columns.
fn compose_left_right(left: &str, right: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let left_width = left.width();
    let right_width = right.width();
    if left_width + right_width + 1 >= width {
        if right_width >= width {
            return truncate_with_ellipsis(right, u16::try_from(width).unwrap_or(u16::MAX));
        }
        let left_budget = width.saturating_sub(right_width).saturating_sub(1);
        let left = truncate_with_ellipsis(left, u16::try_from(left_budget).unwrap_or(u16::MAX));
        if left.is_empty() {
            return right.to_owned();
        }
        return format!("{left} {right}");
    }
    let gap = width - left_width - right_width;
    format!("{left}{}{right}", " ".repeat(gap))
}

/// The single-row status bar pinned to the bottom of the fixed panel.
#[derive(Clone, Debug, Default)]
pub struct StatusBar {
    /// Name of the active model.
    pub model_name: String,
    /// Name of the active session.
    pub session_name: String,
    /// Cumulative input token count for the session.
    pub input_tokens: u64,
    /// Cumulative output token count for the session.
    pub output_tokens: u64,
    /// Free-form key-hint text shown alongside the newline-key hint.
    pub key_hints: String,
    /// Active provider service tier, when explicitly set.
    pub service_tier: Option<String>,
    /// Active reasoning effort, when explicitly set.
    pub reasoning_effort: Option<String>,
}

impl StatusBar {
    /// Render the status bar as a single styled line at zero-based `row`.
    ///
    /// Model and session names are left-aligned; token usage and key
    /// hints — including the capability-appropriate newline key — are
    /// right-aligned. The line is dimmed so it recedes behind the
    /// conversation content.
    pub fn render<W: io::Write>(
        &self,
        row: u16,
        width: u16,
        writer: &mut W,
        caps: &TerminalCaps,
    ) -> io::Result<()> {
        let mut left_parts = vec![self.model_name.as_str(), self.session_name.as_str()];
        let service_tier;
        if let Some(tier) = self.service_tier.as_deref() {
            service_tier = format!("tier:{tier}");
            left_parts.push(service_tier.as_str());
        }
        let reasoning_effort;
        if let Some(effort) = self.reasoning_effort.as_deref() {
            reasoning_effort = format!("effort:{effort}");
            left_parts.push(reasoning_effort.as_str());
        }
        let left = left_parts.join(" │ ");
        let right = format!(
            "{}↑ {}↓ │ {} ^O verbose ^E thinking {}",
            format_count(self.input_tokens),
            format_count(self.output_tokens),
            self.key_hints,
            newline_key_hint(caps),
        );
        let line = compose_left_right(&left, &right, width);
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

/// Tool call the assistant is currently executing, surfaced on the
/// streaming indicator's `Generating` mode while a result is pending.
///
/// `description` is the model-supplied `tool_use_description` envelope
/// field (see [`norn::tool::envelope::ENVELOPE_DESCRIPTION_KEY`]); it
/// arrives only at `ToolCallComplete`, so during the gap between the
/// first `ToolCallDelta` (which carries the name) and `ToolCallComplete`
/// the renderer paints the tool name alone.
#[derive(Clone, Debug)]
pub struct ToolUseInFlight {
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// Model-supplied intent description. `None` when not yet available
    /// or when the model failed to populate the envelope field.
    pub description: Option<String>,
}

/// State of the streaming indicator row.
#[derive(Clone, Debug, Default)]
pub enum StreamingIndicator {
    /// The model is not producing output — the row is absent (0 rows).
    #[default]
    Idle,
    /// The model is producing output — shows elapsed time, an estimated
    /// running output-token count, and (when in flight) the active
    /// tool's name and description (1 row).
    Generating {
        /// Time elapsed since generation began.
        elapsed: Duration,
        /// Output-token estimate accumulated by the dispatch layer —
        /// `bytes / 4` heuristic over `TextDelta`, `ThinkingDelta`, and
        /// `ToolCallDelta` content. Displayed with a `~` prefix to
        /// advertise the approximation.
        est_output_tokens: u64,
        /// Tool call currently between `ToolCallComplete` and its
        /// matching `ToolResult` (or the first `ToolCallDelta` carrying
        /// a name, before `ToolCallComplete` arrives). `None` when no
        /// tool is in flight.
        in_flight: Option<ToolUseInFlight>,
    },
    /// Generation has finished — shows the usage summary (1 row).
    Complete {
        /// Pre-formatted usage summary string supplied by NT-011.
        usage_summary: String,
    },
}

impl StreamingIndicator {
    /// Rows this indicator contributes to the fixed panel height.
    ///
    /// [`StreamingIndicator::Idle`] contributes zero rows; the other
    /// states contribute one.
    pub const fn height(&self) -> u16 {
        match self {
            Self::Idle => 0,
            Self::Generating { .. } | Self::Complete { .. } => 1,
        }
    }

    /// Render the indicator at zero-based `row`.
    ///
    /// [`StreamingIndicator::Idle`] writes nothing. `Generating` paints
    /// one of three shapes depending on whether a tool is in flight and
    /// whether its description is known:
    /// - no tool in flight: `● generating... {elapsed}s  ~{est}↓`
    /// - tool with description: `● {tool}: '{desc}'  {elapsed}s  ~{est}↓`
    /// - tool without description: `● {tool}  {elapsed}s  ~{est}↓`
    ///
    /// `Complete` renders the pre-formatted usage summary verbatim. When
    /// `terminal_cols` does not leave room for the description, the
    /// description is replaced with a Unicode ellipsis so the surrounding
    /// tail (elapsed + token estimate) stays visible.
    pub fn render<W: io::Write>(
        &self,
        row: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        terminal_cols: u16,
    ) -> io::Result<()> {
        match self {
            Self::Idle => Ok(()),
            Self::Generating {
                elapsed,
                est_output_tokens,
                in_flight,
            } => {
                let colour = colour_for(GENERATING_COLOUR, caps);
                let body = format_generating_body(
                    elapsed.as_secs(),
                    *est_output_tokens,
                    in_flight.as_ref(),
                    terminal_cols,
                );
                write!(
                    writer,
                    "{}{}{colour}{body}{}",
                    cursor_to(row),
                    erase_line(),
                    Csi::Sgr(Sgr::Reset),
                )
            }
            Self::Complete { usage_summary } => {
                write!(writer, "{}{}{usage_summary}", cursor_to(row), erase_line())
            }
        }
    }
}

/// Compose the text body for the `Generating` indicator at the current
/// terminal width.
///
/// `est_output_tokens` is rendered with a `~` prefix and the `↓`
/// direction marker matching the status bar. When `in_flight` carries a
/// description, the description is wrapped in single quotes and
/// truncated with a single-codepoint ellipsis to fit the remaining
/// width budget. When the description is absent (or empty) the line
/// shows only the tool name. When `in_flight` is `None`, the original
/// `generating...` shape is used.
fn format_generating_body(
    secs: u64,
    est_output_tokens: u64,
    in_flight: Option<&ToolUseInFlight>,
    terminal_cols: u16,
) -> String {
    let tail = format!("  {secs}s  ~{}↓", format_count(est_output_tokens));
    let Some(in_flight) = in_flight else {
        return format!("● generating...{tail}");
    };
    let head = format!("● {}", in_flight.tool_name);
    let description = in_flight
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let Some(description) = description else {
        return format!("{head}{tail}");
    };
    // Budget for the description segment = total - head - quoted wrap
    // (": ''" = 4 cols) - tail. Caller-side truncation keeps the tail
    // (elapsed + token estimate) visible so the user can still see
    // progress even when descriptions are long.
    let head_cols = u16::try_from(head.width()).unwrap_or(u16::MAX);
    let tail_cols = u16::try_from(tail.width()).unwrap_or(u16::MAX);
    let wrap_cols: u16 = 4; // ": '" + "'"
    let budget = terminal_cols
        .saturating_sub(head_cols)
        .saturating_sub(tail_cols)
        .saturating_sub(wrap_cols);
    let trimmed = truncate_with_ellipsis(description, budget);
    if trimmed.is_empty() {
        // No room for any description text — collapse to the no-desc
        // form rather than rendering empty quotes.
        return format!("{head}{tail}");
    }
    format!("{head}: '{trimmed}'{tail}")
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
    /// Number of activity-log rows. The activity log is the recent
    /// stream of tool-call initiations rendered between the agent
    /// status panel and the streaming indicator. Set by the event
    /// loop's redraw pass from
    /// [`crate::agents::activity_log::height_from_log`].
    activity_lines: u16,
    /// Current streaming indicator state.
    streaming_indicator: StreamingIndicator,
    /// Number of autocomplete popup rows (NT-010 wires their contents).
    autocomplete_popup: u16,
    /// Number of input area rows (NT-004 wires their contents); minimum 1.
    input_area: u16,
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
            autocomplete_popup: 0,
            input_area: 1,
            status_bar,
            last_height: 0,
        };
        panel.last_height = panel.total_height();
        panel
    }

    /// Total panel height — the sum of every active component's rows.
    ///
    /// The separator row at the top of the panel and the status bar row
    /// at the bottom are always present; the agent status lines, the
    /// activity log, the streaming indicator, and the autocomplete popup
    /// contribute zero rows when inactive.
    pub fn total_height(&self) -> u16 {
        SEPARATOR_ROWS
            .saturating_add(self.agent_lines)
            .saturating_add(self.activity_lines)
            .saturating_add(self.streaming_indicator.height())
            .saturating_add(self.autocomplete_popup)
            .saturating_add(self.input_area.max(1))
            .saturating_add(STATUS_BAR_ROWS)
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
    ///
    /// The agent rows sit between the separator (always row 0 of the
    /// panel) and the streaming indicator, popup, input, and status bar.
    /// Callers — the event loop in [`crate::app::event_loop`] — use this
    /// to position [`crate::agents::status_line::AgentStatusPanel::render`]
    /// directly after the fixed-panel frame is drawn.
    #[must_use]
    pub fn agent_rows_top(&self, terminal_rows: u16) -> u16 {
        terminal_rows
            .saturating_sub(self.total_height())
            .saturating_add(SEPARATOR_ROWS)
    }

    /// Set the number of agent status line rows.
    ///
    /// The collapse heuristic in NT-006 keeps this within the design's
    /// 0-5 visible range; the compositor itself imposes no cap (CO6).
    pub fn set_agent_lines(&mut self, rows: u16) {
        self.agent_lines = rows;
    }

    /// Set the number of activity-log rows (0..=`MAX_VISIBLE`).
    ///
    /// The event loop in [`crate::app::event_loop`] snapshots the
    /// log, derives the row count via
    /// [`crate::agents::activity_log::height_from_log`], and passes
    /// the result here before [`Self::render`]. The compositor itself
    /// imposes no cap (CO6).
    pub fn set_activity_lines(&mut self, rows: u16) {
        self.activity_lines = rows;
    }

    /// Zero-based row of the first activity-log line.
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
        let input = self.input_area.max(1);
        terminal_rows
            .saturating_sub(STATUS_BAR_ROWS)
            .saturating_sub(input)
            .saturating_sub(self.autocomplete_popup)
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
        let popup = self.autocomplete_popup;
        let input_area = self.input_area.max(1);
        let indicator = &self.streaming_indicator;
        let status_bar = &self.status_bar;

        sync_render(caps, writer, |w| {
            let mut row = top;
            // Separator — full-width dim horizontal line, always present.
            render_separator(row, terminal_cols, w)?;
            row = row.saturating_add(1);
            // Agent status lines — cleared placeholders (NT-006 wires data).
            for _ in 0..agent_lines {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Activity log — cleared placeholders; the event loop's
            // redraw pass wires the actual contents via
            // [`crate::agents::activity_log::render_view`].
            for _ in 0..activity_lines {
                clear_row(row, w)?;
                row = row.saturating_add(1);
            }
            // Streaming indicator — renders its own content.
            if indicator.height() == 1 {
                indicator.render(row, w, caps, terminal_cols)?;
                row = row.saturating_add(1);
            }
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
            // Status bar — bottom row, renders its own content.
            status_bar.render(row, terminal_cols, w, caps)
        })?;

        self.last_height = total;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn compose_left_right_truncates_left_to_width() {
        let out = compose_left_right("abcdef", "XYZ", 6);
        assert_eq!(out, "a… XYZ");
        assert!(out.width() <= 6);
    }

    #[test]
    fn compose_left_right_truncates_right_when_it_cannot_fit() {
        let out = compose_left_right("abcdef", "XYZXYZ", 5);
        assert_eq!(out, "XYZX…");
        assert!(out.width() <= 5);
    }

    #[test]
    fn total_height_default_is_separator_plus_input_plus_status_bar() {
        let panel = FixedPanel::new(StatusBar::default());
        // 1 separator + 1 input + 1 status bar.
        assert_eq!(panel.total_height(), 3);
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
        // 1 separator + 3 agent + 1 indicator + 5 popup + 2 input + 1 status bar.
        assert_eq!(panel.total_height(), 13);
    }

    #[test]
    fn agent_rows_top_returns_first_row_after_separator() {
        let mut panel = FixedPanel::new(StatusBar::default());
        // Default panel: separator + input + status bar = 3 rows.
        // With 24 terminal rows, panel top is row 21 (zero-based),
        // so agent rows start at row 22.
        assert_eq!(panel.agent_rows_top(24), 22);

        // Adding agent rows grows total_height but the agent slot still
        // sits immediately after the separator — so the top row moves up.
        panel.set_agent_lines(3);
        // total_height = 1 sep + 3 agent + 1 input + 1 status = 6.
        // panel top = 24 - 6 = 18, agent_top = 18 + 1 = 19.
        assert_eq!(panel.agent_rows_top(24), 19);
    }

    #[test]
    fn activity_rows_top_sits_immediately_after_agent_lines() {
        let mut panel = FixedPanel::new(StatusBar::default());
        // No agent panel — activity log sits directly after separator.
        // Default panel + 2 activity rows: panel top = 24 - 5 = 19,
        // agent_top = 19 + 1 = 20, activity_top = 20 + 0 = 20.
        panel.set_activity_lines(2);
        assert_eq!(panel.activity_rows_top(24), 20);

        // Agent panel with 3 rows (visible+overflow already folded in
        // by height_from_view), activity log with 2 rows.
        // total = 1 sep + 3 agent + 2 activity + 1 input + 1 status = 8.
        // panel top = 24 - 8 = 16, agent_top = 17, activity_top = 20.
        panel.set_agent_lines(3);
        assert_eq!(panel.activity_rows_top(24), 20);
    }

    #[test]
    fn total_height_includes_activity_lines() {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_agent_lines(2);
        panel.set_activity_lines(3);
        // 1 sep + 2 agent + 3 activity + 1 input + 1 status = 8.
        assert_eq!(panel.total_height(), 8);
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
    fn render_clears_dirty_and_updates_height() {
        let mut panel = FixedPanel::new(StatusBar::default());
        panel.set_agent_lines(2);
        assert!(panel.height_dirty());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80).unwrap();
        assert!(!panel.height_dirty(), "render clears the dirty flag");
    }

    #[test]
    fn render_stays_within_panel_rows() {
        let mut panel = FixedPanel::new(StatusBar::default());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        // 24 rows, panel height 3 → panel occupies zero-based rows 21-23,
        // i.e. one-based rows 22-24 (separator, input, status bar).
        panel.render(&mut buf, &caps, 24, 80).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("\x1b[22;1H"),
            "separator row must be addressed"
        );
        assert!(out.contains("\x1b[23;1H"), "input row must be addressed");
        assert!(
            out.contains("\x1b[24;1H"),
            "status bar row must be addressed"
        );
        // No cursor position may target a scroll region row (1..=21).
        for one_based in 1..=21u16 {
            let escape = format!("\x1b[{one_based};1H");
            assert!(
                !out.contains(&escape),
                "redraw must not address scroll region row {one_based}"
            );
        }
    }

    #[test]
    fn render_paints_dim_horizontal_separator_as_first_panel_row() {
        let mut panel = FixedPanel::new(StatusBar::default());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 80).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // Separator sits at one-based row 22 with 24 terminal rows and a
        // 3-row default panel.
        assert!(
            out.contains("\x1b[22;1H"),
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
    }

    #[test]
    fn separator_spans_full_terminal_width() {
        let mut panel = FixedPanel::new(StatusBar::default());
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        panel.render(&mut buf, &caps, 24, 40).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let separator_line: String = std::iter::repeat_n('\u{2500}', 40).collect();
        assert!(
            out.contains(&separator_line),
            "separator must be repeated terminal_cols times: {out:?}"
        );
    }

    #[test]
    fn status_bar_render_shows_model_and_token_counts() {
        let bar = StatusBar {
            model_name: "claude-opus".to_string(),
            session_name: "demo".to_string(),
            input_tokens: 12_345,
            output_tokens: 678,
            key_hints: "^C exit".to_string(),
            service_tier: None,
            reasoning_effort: None,
        };
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        bar.render(0, 80, &mut buf, &caps).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("claude-opus"));
        assert!(out.contains("12,345"));
        assert!(out.contains("678"));
        assert!(out.contains("Alt+Enter"), "newline key hint must appear");
        assert!(out.contains("│"), "section separator must appear");
        assert!(out.contains("12,345↑"), "input tokens with arrow: {out:?}");
        assert!(out.contains("678↓"), "output tokens with arrow: {out:?}");
        assert!(out.contains("^O verbose"), "verbosity hint: {out:?}");
        assert!(out.contains("^E thinking"), "thinking hint: {out:?}");
    }

    #[test]
    fn status_bar_render_shows_runtime_mode_badges() {
        let bar = StatusBar {
            model_name: "gpt-5.5".to_string(),
            session_name: "demo".to_string(),
            input_tokens: 1,
            output_tokens: 2,
            key_hints: "^C exit".to_string(),
            service_tier: Some("fast".to_string()),
            reasoning_effort: Some("high".to_string()),
        };
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        bar.render(0, 120, &mut buf, &caps).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("tier:fast"), "service tier badge: {out:?}");
        assert!(out.contains("effort:high"), "effort badge: {out:?}");
    }

    #[test]
    fn streaming_indicator_height_tracks_state() {
        assert_eq!(StreamingIndicator::Idle.height(), 0);
        assert_eq!(
            StreamingIndicator::Generating {
                elapsed: Duration::from_secs(3),
                est_output_tokens: 0,
                in_flight: None,
            }
            .height(),
            1
        );
        assert_eq!(
            StreamingIndicator::Complete {
                usage_summary: "[1 in / 1 out, 0.1s]".to_string(),
            }
            .height(),
            1
        );
    }

    #[test]
    fn streaming_indicator_generating_renders_elapsed_time() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(5),
            est_output_tokens: 0,
            in_flight: None,
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("generating"));
        assert!(
            out.contains("5s"),
            "elapsed seconds must appear, got: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_idle_renders_nothing() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Idle
            .render(0, &mut buf, &caps, 80)
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn streaming_indicator_generating_shows_token_estimate_with_tilde() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 1_234,
            in_flight: None,
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("~1,234↓"),
            "tilde-prefixed estimate with ↓ marker: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_tool_and_description_shows_quotes() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 500,
            in_flight: Some(ToolUseInFlight {
                tool_name: "bash".to_string(),
                description: Some("listing docs folder".to_string()),
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("● bash:"), "tool name with colon: {out:?}");
        assert!(
            out.contains("'listing docs folder'"),
            "description wrapped in single quotes: {out:?}"
        );
        assert!(
            !out.contains("generating..."),
            "generating... must not appear when tool is in flight: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_tool_no_description_omits_quotes() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(1),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "read".to_string(),
                description: None,
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("● read"), "tool name appears: {out:?}");
        assert!(
            !out.contains("''"),
            "no empty quotes when description is None: {out:?}"
        );
        assert!(
            !out.contains(": '"),
            "no colon-quote when description is None: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_empty_description_omits_quotes() {
        // Some("") from split_envelope_fields when the model populates
        // the envelope key with a blank string — treat as missing.
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(1),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "read".to_string(),
                description: Some(String::new()),
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("''"),
            "empty description must not render empty quotes: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_truncates_long_description_with_ellipsis() {
        // Narrow terminal (40 cols) forces truncation; the description
        // must lose tail characters to a Unicode ellipsis but the tail
        // (elapsed + token estimate) must stay visible.
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        let long = "this description is far too long to fit on a forty-column row";
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "bash".to_string(),
                description: Some(long.to_string()),
            }),
        }
        .render(0, &mut buf, &caps, 40)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains('\u{2026}'), "ellipsis: {out:?}");
        assert!(out.contains("2s"), "elapsed survives: {out:?}");
        assert!(out.contains("~100↓"), "token estimate survives: {out:?}");
    }
}
