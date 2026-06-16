//! Application-level rendering helpers.
//!
//! These functions bridge [`AppState`] and [`TerminalGuard`] into
//! concrete terminal output — painting the fixed panel, the input
//! editor, user messages, and status lines into the scroll region or
//! fixed panel as appropriate. They are called from
//! [`super::event_loop`] but carry no event-loop-specific state.

use std::io::Write as _;
use std::time::Instant;

use chrono::Utc;

use termina::Terminal as _;

use crate::TuiError;
use crate::agents::activity_log::{height_from_log, render_view as render_activity_view};
use crate::agents::status_line::height_from_view;
use crate::input::editor::InputEditor;
use crate::input::wrap;
use crate::render::MarkdownRenderer;
use crate::render::fixed_panel::StreamingIndicator;
use crate::render::scroll_region::write_to_scroll;
use crate::render::text::{terminal_safe_input_text, truncate_to_width};
use crate::terminal::setup::TerminalGuard;

use super::autocomplete::render_popup;
use super::helpers::{dim_line_count, erase_dim_lines, sync_with_guard};
use super::state::AppState;

/// Maximum visual rows reserved for input before the editor scrolls internally.
pub(crate) const INPUT_AREA_MAX_ROWS: u16 = 12;
/// Minimum conversation rows to preserve above the fixed panel.
const MIN_SCROLL_REGION_ROWS: u16 = 4;

/// Input rows visible for the given terminal height and editor contents.
#[must_use]
pub(crate) fn capped_input_height(editor: &InputEditor, cols: u16, terminal_rows: u16) -> u16 {
    let cap = (terminal_rows / 2).clamp(1, INPUT_AREA_MAX_ROWS);
    editor.visual_height(cols).min(cap).max(1)
}

/// Sync the fixed panel's input height and editor viewport to visual wrapping.
pub(crate) fn sync_input_area(editor: &mut InputEditor, cols: u16, terminal_rows: u16) -> u16 {
    let input_height = capped_input_height(editor, cols, terminal_rows);
    editor.scroll_to_cursor(cols, input_height);
    input_height
}

