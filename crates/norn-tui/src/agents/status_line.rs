//! Agent status panel — one row per active child agent.
//!
//! The panel reads [`AgentRegistry`] under a `parking_lot::RwLock` on
//! every redraw cycle, applies the tree-collapse heuristic from
//! [`super::tree`], and writes status lines to the fixed-panel rows that
//! NT-002 reserves above the input area.
//!
//! NT-009 owns the read-and-render path. NT-011 will drive
//! [`AgentStatusPanel::set_activity`] and [`AgentStatusPanel::set_tokens`]
//! from the event loop; this brief defines those setters and uses the
//! cached values on the render path so the public surface is stable.
//!
//! ## Visibility rules
//!
//! - The single-agent (root-only) case shows zero rows — R1 / C56.
//! - When a child reaches [`AgentStatus::Completed`] or
//!   [`AgentStatus::Failed`], its line is held for
//!   [`HOLD_DURATION`] showing the terminal icon/state, then reclaimed.
//! - The collapse heuristic in [`super::tree::collapse`] limits visible
//!   rows to five; the panel emits a `⋯ N more active agents` overflow
//!   row when there are unseen entries.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::{Intensity, RgbColor};
use uuid::Uuid;

use norn::agent::registry::{AgentEntry, AgentRegistry, AgentStatus};

use crate::render::style::colour_for;
use crate::terminal::caps::TerminalCaps;

use super::tree::{self, CandidateEntry, CollapsedView};

/// Duration a completed or failed agent's status line stays visible
/// before the row is reclaimed.
pub const HOLD_DURATION: Duration = Duration::from_secs(3);

/// Foreground colour for the running / completed icon.
const GREEN_RUNNING: RgbColor = RgbColor::new(95, 215, 95);
/// Foreground colour for the failed icon.
const RED_FAILED: RgbColor = RgbColor::new(215, 95, 95);
/// Foreground colour for the spawning icon. Matches the streaming
/// indicator's amber on purpose; the constant stays local so future
/// colour-policy changes here do not ripple through the rest of the
/// fixed panel.
const YELLOW_SPAWNING: RgbColor = RgbColor::new(215, 175, 0);

/// Number of fixed-panel rows a [`CollapsedView`] occupies.
///
/// Counts the visible rows plus an overflow summary row when applicable.
/// Lifted out of [`AgentStatusPanel::height`] so the event loop can size
/// the fixed panel and render the status rows from the *same*
/// [`AgentStatusPanel::snapshot`] result — see the docstring on
/// [`AgentStatusPanel::height`] for the snapshot-idempotency caveat.
#[must_use]
pub fn height_from_view(view: &CollapsedView) -> u16 {
    let visible = u16::try_from(view.visible.len()).unwrap_or(u16::MAX);
    let overflow_row = u16::from(view.overflow_count > 0);
    visible.saturating_add(overflow_row)
}

/// Activity state a panel client (NT-011) attaches to an agent.
///
/// The registry only knows lifecycle status; activity comes from the
/// event loop. The panel cache is keyed by agent id.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AgentActivity {
    /// Waiting for a child or tool result.
    #[default]
    Idle,
    /// Currently running — typically the tool name in flight.
    Running(String),
    /// Terminal result summary, shown during the hold window.
    Result(String),
}

/// Lifecycle-status icon for an agent row.
///
/// The idle swap (`●` → `◌`) happens at the panel render layer when
/// [`AgentActivity::Idle`] is the live activity for an [`AgentStatus::Active`]
/// entry. This function only reports the lifecycle-derived icon.
#[must_use]
pub fn icon_for(status: AgentStatus) -> char {
    match status {
        AgentStatus::Spawning => '⊙',
        AgentStatus::Active | AgentStatus::Completing => '●',
        AgentStatus::Completed => '✓',
        AgentStatus::Failed => '✗',
    }
}

/// Cursor-position escape targeting the start of zero-based `row`.
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

