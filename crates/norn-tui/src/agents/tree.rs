//! Agent-tree collapse heuristic.
//!
//! [`collapse`] takes a snapshot of agent candidates and reduces it to at
//! most five visible entries, reporting how many fell into overflow. The
//! function is pure — the caller passes the current [`Instant`] so the
//! recency check is testable without wall-clock dependence.
//!
//! Priority, applied top-down and stopping when five rows are filled:
//!
//! 1. Root entry (`parent_id == None`) — included first when any non-root
//!    candidate exists. When the snapshot contains only root entries the
//!    view is empty: the single-agent case (R1) shows zero chrome.
//! 2. Most-recently-spawned active agents (status is `Spawning`, `Active`,
//!    or `Completing`), sorted by `spawned_at` descending.
//! 3. Agents whose status changed in the last five seconds, sorted by
//!    `last_change_at` descending. Captures completed/failed entries the
//!    panel is holding briefly (R5) so they stay visible while held.
//! 4. Oldest active agents fill any remaining slots, sorted by
//!    `spawned_at` ascending — a defensive backstop when steps 2-3 do
//!    not exhaust available activity.
//!
//! Any candidate that does not land in `visible` is counted in
//! `overflow_count`. Rendering the `⋯ N more active agents` summary row
//! is the panel's responsibility (see [`crate::agents::status_line`]).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use norn::agent::registry::AgentStatus;

/// Maximum number of agent rows the fixed panel will show before
/// collapsing the remainder into an overflow summary.
pub const MAX_VISIBLE: usize = 5;

/// Window during which a recently-changed agent is prioritised for
/// inclusion in the visible set (step 3 of the priority order).
pub const RECENT_CHANGE_WINDOW: Duration = Duration::from_secs(5);

/// A single agent candidate for the visible slice.
///
/// The collapse function operates on these projections rather than full
/// [`norn::agent::registry::AgentEntry`] records so that the panel can
/// thread `Instant`-keyed bookkeeping (last status-change timestamps,
/// hold deadlines) without the registry needing to learn about it.
#[derive(Clone, Debug)]
pub struct CandidateEntry {
    /// Stable agent identifier.
    pub id: Uuid,
    /// Parent agent id; `None` for root.
    pub parent_id: Option<Uuid>,
    /// When the agent was reserved on the registry.
    pub spawned_at: DateTime<Utc>,
    /// When the panel last observed a status transition for this entry.
    pub last_change_at: Instant,
    /// Current lifecycle status as reported by the registry.
    pub status: AgentStatus,
}

/// Result of one collapse pass.
#[derive(Clone, Debug)]
pub struct CollapsedView {
    /// Entries that should be rendered, in display order.
    pub visible: Vec<CandidateEntry>,
    /// Number of input entries that did not make it into `visible`.
    /// Drives the `⋯ N more active agents` overflow row.
    pub overflow_count: usize,
}

/// Whether `status` counts as active for steps 2 and 4 — anything that
/// is not yet terminal (`Completed`, `Failed`).
fn is_active(status: AgentStatus) -> bool {
    !matches!(status, AgentStatus::Completed | AgentStatus::Failed)
}

