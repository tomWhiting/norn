//! Activity log — a backing stream of recent tool-call initiations.
//!
//! The main fixed panel now folds live work into per-agent status rows,
//! but this ring remains available as a compact backing/debug view. Each
//! entry represents a `ProviderEvent::ToolCallComplete` seen by the
//! dispatch layer; the log shows the *initiation* — the per-tool result
//! renderer in the scroll region shows the *completion*.
//!
//! ## Data source
//!
//! Entries arrive from
//! [`crate::app::dispatch::handle_agent_event`] which routes
//! `AgentEvent` values from the shared broadcast channel. Every
//! agent — root, fork, spawn — writes tagged events, so the
//! `agent_role` field reflects the actual emitting agent.
//!
//! ## Show / hide
//!
//! - Hidden (0 rows) when [`ActivityLog::is_empty`] reports no
//!   entries younger than [`IDLE_FADE`].
//! - Entries age out one row at a time. A render pass evicts every
//!   entry where `at + IDLE_FADE < now` before computing the visible
//!   set, so the panel shrinks gradually rather than flash-clearing
//!   when the last entry expires.
//! - Capacity equals the visible window — extra scrollback is not on
//!   the roadmap. When the design grows a scrollable activity surface
//!   the cap is lifted there, not here.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::Intensity;

use unicode_width::UnicodeWidthStr as _;

use crate::render::text::truncate_with_ellipsis;
use crate::terminal::caps::TerminalCaps;

/// Maximum number of rows the activity log will paint.
///
/// Matches the agent panel's `MAX_VISIBLE` (5) so the two panels share
/// a height budget the user can predict.
pub const MAX_VISIBLE: usize = 5;

/// How long an entry stays visible from the instant it was pushed.
///
/// 10s is conservative — long enough that a quick reader can catch the
/// tool name + description, short enough that a stale tool call from a
/// finished turn fades before the next turn writes over it. The value
/// is shared across root and child entries; the design left the choice
/// to Tom to tune live.
pub const IDLE_FADE: Duration = Duration::from_secs(10);

/// Dim attribute applied to the activity row body. Activity entries
/// sit below the agent panel and above the streaming indicator; they
/// are background information, not focus.
const DIM_ON: Csi = Csi::Sgr(Sgr::Intensity(Intensity::Dim));

/// SGR reset closes every row so the cursor doesn't trail attributes
/// into the streaming indicator below.
const SGR_RESET: Csi = Csi::Sgr(Sgr::Reset);

/// One row of the activity log.
#[derive(Clone, Debug)]
pub struct ActivityLogEntry {
    /// Short label identifying the emitting agent (e.g. `"root"`,
    /// `"fork/gpt-5.5"`, `"spawn/haiku"`).
    pub agent_role: String,
    /// Tool whose call started this entry.
    pub tool_name: String,
    /// Model-supplied intent description from the
    /// `tool_use_description` envelope field. `None` when the model
    /// omitted the envelope key or populated it with an empty string.
    pub description: Option<String>,
    /// Wall-clock instant the entry was pushed. Drives both the age
    /// column and the per-entry fade-out.
    pub at: Instant,
}

/// Rolling ring of recent tool-call entries.
#[derive(Debug, Default)]
pub struct ActivityLog {
    entries: VecDeque<ActivityLogEntry>,
}