/// Write a user message to the scroll region with the rendered prefix.
///
/// Restores the cursor into the scroll region (DECRC) first, since the
/// cursor is typically in the input area when this is called. A blank
/// line is prepended to visually separate the new turn from previous
/// output — on the first message this produces a top margin, on
/// subsequent messages it creates turn separation.
pub fn write_user_message(
    text: &str,
    state: &mut AppState,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    // Restore the scroll-region cursor via the guard's clamping helper.
    // If a redraw between turns grew the panel, the saved row may now
    // sit in the panel area; the clamp pulls the cursor back into the
    // scroll region before we paint the user message.
    guard.restore_scroll_cursor_clamped()?;
    write_to_scroll("\n", guard.terminal_mut())?;
    guard.note_scroll_newlines("\n")?;
    let rendered = crate::events::schema_render::render_user_message(text, &state.terminal_caps);
    write_to_scroll(&rendered, guard.terminal_mut())?;
    guard.note_scroll_newlines(&rendered)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Write a dim `[cancelled]` indicator into the scroll region.
///
/// The trailing newline keeps the indicator on its own scroll-region
/// row so the user can scan back through previous turns and spot
/// cancellations at a glance.
pub fn write_cancelled_line(guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let line = "\x1b[2m[cancelled]\x1b[22m\n";
    write_to_scroll(line, guard.terminal_mut())?;
    guard.note_scroll_newlines(line)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Render the input editor into the fixed panel's input area.
///
/// Visual rows from [`wrap::layout`] are written with absolute cursor
/// positioning inside the fixed panel. Rows outside the editor viewport
/// are skipped, and the terminal cursor is parked at the visual cursor
/// position relative to the visible window.
pub fn render_input(state: &AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let rows = guard.terminal_rows();
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let caps = state.terminal_caps.clone();
    sync_with_guard(&caps, guard, |guard| {
        render_input_frame(state, rows, cols, guard.terminal_mut())?;
        Ok(())
    })?;
    guard.terminal_mut().flush()?;
    Ok(())
}

fn render_input_frame<W: std::io::Write>(
    state: &AppState,
    rows: u16,
    cols: u16,
    writer: &mut W,
) -> std::io::Result<()> {
    let input_height = capped_input_height(&state.input_editor, cols, rows);
    let input_top = rows.saturating_sub(1).saturating_sub(input_height);
    let viewport_top = state.input_editor.viewport_top();
    let viewport_bottom = viewport_top.saturating_add(input_height);

    let lines = state.input_editor.lines();
    let (cursor_row, cursor_col) = state.input_editor.cursor_position();
    let layout = wrap::layout(lines, cursor_row, cursor_col, cols);

    for (visual_idx, row) in layout.rows.iter().enumerate() {
        let visual_row = u16::try_from(visual_idx).unwrap_or(u16::MAX);
        if visual_row < viewport_top || visual_row >= viewport_bottom {
            continue;
        }
        let panel_row = visual_row.saturating_sub(viewport_top);
        let row_1b = input_top.saturating_add(panel_row).saturating_add(1);
        let text = lines
            .get(row.logical_row)
            .map_or("", |line| char_slice(line, row.char_start, row.char_end));
        let safe_text = terminal_safe_input_text(text);
        let clipped = truncate_to_width(safe_text.as_ref(), cols);
        write!(writer, "\x1b[{row_1b};1H\x1b[2K{clipped}")?;
    }

    let cursor_visual_row = u16::try_from(layout.cursor.visual_row).unwrap_or(u16::MAX);
    if cursor_visual_row >= viewport_top && cursor_visual_row < viewport_bottom {
        let cursor_panel_row = cursor_visual_row.saturating_sub(viewport_top);
        let cursor_row_1b = input_top.saturating_add(cursor_panel_row).saturating_add(1);
        let display_col = layout.cursor.display_col.min(cols.saturating_sub(1));
        write!(writer, "\x1b[{cursor_row_1b};{}H", display_col + 1)?;
    }

    Ok(())
}

fn char_slice(line: &str, char_start: usize, char_end: usize) -> &str {
    let byte_start = line
        .char_indices()
        .nth(char_start)
        .map_or(line.len(), |(idx, _)| idx);
    let byte_end = line
        .char_indices()
        .nth(char_end)
        .map_or(line.len(), |(idx, _)| idx);
    line.get(byte_start..byte_end).unwrap_or("")
}

/// Reissue DECSTBM (when needed) and redraw the fixed panel.
///
/// Does NOT touch DECSC/DECRC — the scroll-region cursor is tracked
/// separately via explicit DECSC at points where the cursor is known
/// to be inside the scroll region.
///
/// When the panel shrinks (`height_dirty()` is set and the new height
/// is smaller than the old), the rows that are about to transition
/// from panel space back into the scroll region still contain old
/// panel paint. They are cleared *before* DECSTBM is reissued, while
/// they still belong to the panel: this keeps the erase strictly
/// within fixed-panel territory (CO8).
///
/// When the panel grows, the rows that are about to transition the
/// other way — from scroll-region space into panel space — hold live
/// conversation content. Before DECSTBM is reissued the OLD scroll
/// region is scrolled up by `claim = new_height − old_height` rows so
/// that content moves into the terminal's native scrollback (where
/// the user can still see it by scrolling back) instead of being
/// overwritten by the panel paint that follows. The scroll-up uses
/// the standard VT100 bottom-margin trick — `\n` while the cursor
/// sits on the scroll region's bottom row scrolls every row one
/// position up and drops the topmost row into native scrollback. The
/// grow path calls [`TerminalGuard::note_panel_grew`] so that the
/// event-loop bracket's `restore_scroll_cursor_clamped` knows the
/// content was already preserved and repositions without a redundant
/// `\r\n` (which would otherwise create a blank line in the output).
pub fn redraw_panel(state: &mut AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    tracing::debug!("redraw_panel paint");
    state.sync_indicator_into_panel();

    let now = Instant::now();
    let now_utc = Utc::now();
    // Snapshot the agent panel and the activity log ONCE each per
    // redraw and feed the same snapshots into both the height
    // calculation (which sizes the fixed panel) and the render call.
    // `AgentStatusPanel::snapshot` mutates the hold map via
    // `reclaim_expired_holds` and `ActivityLog::snapshot` mutates the
    // entry deque via `reclaim_expired`; a second internal call at an
    // expiry boundary would silently shrink the view relative to the
    // height the fixed panel was sized to.
    let (view, entries) = state.agent_panel.snapshot(now);
    let mut agent_rows = height_from_view(&view);
    state.fixed_panel.set_agent_lines(agent_rows);

    let rows = guard.terminal_rows();
    let activity_snap = state.activity_log.snapshot(now);
    let proposed_activity_rows = height_from_log(&activity_snap);
    // First-pass floor guard: skip the activity log when it would
    // squeeze the scroll region below the minimum conversation rows.
    // The broader row budget below can shed additional optional
    // surfaces when the terminal is especially short.
    state.fixed_panel.set_activity_lines(proposed_activity_rows);
    let mut activity_rows = if proposed_activity_rows > 0
        && rows.saturating_sub(state.fixed_panel.total_height()) < MIN_SCROLL_REGION_ROWS
    {
        state.fixed_panel.set_activity_lines(0);
        0
    } else {
        proposed_activity_rows
    };
    enforce_scroll_region_floor(state, rows, &mut agent_rows, &mut activity_rows);

    if state.fixed_panel.height_dirty() {
        let old_height = guard.panel_height();
        let new_height = state.fixed_panel.total_height();
        if old_height > new_height {
            let clear_top = rows.saturating_sub(old_height);
            let clear_bottom = rows.saturating_sub(new_height);
            let writer = guard.terminal_mut();
            for row in clear_top..clear_bottom {
                let row_1b = row.saturating_add(1);
                write!(writer, "\x1b[{row_1b};1H\x1b[2K")?;
            }
        } else if new_height > old_height {
            // Grow path: scroll the OLD scroll region up by `claim`
            // rows so the rows that are about to be reclassified as
            // panel land in native scrollback instead of being painted
            // over. Position the cursor at the OLD scroll-region
            // bottom — the bottom-margin newline scrolls the region
            // and leaves the cursor parked at the bottom row.
            let claim = new_height - old_height;
            let old_scroll_bottom_1b = rows.saturating_sub(old_height);
            if old_scroll_bottom_1b > 0 {
                let writer = guard.terminal_mut();
                write!(writer, "\x1b[{old_scroll_bottom_1b};1H")?;
                for _ in 0..claim {
                    writer.write_all(b"\n")?;
                }
                guard.note_panel_grew(claim);
            }
        }
        guard.reissue_scroll_region(new_height)?;
    }
    let caps = state.terminal_caps.clone();
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let agent_top = state.fixed_panel.agent_rows_top(rows);
    let activity_top = state.fixed_panel.activity_rows_top(rows);
    state
        .fixed_panel
        .render(guard.terminal_mut(), &caps, rows, cols)?;
    if agent_rows > 0 {
        state.agent_panel.render_view(
            &view,
            &entries,
            agent_top,
            guard.terminal_mut(),
            &caps,
            now_utc,
        )?;
    }
    if activity_rows > 0 {
        render_activity_view(
            &activity_snap,
            activity_top,
            guard.terminal_mut(),
            &caps,
            now,
            now_utc,
            cols,
        )?;
    }
    guard.terminal_mut().flush()?;
    Ok(())
}

fn enforce_scroll_region_floor(
    state: &mut AppState,
    terminal_rows: u16,
    agent_rows: &mut u16,
    activity_rows: &mut u16,
) {
    if has_scroll_floor(state, terminal_rows) {
        return;
    }
    if *activity_rows > 0 {
        state.fixed_panel.set_activity_lines(0);
        *activity_rows = 0;
        if has_scroll_floor(state, terminal_rows) {
            return;
        }
    }
    if state.fixed_panel.autocomplete_popup_rows() > 0 {
        state.fixed_panel.set_autocomplete_popup(0);
        state.autocomplete = None;
        if has_scroll_floor(state, terminal_rows) {
            return;
        }
    }
    if *agent_rows > 0 {
        state.fixed_panel.set_agent_lines(0);
        *agent_rows = 0;
        if has_scroll_floor(state, terminal_rows) {
            return;
        }
    }
    if !matches!(state.streaming_indicator, StreamingIndicator::Idle) {
        state
            .fixed_panel
            .set_streaming_indicator(StreamingIndicator::Idle);
    }
}

fn has_scroll_floor(state: &AppState, terminal_rows: u16) -> bool {
    terminal_rows.saturating_sub(state.fixed_panel.total_height()) >= MIN_SCROLL_REGION_ROWS
}

/// Redraw the panel, then the popup, then the input — the canonical
/// post-mutation paint sequence used after every input action and
/// popup lifecycle change.
pub fn redraw_all(state: &mut AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    redraw_panel(state, guard)?;
    render_popup(state, guard)?;
    render_input(state, guard)?;
    Ok(())
}

/// Drive a single mid-turn streaming-tick paint.
///
/// Encapsulates the full atomic frame: state tick + indicator sync,
/// dim-erase (if the renderer holds dim text on the current line),
/// scroll-cursor save, panel/popup/input redraws, scroll-cursor
/// restore, dim repaint (within remaining scroll-region space), and
/// flush. The save→redraw→restore→repaint sequence is wrapped in
/// [`sync_with_guard`] so capable terminals see the entire frame land
/// atomically via DCS 2026, and baseline terminals still get the
/// cursor hide/show fallback that the inline `\x1b[?25l/h` pair used
/// to provide.
///
/// `now` is injected to keep the function free of implicit clock
/// access — the caller passes `Instant::now()` from the tokio tick
/// arm so tests can drive `state.tick` deterministically.
pub fn redraw_streaming_tick(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: Option<&MarkdownRenderer>,
    now: Instant,
) -> Result<(), TuiError> {
    state.tick(now);
    state.sync_indicator_into_panel();
    let dim_was_active = renderer.is_some_and(MarkdownRenderer::is_dim_active);
    // Always erase dim before the save/restore cycle.
    // restore_scroll_cursor_clamped may emit \r\n when the panel grew,
    // which scrolls the current line into permanent scrollback — any
    // dim text on it becomes a ghost. Erasing first prevents this.
    // Repaint after restore so the dim preview returns within the same
    // flush (no visible gap at 120fps).
    if dim_was_active {
        erase_dim_lines(state.dim_wrapped_lines, guard)?;
    }
    let caps = state.terminal_caps.clone();
    sync_with_guard(&caps, guard, |guard| {
        guard.save_scroll_cursor()?;
        redraw_panel(state, guard)?;
        render_popup(state, guard)?;
        render_input(state, guard)?;
        guard.restore_scroll_cursor_clamped()?;
        if dim_was_active {
            repaint_dim(state, guard, renderer)?;
        }
        Ok(())
    })?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Repaint the renderer's current dim preview into the scroll region
/// after a panel redraw — used by [`redraw_streaming_tick`] when dim
/// was active before the redraw. Three cases all converge on resetting
/// `state.dim_wrapped_lines`: no renderer / empty preview / insufficient
/// remaining scroll-region rows. Only the happy path writes the dim
/// payload and stores the new line count.
fn repaint_dim(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: Option<&MarkdownRenderer>,
) -> Result<(), TuiError> {
    let Some(r) = renderer else {
        state.dim_wrapped_lines = 0;
        return Ok(());
    };
    let dim = r.current_dim_preview();
    if dim.is_empty() {
        state.dim_wrapped_lines = 0;
        return Ok(());
    }
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let lines = dim_line_count(&dim, cols);
    let avail = guard.scroll_rows_below_cursor();
    if lines > avail {
        state.dim_wrapped_lines = 0;
        return Ok(());
    }
    let writer = guard.terminal_mut();
    write!(writer, "\x1b[2m")?;
    write_to_scroll(&dim, writer)?;
    write!(writer, "\x1b[22m")?;
    state.dim_wrapped_lines = lines;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::autocomplete::{AutocompletePopup, SlashCandidate, SourceTag};
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::{StatusBar, StreamingIndicator};
    use crate::terminal::caps::TerminalCaps;

    fn fresh_state() -> Result<AppState, Box<dyn std::error::Error>> {
        let registry: Arc<RwLock<AgentRegistry>> = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root-render".to_string(),
            "lead".to_string(),
            "claude".to_string(),
            None,
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 5,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            None,
        )?;
        let root_id = guard.id();
        guard.confirm()?;
        Ok(AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            root_id,
            StatusBar::default(),
        ))
    }

    fn type_text(editor: &mut InputEditor, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                editor.insert_newline();
            } else {
                editor.insert_char(ch);
            }
        }
    }

    fn seed_popup(state: &mut AppState, rows: u16) {
        let candidates = vec![SlashCandidate {
            name: "help".to_owned(),
            source_tag: SourceTag::Builtin,
            description: "Show help".to_owned(),
        }];
        state.autocomplete = Some(AutocompletePopup::new_slash(candidates, "", 0));
        state.fixed_panel.set_autocomplete_popup(rows);
    }

    #[test]
    fn row_budget_drops_optional_surfaces_before_scroll_floor()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        let mut agent_rows = 3;
        let mut activity_rows = 2;
        state.fixed_panel.set_agent_lines(agent_rows);
        state.fixed_panel.set_activity_lines(activity_rows);
        seed_popup(&mut state, 5);

        enforce_scroll_region_floor(&mut state, 12, &mut agent_rows, &mut activity_rows);

        assert_eq!(activity_rows, 0);
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        assert!(state.autocomplete.is_none());
        assert!(12u16.saturating_sub(state.fixed_panel.total_height()) >= MIN_SCROLL_REGION_ROWS);
        Ok(())
    }

    #[test]
    fn row_budget_hides_streaming_indicator_when_terminal_is_tiny()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        state.streaming_indicator = StreamingIndicator::Generating {
            elapsed: std::time::Duration::from_secs(1),
            est_output_tokens: 0,
            in_flight: None,
        };
        state.sync_indicator_into_panel();
        let mut agent_rows = 0;
        let mut activity_rows = 0;

        enforce_scroll_region_floor(&mut state, 6, &mut agent_rows, &mut activity_rows);

        assert_eq!(state.fixed_panel.total_height(), 3);
        Ok(())
    }

    #[test]
    fn render_paints_visual_rows_and_cursor() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_text(&mut state.input_editor, "abcdefghij");
        let input_rows = sync_input_area(&mut state.input_editor, 5, 24);
        state.fixed_panel.set_input_area(input_rows);
        let mut buf = Vec::new();
        render_input_frame(&state, 24, 5, &mut buf)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("\x1b[22;1H\x1b[2Kabcde"));
        assert!(out.contains("\x1b[23;1H\x1b[2Kfghij"));
        assert!(out.contains("\x1b[23;5H"));
        Ok(())
    }

    #[test]
    fn render_paints_multiline_wrap_rows() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_text(&mut state.input_editor, "short\nverylonglineHERE");
        let input_rows = sync_input_area(&mut state.input_editor, 10, 24);
        state.fixed_panel.set_input_area(input_rows);
        let mut buf = Vec::new();
        render_input_frame(&state, 24, 10, &mut buf)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("\x1b[21;1H\x1b[2Kshort"));
        assert!(out.contains("\x1b[22;1H\x1b[2Kverylongli"));
        assert!(out.contains("\x1b[23;1H\x1b[2KneHERE"));
        Ok(())
    }

    #[test]
    fn render_input_replaces_control_characters() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = fresh_state()?;
        type_text(&mut state.input_editor, "a\x1b[31mb\tc");
        let input_rows = sync_input_area(&mut state.input_editor, 20, 24);
        state.fixed_panel.set_input_area(input_rows);
        let mut buf = Vec::new();
        render_input_frame(&state, 24, 20, &mut buf)?;
        let out = String::from_utf8(buf)?;
        assert!(out.contains("a?[31mb?c"), "got: {out:?}");
        assert!(
            !out.contains("\x1b[31m"),
            "raw SGR from input must not reach terminal output: {out:?}",
        );
        Ok(())
    }
}