/// Last path segment as the agent's display name.
///
/// Mirrors the design's `path.rsplit('/').next().unwrap_or(path)`
/// idiom. `rsplit` always yields at least one element, so the fallback
/// is unreachable for any non-empty path; it preserves the original
/// string defensively.
fn agent_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Format `n` tokens in the compact form used in status lines.
///
/// - `< 1_000` → `"{n}"`.
/// - `1_000..=999_999` → integer-divided `"{k}k"`.
/// - `1_000_000..=9_999_999` → one-decimal `"{n}.{d}M"`.
/// - `>= 10_000_000` → integer-divided `"{m}M"`.
fn format_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else if n < 10_000_000 {
        let whole = n / 1_000_000;
        let tenth = (n % 1_000_000) / 100_000;
        format!("{whole}.{tenth}M")
    } else {
        format!("{}M", n / 1_000_000)
    }
}

/// Format elapsed wall-clock between `spawned_at` and `now` as `"{n}s"`,
/// `"{n}m"`, or `"{n}h"` — whichever bucket the duration falls in.
fn format_elapsed(spawned_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let raw = (now - spawned_at).num_seconds().max(0);
    let secs = u64::try_from(raw).unwrap_or(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Activity column text — `"idle"` when no metadata, the running label
/// when in flight, the result summary when held after completion.
fn activity_text(activity: Option<&AgentActivity>) -> String {
    match activity {
        None | Some(AgentActivity::Idle) => "idle".to_string(),
        Some(AgentActivity::Running(s) | AgentActivity::Result(s)) => s.clone(),
    }
}

/// Format one status line as plain text (no SGR escapes).
///
/// The format is `{indent}{icon} {name}  {activity}  {tokens}  {elapsed}`
/// — note the double space between every field after the icon. Colour
/// and dim attributes are applied around this string by
/// [`AgentStatusPanel::render`]; tests inspecting the textual form can
/// rely on byte-for-byte prefixes.
///
/// `depth` is the row's display depth from
/// [`tree::order_for_display`] — genealogical (`parent_id`-derived)
/// nesting under the nearest visible ancestor, two spaces per level.
/// It is deliberately not derived from the registry path: auto-generated
/// paths interleave literal `spawn`/`fork` namespace segments
/// (`/root/spawn/{id}/spawn/{id}`), so segment counting over-indents,
/// and explicit `path` arguments need not nest under the spawner at all.
fn format_status_line(
    entry: &AgentEntry,
    activity: Option<&AgentActivity>,
    input_tokens: u64,
    output_tokens: u64,
    now: DateTime<Utc>,
    depth: usize,
) -> String {
    let indent = " ".repeat(2 * depth);
    let icon = icon_for(entry.status);
    let name = agent_name(&entry.path);
    let activity_str = activity_text(activity);
    let tokens = format_tokens(input_tokens.saturating_add(output_tokens));
    let elapsed = format_elapsed(entry.spawned_at, now);
    format!("{indent}{icon} {name}  {activity_str}  {tokens}  {elapsed}")
}

/// SGR foreground colour for an agent row given its current status and
/// live activity. Returns `None` when the row should render with the
/// dim attribute (idle-active) instead of a colour.
fn row_colour(
    status: AgentStatus,
    activity: Option<&AgentActivity>,
    caps: &TerminalCaps,
) -> Option<String> {
    match status {
        AgentStatus::Spawning => Some(colour_for(YELLOW_SPAWNING, caps)),
        AgentStatus::Active | AgentStatus::Completing => {
            if matches!(activity, Some(AgentActivity::Idle)) {
                None
            } else {
                Some(colour_for(GREEN_RUNNING, caps))
            }
        }
        AgentStatus::Completed => Some(colour_for(GREEN_RUNNING, caps)),
        AgentStatus::Failed => Some(colour_for(RED_FAILED, caps)),
    }
}

/// Status panel — owns the live cache that bridges
/// [`AgentRegistry`] (lifecycle status) and event-loop-supplied
/// metadata (activity / token counts).
pub struct AgentStatusPanel {
    registry: Arc<RwLock<AgentRegistry>>,
    holds: HashMap<Uuid, Instant>,
    last_status: HashMap<Uuid, AgentStatus>,
    last_change_at: HashMap<Uuid, Instant>,
    activity: HashMap<Uuid, AgentActivity>,
    tokens: HashMap<Uuid, (u64, u64)>,
}

impl AgentStatusPanel {
    /// Wrap the shared registry and start with empty caches.
    pub fn new(registry: Arc<RwLock<AgentRegistry>>) -> Self {
        Self {
            registry,
            holds: HashMap::new(),
            last_status: HashMap::new(),
            last_change_at: HashMap::new(),
            activity: HashMap::new(),
            tokens: HashMap::new(),
        }
    }

    /// Record live activity for an agent. NT-011 calls this from its
    /// event loop; the value is read on the next [`Self::render`] pass.
    pub fn set_activity(&mut self, id: Uuid, activity: AgentActivity) {
        self.activity.insert(id, activity);
    }

    /// Accumulate token counts for an agent.
    ///
    /// The per-turn usage figures the event loop hands in are added to
    /// the existing running totals via [`u64::saturating_add`]. Calling
    /// `set_tokens(id, 3_000, 2_000)` then `set_tokens(id, 4_000, 1_000)`
    /// leaves the cached pair at `(7_000, 3_000)` — the agent panel
    /// shows session-cumulative usage, not the last turn alone.
    pub fn set_tokens(&mut self, id: Uuid, input: u64, output: u64) {
        let entry = self.tokens.entry(id).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(input);
        entry.1 = entry.1.saturating_add(output);
    }

    /// Drop the cumulative token tally for an agent.
    ///
    /// Used by `/clear` to reset the panel's session-cumulative figures
    /// alongside the event-store swap, so the next turn's usage figures
    /// start from zero rather than inheriting a stale baseline (which
    /// `set_tokens`' `saturating_add` would otherwise accumulate over).
    pub fn reset_tokens(&mut self, id: Uuid) {
        self.tokens.remove(&id);
    }

    /// Capture the registry, detect transitions, apply hold reclaim,
    /// and run the collapse heuristic.
    ///
    /// Idempotent for a fixed `now` — repeated calls do not move the
    /// view as long as the registry and `now` stay constant. NT-011
    /// can call this between [`Self::height`] and [`Self::render`]
    /// without seeing flicker.
    pub fn snapshot(&mut self, now: Instant) -> (CollapsedView, Vec<AgentEntry>) {
        let entries = self.registry.read().list();

        self.absorb_transitions(&entries, now);
        let candidates = self.build_candidates(&entries, now);
        let view = tree::collapse(&candidates, now);
        self.reclaim_expired_holds(now);
        (view, entries)
    }

    /// Number of fixed-panel rows the agent status section needs at
    /// `now`. Includes the overflow summary row when applicable.
    ///
    /// This is a convenience wrapper that internally calls
    /// [`Self::snapshot`]. Event-loop callers that need both the height
    /// and the rendered output must instead call [`Self::snapshot`] once
    /// and feed the result into [`height_from_view`] and
    /// [`Self::render_view`] — [`Self::snapshot`] is NOT idempotent at
    /// hold-expiry boundaries (it mutates the hold map), so two
    /// back-to-back calls with the same `now` can disagree on what is
    /// visible.
    pub fn height(&mut self, now: Instant) -> u16 {
        let (view, _entries) = self.snapshot(now);
        height_from_view(&view)
    }

    /// Redraw the status panel starting at zero-based `top_row`.
    ///
    /// Convenience wrapper that snapshots and renders in one call.
    /// Event-loop callers that have already snapshotted (to size the
    /// fixed panel) must instead use [`Self::render_view`] with the
    /// stored snapshot.
    pub fn render<W: io::Write>(
        &mut self,
        top_row: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        now: Instant,
        now_utc: DateTime<Utc>,
    ) -> io::Result<()> {
        let (view, entries) = self.snapshot(now);
        self.render_view(&view, &entries, top_row, writer, caps, now_utc)
    }

    /// Redraw using an already-computed snapshot.
    ///
    /// The caller is responsible for ensuring `view` and `entries` come
    /// from the same [`Self::snapshot`] call as the height-driven panel
    /// sizing — feeding mismatched snapshots produces inconsistent
    /// paint. The point of taking the snapshot externally is to avoid
    /// the second internal `snapshot` call mutating the hold map and
    /// silently shrinking the rendered view relative to the height the
    /// caller already committed to.
    pub fn render_view<W: io::Write>(
        &self,
        view: &CollapsedView,
        entries: &[AgentEntry],
        top_row: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        now_utc: DateTime<Utc>,
    ) -> io::Result<()> {
        let entries_by_id: HashMap<Uuid, &AgentEntry> = entries.iter().map(|e| (e.id, e)).collect();

        // Genealogy for ordering/indentation: live + terminal entries
        // plus completion records, so an ancestor chain crossing a
        // reclaimed mid-tree agent still resolves (W3.4 trees nest to
        // any depth and inner nodes can finish before their children).
        let parent_of: HashMap<Uuid, Option<Uuid>> = {
            let mut map: HashMap<Uuid, Option<Uuid>> = self
                .registry
                .read()
                .tombstones()
                .into_iter()
                .map(|t| (t.id, t.parent_id))
                .collect();
            map.extend(entries.iter().map(|e| (e.id, e.parent_id)));
            map
        };
        let ordered = tree::order_for_display(view.visible.clone(), &parent_of);

        let mut row = top_row;
        for (candidate, depth) in &ordered {
            if let Some(entry) = entries_by_id.get(&candidate.id) {
                let activity = self.activity.get(&candidate.id);
                let (input, output) = self.tokens.get(&candidate.id).copied().unwrap_or((0, 0));
                let line = format_status_line(entry, activity, input, output, now_utc, *depth);
                Self::write_styled_line(row, writer, caps, entry.status, activity, &line)?;
            } else {
                // Entry vanished between snapshot and re-read — clear
                // the row so we never leave stale paint behind.
                clear_row(row, writer)?;
            }
            row = row.saturating_add(1);
        }

        if view.overflow_count > 0 {
            Self::write_overflow_row(row, writer, view.overflow_count)?;
        }
        Ok(())
    }

    fn absorb_transitions(&mut self, entries: &[AgentEntry], now: Instant) {
        for entry in entries {
            let prev = self.last_status.get(&entry.id).copied();
            if prev != Some(entry.status) {
                self.last_change_at.insert(entry.id, now);
                self.last_status.insert(entry.id, entry.status);
                if matches!(entry.status, AgentStatus::Completed | AgentStatus::Failed) {
                    self.holds.insert(entry.id, now + HOLD_DURATION);
                }
            }
        }
    }

    fn build_candidates(&self, entries: &[AgentEntry], now: Instant) -> Vec<CandidateEntry> {
        entries
            .iter()
            .filter(|e| self.is_visible_candidate(e, now))
            .map(|e| CandidateEntry {
                id: e.id,
                parent_id: e.parent_id,
                spawned_at: e.spawned_at,
                last_change_at: self.last_change_at.get(&e.id).copied().unwrap_or(now),
                status: e.status,
            })
            .collect()
    }

    fn is_visible_candidate(&self, entry: &AgentEntry, now: Instant) -> bool {
        match entry.status {
            AgentStatus::Spawning | AgentStatus::Active | AgentStatus::Completing => true,
            AgentStatus::Completed | AgentStatus::Failed => self
                .holds
                .get(&entry.id)
                .is_some_and(|deadline| now < *deadline),
        }
    }

    fn reclaim_expired_holds(&mut self, now: Instant) {
        let expired: HashSet<Uuid> = self
            .holds
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(id, _)| *id)
            .collect();
        if expired.is_empty() {
            return;
        }
        // The registry retains terminal entries precisely so this panel
        // can show them through the hold window; once the hold expires
        // the panel is the reclaimer of record for entries nothing else
        // (e.g. `close_agent`) removed first.
        let mut registry = self.registry.write();
        for id in &expired {
            self.holds.remove(id);
            self.activity.remove(id);
            self.tokens.remove(id);
            self.last_change_at.remove(id);
            self.last_status.remove(id);
            registry.remove_terminal(*id);
        }
    }

    fn write_styled_line<W: io::Write>(
        row: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        status: AgentStatus,
        activity: Option<&AgentActivity>,
        line: &str,
    ) -> io::Result<()> {
        clear_row(row, writer)?;
        let idle_active = matches!(status, AgentStatus::Active | AgentStatus::Completing)
            && matches!(activity, Some(AgentActivity::Idle));
        if idle_active {
            // Idle swap: dim attribute, no foreground colour. Replace
            // the lifecycle `●` icon with the explicit-idle `◌`.
            write!(writer, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Dim)))?;
            let swapped = line.replacen('●', "◌", 1);
            write!(writer, "{swapped}")?;
        } else {
            match row_colour(status, activity, caps) {
                Some(colour) => write!(writer, "{colour}{line}")?,
                None => write!(writer, "{line}")?,
            }
        }
        write!(writer, "{}", Csi::Sgr(Sgr::Reset))
    }

    fn write_overflow_row<W: io::Write>(
        row: u16,
        writer: &mut W,
        overflow: usize,
    ) -> io::Result<()> {
        clear_row(row, writer)?;
        write!(
            writer,
            "{}⋯ {overflow} more active agents{}",
            Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
            Csi::Sgr(Sgr::Reset),
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::missing_const_for_fn,
    clippy::similar_names,
    clippy::too_many_arguments
)]
mod tests {
    use super::*;