impl ActivityLog {
    /// Construct an empty log with [`MAX_VISIBLE`] capacity reserved.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_VISIBLE),
        }
    }

    /// Append `entry`, dropping the oldest entry when the ring is full.
    pub fn push(&mut self, entry: ActivityLogEntry) {
        if self.entries.len() == MAX_VISIBLE {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Drop every entry whose `at + IDLE_FADE < now`.
    ///
    /// Called by [`Self::snapshot`] so the visible window in any frame
    /// only contains live entries. Per-entry expiry means the panel
    /// shrinks one row at a time as the oldest entries fade — the
    /// alternative blanket "clear all when oldest expires" would
    /// flash-clear the entire panel which jars.
    pub fn reclaim_expired(&mut self, now: Instant) {
        while let Some(front) = self.entries.front() {
            if front.at + IDLE_FADE <= now {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Whether the log currently has zero visible entries at `now`.
    ///
    /// Does not mutate state — call [`Self::reclaim_expired`] first if
    /// you want stale entries to count as absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of currently buffered entries (visible plus any not-yet
    /// reclaimed). Capped at [`MAX_VISIBLE`] by [`Self::push`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Read-only access to the entries in oldest-first order.
    #[must_use]
    pub fn entries(&self) -> &VecDeque<ActivityLogEntry> {
        &self.entries
    }

    /// Reclaim expired entries and return the snapshot the caller
    /// should size + render against.
    ///
    /// Both `height_from_log` and `render_view` MUST be called with the
    /// snapshot returned by the same `snapshot()` call — the same
    /// invariant the agent status panel enforces. A second internal
    /// call at an expiry boundary would silently reclaim more entries
    /// and the height fed into the fixed panel would no longer match
    /// the paint.
    pub fn snapshot(&mut self, now: Instant) -> Vec<ActivityLogEntry> {
        self.reclaim_expired(now);
        self.entries.iter().take(MAX_VISIBLE).cloned().collect()
    }
}

/// Rows an activity-log snapshot occupies in the fixed panel.
///
/// Visible entries plus zero — the design has no overflow summary row
/// for the activity log (the ring is tail-of-stream by definition and
/// capacity equals the visible window).
#[must_use]
pub fn height_from_log(snapshot: &[ActivityLogEntry]) -> u16 {
    u16::try_from(snapshot.len()).unwrap_or(u16::MAX)
}

/// Format `n` seconds since the entry was pushed.
fn format_age(since: Duration) -> String {
    let secs = since.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Compose one activity row's plain-text body, fitting within
/// `terminal_cols` display columns.
///
/// Shape: `[{agent}] {tool}: '{desc}'  {age}`. When the description
/// is missing or empty: `[{agent}] {tool}  {age}`. When the
/// description would push past `terminal_cols` it is truncated with a
/// single-codepoint Unicode ellipsis so the trailing age stays
/// visible.
fn format_activity_line(entry: &ActivityLogEntry, now: Instant, terminal_cols: u16) -> String {
    let age = format_age(now.saturating_duration_since(entry.at));
    let head = format!("[{}] {}", entry.agent_role, entry.tool_name);
    let age_segment = format!("  {age}");
    let description = entry
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let Some(description) = description else {
        return format!("{head}{age_segment}");
    };
    // Budget for the description segment = total - head - ": '...'"
    // wrap (4 cols) - age segment.
    let head_cols = u16::try_from(head.width()).unwrap_or(u16::MAX);
    let age_cols = u16::try_from(age_segment.width()).unwrap_or(u16::MAX);
    let wrap_cols: u16 = 4;
    let budget = terminal_cols
        .saturating_sub(head_cols)
        .saturating_sub(age_cols)
        .saturating_sub(wrap_cols);
    let trimmed = truncate_with_ellipsis(description, budget);
    if trimmed.is_empty() {
        return format!("{head}{age_segment}");
    }
    format!("{head}: '{trimmed}'{age_segment}")
}

/// Position the cursor at zero-based `row` column 0 and clear the line.
fn clear_row<W: io::Write>(row: u16, writer: &mut W) -> io::Result<()> {
    let cursor = Csi::Cursor(Cursor::Position {
        line: OneBased::from_zero_based(row),
        col: OneBased::from_zero_based(0),
    });
    let erase = Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine));
    write!(writer, "{cursor}{erase}")
}

/// Render `snapshot` cursor-addressed to `top_row..top_row + len`.
///
/// `now_utc` is unused by the line layout today (the age column uses
/// the wall-clock `Instant` from `now`) but is taken alongside `now`
/// for parity with the agent status panel — both panels are addressed
/// from the same redraw pass and threading the same pair of clocks
/// keeps the call sites symmetric.
pub fn render_view<W: io::Write>(
    snapshot: &[ActivityLogEntry],
    top_row: u16,
    writer: &mut W,
    _caps: &TerminalCaps,
    now: Instant,
    _now_utc: DateTime<Utc>,
    terminal_cols: u16,
) -> io::Result<()> {
    let mut row = top_row;
    for entry in snapshot {
        clear_row(row, writer)?;
        let line = format_activity_line(entry, now, terminal_cols);
        write!(writer, "{DIM_ON}{line}{SGR_RESET}")?;
        row = row.saturating_add(1);
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::similar_names,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::*;

    fn entry(role: &str, tool: &str, desc: Option<&str>, at: Instant) -> ActivityLogEntry {
        ActivityLogEntry {
            agent_role: role.to_string(),
            tool_name: tool.to_string(),
            description: desc.map(str::to_owned),
            at,
        }
    }

    #[test]
    fn new_log_is_empty() {
        let log = ActivityLog::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn push_appends_in_order() {
        let now = Instant::now();
        let mut log = ActivityLog::new();
        log.push(entry("root", "bash", Some("ls"), now));
        log.push(entry("root", "read", Some("DESIGN.md"), now));
        assert_eq!(log.len(), 2);
        let entries: Vec<_> = log.entries().iter().collect();
        assert_eq!(entries[0].tool_name, "bash");
        assert_eq!(entries[1].tool_name, "read");
    }

    #[test]
    fn push_rolls_oldest_when_capacity_full() {
        let now = Instant::now();
        let mut log = ActivityLog::new();
        for i in 0..(MAX_VISIBLE + 2) {
            log.push(entry("root", &format!("tool_{i}"), None, now));
        }
        assert_eq!(log.len(), MAX_VISIBLE);
        // Oldest two entries dropped — the surviving front should be
        // tool_2 (indices 2..MAX_VISIBLE+1).
        let front = log.entries().front().unwrap();
        assert_eq!(front.tool_name, "tool_2");
    }

    #[test]
    fn reclaim_expired_drops_only_aged_entries_in_order() {
        let t0 = Instant::now();
        let mut log = ActivityLog::new();
        log.push(entry("root", "old", None, t0));
        log.push(entry("root", "middle", None, t0 + Duration::from_secs(3)));
        log.push(entry("root", "fresh", None, t0 + Duration::from_secs(9)));

        // Now is t0 + 11s — IDLE_FADE = 10s, so the t0 entry expired
        // but the t0+3 entry is still live (3 + 10 = 13 > 11).
        log.reclaim_expired(t0 + Duration::from_secs(11));
        let names: Vec<_> = log.entries().iter().map(|e| e.tool_name.as_str()).collect();
        assert_eq!(names, vec!["middle", "fresh"]);
    }

    #[test]
    fn reclaim_expired_stops_at_first_live_entry() {
        let t0 = Instant::now();
        let mut log = ActivityLog::new();
        // Out-of-order timestamps would never happen in practice
        // (push is monotonic) but exercising the loop's stop condition
        // protects against accidental refactors.
        log.push(entry("root", "old", None, t0));
        log.push(entry("root", "fresher", None, t0 + Duration::from_secs(20)));
        log.reclaim_expired(t0 + Duration::from_secs(11));
        let names: Vec<_> = log.entries().iter().map(|e| e.tool_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["fresher"],
            "must not reclaim past first live entry"
        );
    }

    #[test]
    fn snapshot_returns_visible_window_after_reclaim() {
        let t0 = Instant::now();
        let mut log = ActivityLog::new();
        log.push(entry("root", "expired", None, t0));
        log.push(entry("root", "live", None, t0 + Duration::from_secs(5)));
        let snap = log.snapshot(t0 + Duration::from_secs(11));
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].tool_name, "live");
        // Reclaim is persistent — the original entry list is shorter.
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn height_from_log_counts_snapshot_rows() {
        let now = Instant::now();
        let mut log = ActivityLog::new();
        for i in 0..3 {
            log.push(entry("root", &format!("t{i}"), None, now));
        }
        let snap = log.snapshot(now);
        assert_eq!(height_from_log(&snap), 3);
    }

    #[test]
    fn format_age_buckets_correctly() {
        assert_eq!(format_age(Duration::from_secs(0)), "0s");
        assert_eq!(format_age(Duration::from_secs(59)), "59s");
        assert_eq!(format_age(Duration::from_mins(1)), "1m");
        assert_eq!(format_age(Duration::from_hours(1)), "1h");
    }

    #[test]
    fn format_activity_line_renders_with_description() {
        let now = Instant::now();
        let e = entry("root", "bash", Some("listing docs"), now);
        let line = format_activity_line(&e, now + Duration::from_secs(3), 80);
        assert!(line.starts_with("[root] bash: 'listing docs'"));
        assert!(line.ends_with("3s"), "age tail: {line:?}");
    }

    #[test]
    fn format_activity_line_omits_quotes_when_description_none() {
        let now = Instant::now();
        let e = entry("root", "read", None, now);
        let line = format_activity_line(&e, now + Duration::from_secs(1), 80);
        assert!(line.starts_with("[root] read"));
        assert!(
            !line.contains(": '"),
            "no colon-quote when desc is None: {line:?}"
        );
        assert!(!line.contains("''"), "no empty quotes: {line:?}");
    }

    #[test]
    fn format_activity_line_omits_quotes_when_description_empty() {
        // split_envelope_fields yields Some("") when the model
        // populated the envelope key with a blank string. The line
        // builder must treat that as missing — no `: ''` on screen.
        let now = Instant::now();
        let e = entry("root", "read", Some(""), now);
        let line = format_activity_line(&e, now + Duration::from_secs(1), 80);
        assert!(
            !line.contains("''"),
            "empty desc must not render empty quotes: {line:?}"
        );
    }

    #[test]
    fn format_activity_line_truncates_long_description_with_ellipsis() {
        let now = Instant::now();
        let long = "this description is comfortably longer than a 40-column row";
        let e = entry("root", "bash", Some(long), now);
        let line = format_activity_line(&e, now + Duration::from_secs(2), 40);
        assert!(line.contains('\u{2026}'), "ellipsis must appear: {line:?}");
        assert!(
            line.contains("2s"),
            "age tail must survive truncation: {line:?}"
        );
    }

    #[test]
    fn render_view_writes_one_row_per_snapshot_entry_with_dim_attribute() {
        let now = Instant::now();
        let mut log = ActivityLog::new();
        log.push(entry("root", "bash", Some("ls"), now));
        log.push(entry("root", "read", Some("DESIGN.md"), now));
        let snap = log.snapshot(now);
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        render_view(&snap, 10, &mut buf, &caps, now, Utc::now(), 80).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // Both rows addressed by cursor positioning.
        assert!(
            out.contains("\x1b[11;1H"),
            "first row 10 -> 11 one-based: {out:?}"
        );
        assert!(
            out.contains("\x1b[12;1H"),
            "second row 11 -> 12 one-based: {out:?}"
        );
        // Dim SGR wraps each line.
        assert!(out.contains("\x1b[2m"), "dim SGR must appear: {out:?}");
        assert!(out.contains("bash"));
        assert!(out.contains("read"));
    }

    #[test]
    fn render_view_writes_nothing_for_empty_snapshot() {
        let snap: Vec<ActivityLogEntry> = Vec::new();
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        render_view(&snap, 10, &mut buf, &caps, Instant::now(), Utc::now(), 80).unwrap();
        assert!(
            buf.is_empty(),
            "empty snapshot must produce zero paint, got {} bytes",
            buf.len()
        );
    }
}