/// Reduce `entries` to at most [`MAX_VISIBLE`] rows plus an overflow
/// count.
///
/// See the module docs for the full priority order. The function makes
/// no syscalls and clones only the entries that survive the pass, so it
/// is safe to call on every render frame.
#[must_use]
pub fn collapse(entries: &[CandidateEntry], now: Instant) -> CollapsedView {
    let roots: Vec<&CandidateEntry> = entries.iter().filter(|e| e.parent_id.is_none()).collect();
    let non_roots: Vec<&CandidateEntry> =
        entries.iter().filter(|e| e.parent_id.is_some()).collect();

    if non_roots.is_empty() {
        return CollapsedView {
            visible: Vec::new(),
            overflow_count: 0,
        };
    }

    let mut visible: Vec<CandidateEntry> = Vec::with_capacity(MAX_VISIBLE);
    let mut included: HashSet<Uuid> = HashSet::new();

    // Step 1: root entries. The design guarantees only one root in
    // practice but the loop handles multi-root snapshots without
    // panicking on the unusual case.
    for root in &roots {
        if visible.len() >= MAX_VISIBLE {
            break;
        }
        if included.insert(root.id) {
            visible.push((*root).clone());
        }
    }

    // Step 2: most-recently-spawned active non-root agents.
    let mut active_desc: Vec<&CandidateEntry> = non_roots
        .iter()
        .copied()
        .filter(|e| is_active(e.status))
        .collect();
    active_desc.sort_by_key(|e| std::cmp::Reverse(e.spawned_at));
    for entry in &active_desc {
        if visible.len() >= MAX_VISIBLE {
            break;
        }
        if !included.contains(&entry.id) {
            included.insert(entry.id);
            visible.push((*entry).clone());
        }
    }

    // Step 3: agents with a status change inside the recency window —
    // covers entries on the 3-second terminal-status hold (R5) so they
    // do not vanish from the panel while the hold is active.
    let mut recent: Vec<&CandidateEntry> = non_roots
        .iter()
        .copied()
        .filter(|e| !included.contains(&e.id))
        .filter(|e| now.saturating_duration_since(e.last_change_at) < RECENT_CHANGE_WINDOW)
        .collect();
    recent.sort_by_key(|e| std::cmp::Reverse(e.last_change_at));
    for entry in &recent {
        if visible.len() >= MAX_VISIBLE {
            break;
        }
        if !included.contains(&entry.id) {
            included.insert(entry.id);
            visible.push((*entry).clone());
        }
    }

    // Step 4: oldest active fills any remaining slots.
    let mut oldest: Vec<&CandidateEntry> = non_roots
        .iter()
        .copied()
        .filter(|e| is_active(e.status) && !included.contains(&e.id))
        .collect();
    oldest.sort_by_key(|e| e.spawned_at);
    for entry in &oldest {
        if visible.len() >= MAX_VISIBLE {
            break;
        }
        if !included.contains(&entry.id) {
            included.insert(entry.id);
            visible.push((*entry).clone());
        }
    }

    let overflow_count = entries.iter().filter(|e| !included.contains(&e.id)).count();

    CollapsedView {
        visible,
        overflow_count,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::*;

    fn candidate(
        parent_id: Option<Uuid>,
        spawned_offset_secs: i64,
        status: AgentStatus,
        last_change: Instant,
    ) -> CandidateEntry {
        CandidateEntry {
            id: Uuid::new_v4(),
            parent_id,
            spawned_at: Utc::now() - chrono::Duration::seconds(spawned_offset_secs),
            last_change_at: last_change,
            status,
        }
    }

    #[test]
    fn empty_snapshot_yields_empty_view() {
        let now = Instant::now();
        let view = collapse(&[], now);
        assert!(view.visible.is_empty());
        assert_eq!(view.overflow_count, 0);
    }

    #[test]
    fn lone_root_is_not_visible() {
        // R1 interaction: root-only snapshot collapses to zero chrome.
        let now = Instant::now();
        let root = candidate(None, 60, AgentStatus::Active, now);
        let view = collapse(&[root], now);
        assert!(view.visible.is_empty());
        assert_eq!(view.overflow_count, 0);
    }

    #[test]
    fn eight_agents_collapse_to_five_plus_overflow_three() {
        let now = Instant::now();
        let root = candidate(None, 100, AgentStatus::Active, now);
        let root_id = Some(root.id);
        let mut entries = vec![root];
        for offset in (10..=70).step_by(10) {
            entries.push(candidate(root_id, offset, AgentStatus::Active, now));
        }
        assert_eq!(entries.len(), 8, "1 root + 7 children");

        let view = collapse(&entries, now);
        assert_eq!(view.visible.len(), 5);
        assert_eq!(view.overflow_count, 3);
    }

    #[test]
    fn root_always_present_when_children_exist() {
        let now = Instant::now();
        let root = candidate(None, 100, AgentStatus::Active, now);
        let root_id_value = root.id;
        let parent = Some(root.id);
        let mut entries = vec![root];
        for offset in (5..=35).step_by(5) {
            entries.push(candidate(parent, offset, AgentStatus::Active, now));
        }

        let view = collapse(&entries, now);
        assert_eq!(view.visible.len(), 5);
        assert!(
            view.visible.iter().any(|e| e.id == root_id_value),
            "root must appear regardless of how many children exist"
        );
    }

    #[test]
    fn most_recently_spawned_active_fills_first() {
        let now = Instant::now();
        let root = candidate(None, 100, AgentStatus::Active, now);
        let parent = Some(root.id);
        let mut entries = vec![root];

        // Children spawned at 80,70,...,10s ago — 10s ago is the newest.
        let mut child_ids: Vec<Uuid> = Vec::new();
        for offset in (10..=80).step_by(10) {
            let child = candidate(parent, offset, AgentStatus::Active, now);
            child_ids.push(child.id);
            entries.push(child);
        }
        // child_ids ordering: spawned_offset 10,20,30,...,80
        // most-recent (smallest offset) = index 0 — id at offset 10.

        let view = collapse(&entries, now);
        assert_eq!(view.visible.len(), 5);
        // visible[0] is root, then four most-recent children.
        let expected_recent: Vec<Uuid> = child_ids.iter().take(4).copied().collect();
        let actual_children: Vec<Uuid> = view.visible.iter().skip(1).map(|e| e.id).collect();
        assert_eq!(actual_children, expected_recent);
    }

    #[test]
    fn completed_entry_inside_recency_window_is_kept() {
        let now = Instant::now();
        let root = candidate(None, 100, AgentStatus::Active, now);
        let parent = Some(root.id);
        // One completed child whose status changed 1 second ago.
        let completed = candidate(
            parent,
            5,
            AgentStatus::Completed,
            now.checked_sub(Duration::from_secs(1)).unwrap(),
        );
        let completed_id = completed.id;

        let view = collapse(&[root, completed], now);
        assert_eq!(view.visible.len(), 2);
        assert!(view.visible.iter().any(|e| e.id == completed_id));
        assert_eq!(view.overflow_count, 0);
    }

    #[test]
    fn completed_entry_outside_recency_window_is_dropped() {
        let now = Instant::now();
        let root = candidate(None, 100, AgentStatus::Active, now);
        let parent = Some(root.id);
        // Completed 10s ago — outside the 5s window — and not active, so
        // it is not picked up by steps 2 or 4 either.
        let completed = candidate(
            parent,
            120,
            AgentStatus::Completed,
            now.checked_sub(Duration::from_secs(10)).unwrap(),
        );
        let completed_id = completed.id;

        let view = collapse(&[root, completed], now);
        assert!(!view.visible.iter().any(|e| e.id == completed_id));
        assert_eq!(view.overflow_count, 1);
    }
}