    use norn::agent::registry::AgentRegistry;

    fn fresh_registry() -> Arc<RwLock<AgentRegistry>> {
        AgentRegistry::shared()
    }

    fn confirm_root(registry: &Arc<RwLock<AgentRegistry>>) -> Uuid {
        let guard = AgentRegistry::reserve(
            registry,
            "/root".to_string(),
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
            },
            None,
        )
        .expect("reserve root");
        let id = guard.id();
        guard.confirm().expect("confirm root");
        id
    }

    fn confirm_child(registry: &Arc<RwLock<AgentRegistry>>, path: &str, parent: Uuid) -> Uuid {
        confirm_child_with_depth(registry, path, parent, 4)
    }

    /// Reserve + confirm a child whose stamped budget has
    /// `remaining_depth: depth`. The registry enforces narrowing
    /// (child ≤ spawner − 1), so deeper levels pass a smaller depth.
    fn confirm_child_with_depth(
        registry: &Arc<RwLock<AgentRegistry>>,
        path: &str,
        parent: Uuid,
        depth: u32,
    ) -> Uuid {
        let guard = AgentRegistry::reserve(
            registry,
            path.to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent),
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: depth,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
            },
            None,
        )
        .expect("reserve child");
        let id = guard.id();
        guard.confirm().expect("confirm child");
        id
    }

    #[test]
    fn icon_for_active_returns_filled_circle() {
        assert_eq!(icon_for(AgentStatus::Active), '●');
    }

    #[test]
    fn icon_for_each_status_matches_design() {
        assert_eq!(icon_for(AgentStatus::Spawning), '⊙');
        assert_eq!(icon_for(AgentStatus::Active), '●');
        assert_eq!(icon_for(AgentStatus::Completing), '●');
        assert_eq!(icon_for(AgentStatus::Completed), '✓');
        assert_eq!(icon_for(AgentStatus::Failed), '✗');
    }

    #[test]
    fn agent_name_returns_last_segment() {
        assert_eq!(agent_name("/root/child"), "child");
        assert_eq!(agent_name("/root"), "root");
    }

    #[test]
    fn format_tokens_buckets() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(12_000), "12k");
        assert_eq!(format_tokens(999_999), "999k");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(9_999_999), "9.9M");
        assert_eq!(format_tokens(10_000_000), "10M");
        assert_eq!(format_tokens(123_000_000), "123M");
    }

    #[test]
    fn format_elapsed_buckets() {
        let t0 = Utc::now();
        assert_eq!(format_elapsed(t0, t0), "0s");
        assert_eq!(
            format_elapsed(t0, t0 + chrono::Duration::seconds(59)),
            "59s"
        );
        assert_eq!(format_elapsed(t0, t0 + chrono::Duration::seconds(60)), "1m");
        assert_eq!(
            format_elapsed(t0, t0 + chrono::Duration::seconds(3_600)),
            "1h"
        );
    }

    #[test]
    fn height_zero_for_single_root_agent() {
        let registry = fresh_registry();
        let _root = confirm_root(&registry);
        let mut panel = AgentStatusPanel::new(registry);
        assert_eq!(panel.height(Instant::now()), 0);
    }

    #[test]
    fn height_increases_when_child_registered() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        assert_eq!(panel.height(Instant::now()), 0);

        let _child = confirm_child(&registry, "/root/child", root);
        assert!(
            panel.height(Instant::now()) > 0,
            "panel height must grow when a child is registered"
        );
    }

    #[test]
    fn status_line_for_depth_one_running_agent_has_two_space_indent_and_filled_circle() {
        let entry = AgentEntry {
            id: Uuid::new_v4(),
            path: "/root/child".to_string(),
            role: "dev".to_string(),
            status: AgentStatus::Active,
            model: "claude".to_string(),
            spawned_at: Utc::now(),
            parent_id: Some(Uuid::new_v4()),
            completed_at: None,
            policy: norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 0,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
            },
        };
        let line = format_status_line(&entry, None, 0, 0, Utc::now(), 1);
        assert!(
            line.starts_with("  ●"),
            "expected 2-space indent + ●, got: {line:?}"
        );
    }

    /// W3.4 auto-generated paths carry literal `spawn`/`fork` namespace
    /// segments (`/root/spawn/{id}`); indentation must come from the
    /// genealogical depth parameter, never from counting path segments
    /// (which would indent this depth-1 child two levels).
    #[test]
    fn status_line_indent_uses_genealogical_depth_not_path_segments() {
        let entry = AgentEntry {
            id: Uuid::new_v4(),
            path: "/root/spawn/0a1b2c3d".to_string(),
            role: "worker".to_string(),
            status: AgentStatus::Active,
            model: "claude".to_string(),
            spawned_at: Utc::now(),
            parent_id: Some(Uuid::new_v4()),
            completed_at: None,
            policy: norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 0,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
            },
        };
        let line = format_status_line(&entry, None, 0, 0, Utc::now(), 1);
        assert!(
            line.starts_with("  ●"),
            "depth-1 child of root indents one level regardless of its \
             path shape, got: {line:?}"
        );
    }

    #[test]
    fn completed_agent_visible_for_three_seconds_then_reclaimed() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child_id = confirm_child(&registry, "/root/child", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));

        let t0 = Instant::now();
        let (view, _) = panel.snapshot(t0);
        assert!(
            view.visible.iter().any(|e| e.id == child_id),
            "active child must be visible before completion"
        );

        registry.write().mark_completed(child_id).expect("complete");

        let (view, _) = panel.snapshot(t0);
        assert!(
            view.visible.iter().any(|e| e.id == child_id),
            "completed child must be visible during hold window"
        );

        let (view, _) = panel.snapshot(t0 + Duration::from_millis(3_100));
        assert!(
            !view.visible.iter().any(|e| e.id == child_id),
            "completed child must be reclaimed after hold expires"
        );
    }

    #[test]
    fn failed_agent_held_then_reclaimed() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child_id = confirm_child(&registry, "/root/failer", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));

        let t0 = Instant::now();
        let (_, _) = panel.snapshot(t0);
        registry.write().mark_failed(child_id).expect("fail");

        let (view, _) = panel.snapshot(t0);
        assert!(view.visible.iter().any(|e| e.id == child_id));

        let (view, _) = panel.snapshot(t0 + Duration::from_millis(3_100));
        assert!(!view.visible.iter().any(|e| e.id == child_id));
    }

    #[test]
    fn snapshot_idempotent_for_constant_now() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let _child = confirm_child(&registry, "/root/child", root);
        let mut panel = AgentStatusPanel::new(registry);
        let now = Instant::now();
        let (first, _) = panel.snapshot(now);
        let (second, _) = panel.snapshot(now);
        assert_eq!(first.visible.len(), second.visible.len());
    }

    #[test]
    fn render_writes_one_row_per_visible_agent_and_overflow_summary() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        for i in 0..7 {
            let _ = confirm_child(&registry, &format!("/root/child-{i}"), root);
        }
        let mut panel = AgentStatusPanel::new(registry);
        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(10, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            out.contains("more active agents"),
            "overflow row must appear, got: {out:?}"
        );
    }

    #[test]
    fn render_paints_no_rows_when_only_root_present() {
        let registry = fresh_registry();
        let _root = confirm_root(&registry);
        let mut panel = AgentStatusPanel::new(registry);
        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(10, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        assert!(
            buf.is_empty(),
            "single-agent case must produce zero paint, got {} bytes",
            buf.len()
        );
    }

    #[test]
    fn idle_activity_swaps_filled_to_dotted_circle_in_paint() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/idle-child", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        panel.set_activity(child, AgentActivity::Idle);

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(0, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            out.contains('◌'),
            "idle child must render with ◌, got: {out:?}"
        );
        assert!(
            out.contains("idle-child"),
            "child name must appear, got: {out:?}"
        );
    }

    /// A depth-2 tree (W3.4 recursion) paints in genealogical preorder
    /// with one indent level per hop, even though the auto-path shapes
    /// contain literal `spawn` namespace segments.
    #[test]
    fn render_paints_deep_tree_in_preorder_with_genealogical_indent() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/spawn/mid", root);
        let _grandchild =
            confirm_child_with_depth(&registry, "/root/spawn/mid/spawn/leaf", child, 3);

        let mut panel = AgentStatusPanel::new(registry);
        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(0, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");

        let root_pos = out.find("root  ").expect("root row");
        let mid_pos = out.find("  ● mid").expect("child row indented one level");
        let leaf_pos = out
            .find("    ● leaf")
            .expect("grandchild row indented two levels");
        assert!(
            root_pos < mid_pos && mid_pos < leaf_pos,
            "rows must paint in preorder: {out:?}"
        );
        assert!(
            !out.contains("      ● "),
            "no row may over-indent from path-segment counting: {out:?}"
        );
    }

    /// A reclaimed mid-tree parent (tombstone genealogy) still anchors
    /// its live child under the root instead of dropping or floating it.
    #[test]
    fn render_anchors_child_of_reclaimed_parent_under_root() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let mid = confirm_child(&registry, "/root/spawn/mid", root);
        let _leaf = confirm_child_with_depth(&registry, "/root/spawn/mid/spawn/leaf", mid, 3);
        {
            let mut reg = registry.write();
            reg.mark_completed(mid).expect("complete mid");
            assert!(reg.remove_terminal(mid), "reclaim mid");
        }

        let mut panel = AgentStatusPanel::new(registry);
        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(0, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(
            out.contains("  ● leaf"),
            "leaf anchors one level under the root (its visible \
             ancestor) via the tombstone's parent link: {out:?}"
        );
    }

    #[test]
    fn set_tokens_is_cumulative_across_calls() {
        // Sandra grill fix: set_tokens must saturating_add to existing
        // values, not replace. Multi-turn usage from successive Done
        // events must accumulate into a session total.
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/multi-turn", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        panel.set_tokens(child, 3_000, 2_000);
        panel.set_tokens(child, 4_000, 1_000);
        // Running activity makes the row paint a non-trivial line so
        // the assertion below has something to grep.
        panel.set_activity(child, AgentActivity::Running("bash".to_string()));

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(0, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");
        // 7_000 in + 3_000 out = 10_000 total → format_tokens → "10k"
        assert!(
            out.contains("10k"),
            "cumulative tokens (7k in + 3k out = 10k total) must surface: {out:?}",
        );
    }

    #[test]
    fn set_tokens_saturates_on_overflow() {
        // Cumulative addition must not panic when the running total
        // would overflow u64. Saturating_add caps at u64::MAX.
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/overflow", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        panel.set_tokens(child, u64::MAX, u64::MAX);
        panel.set_tokens(child, 1, 1);
        let view_tokens = panel.tokens.get(&child).copied().expect("token entry");
        assert_eq!(view_tokens, (u64::MAX, u64::MAX), "must saturate, not wrap");
    }

    #[test]
    fn set_tokens_feed_into_rendered_line() {
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/busy", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        panel.set_tokens(child, 8_000, 4_000);
        panel.set_activity(child, AgentActivity::Running("bash".to_string()));

        let mut buf: Vec<u8> = Vec::new();
        let caps = TerminalCaps::baseline();
        panel
            .render(0, &mut buf, &caps, Instant::now(), Utc::now())
            .expect("render");
        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.contains("bash"), "activity must surface: {out:?}");
        assert!(out.contains("12k"), "combined tokens must surface: {out:?}");
    }

    #[test]
    fn reset_tokens_drops_the_running_tally() {
        // /clear's contract: after the event-store swap the agent
        // panel's session-cumulative tally must go back to zero so the
        // next turn's saturating_add starts from a clean baseline
        // instead of inheriting the pre-clear running totals.
        let registry = fresh_registry();
        let root = confirm_root(&registry);
        let child = confirm_child(&registry, "/root/reset-target", root);
        let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
        panel.set_tokens(child, 5_000, 2_000);
        assert_eq!(panel.tokens.get(&child).copied(), Some((5_000, 2_000)));
        panel.reset_tokens(child);
        assert!(!panel.tokens.contains_key(&child), "tally must be cleared");

        // Calling reset_tokens for an unknown agent must be a no-op,
        // not a panic — /clear runs unconditionally on submission.
        let stranger = Uuid::new_v4();
        panel.reset_tokens(stranger);
    }
}
