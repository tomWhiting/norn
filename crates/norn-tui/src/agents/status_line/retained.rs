//! Typed status rows from one existing panel snapshot; no terminal writer or agent authority.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use norn::agent::registry::{AgentEntry, AgentStatus};
use uuid::Uuid;

use crate::render::retained_text::{TextAttribute, TextStyle};

use super::{
    AgentActivity, AgentStatusPanel, format_status_line, is_last_at_depth, row_foreground, tree,
    tree_prefix,
};

/// Stable row identity or a count of agents omitted by the existing collapse policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetainedAgentRowKind {
    /// A registry entry; presentation does not grant access to its conversation or input.
    Agent {
        /// Actual registry identity.
        id: Uuid,
        /// Actual registry parent, including an ancestor omitted from the visible slice.
        parent_id: Option<Uuid>,
    },
    /// Agents outside the visible slice.
    Overflow {
        /// Number of omitted agents.
        count: usize,
    },
}

/// One untruncated status row; the frame owner escapes controls and clips whole graphemes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedAgentRow {
    /// Registry identity or overflow evidence.
    pub kind: RetainedAgentRowKind,
    /// Existing status, activity, token and elapsed formatting, with no generated ANSI.
    pub text: String,
    /// Existing palette and idle intensity represented directly.
    pub style: TextStyle,
}

impl RetainedAgentRow {
    /// The same dim overflow wording as the existing panel.
    #[must_use]
    pub fn overflow(count: usize) -> Self {
        Self {
            kind: RetainedAgentRowKind::Overflow { count },
            text: format!("⋯ {count} more active agents"),
            style: TextStyle {
                attributes: TextStyle::default().attributes.with(TextAttribute::Dim),
                ..TextStyle::default()
            },
        }
    }
}

/// One coherent collapsed tree plus the next display-time boundary.
pub struct RetainedAgentSnapshot {
    /// Genealogical display order, followed by the optional overflow row.
    pub rows: Vec<RetainedAgentRow>,
    /// Full admitted list for the explicit read-only Agents pane, including root.
    pub all_rows: Vec<RetainedAgentRow>,
    /// Full-list age boundary, used only while its pane is visible.
    pub pane_next_refresh: Option<Instant>,
    /// Earliest existing age/hold/recency boundary; driven by the frontend's existing tick.
    pub next_refresh: Option<Instant>,
}

impl AgentStatusPanel {
    /// Snapshot exactly once, preserving hold expiry, recovery ownership and tombstone genealogy.
    pub fn retained_snapshot(
        &mut self,
        now: Instant,
        now_utc: DateTime<Utc>,
    ) -> RetainedAgentSnapshot {
        let (view, entries) = self.snapshot(now);
        let entries_by_id: HashMap<_, _> = entries.iter().map(|entry| (entry.id, entry)).collect();
        let mut parent_of: HashMap<_, _> = self
            .registry
            .read()
            .tombstones()
            .into_iter()
            .map(|entry| (entry.id, entry.parent_id))
            .collect();
        parent_of.extend(entries.iter().map(|entry| (entry.id, entry.parent_id)));
        let ordered = tree::order_for_display(view.visible, &parent_of);
        let mut rows = self.project_retained_rows(&ordered, &entries_by_id, now_utc);
        if view.overflow_count > 0 {
            rows.push(RetainedAgentRow::overflow(view.overflow_count));
        }
        // The same snapshot already decided terminal retention/reclamation. A retained
        // terminal entry keeps its hold key even when only pending recovery keeps it alive.
        let admitted: Vec<_> = entries
            .iter()
            .filter(|entry| !entry.status.is_terminal() || self.holds.contains_key(&entry.id))
            .map(|entry| tree::CandidateEntry {
                id: entry.id,
                parent_id: entry.parent_id,
                spawned_at: entry.spawned_at,
                last_change_at: self.last_change_at.get(&entry.id).copied().unwrap_or(now),
                status: entry.status,
            })
            .collect();
        let all_ordered = tree::order_for_display(admitted, &parent_of);
        let all_rows = self.project_retained_rows(&all_ordered, &entries_by_id, now_utc);
        let mut boundaries: Vec<Instant> = self
            .holds
            .values()
            .copied()
            .filter(|deadline| *deadline > now)
            .collect();
        boundaries.extend(
            self.last_change_at
                .values()
                .map(|changed| *changed + tree::RECENT_CHANGE_WINDOW)
                .filter(|deadline| *deadline > now),
        );
        let lifecycle = boundaries.into_iter().min();
        let age_boundary = |ordered: &[(tree::CandidateEntry, usize)]| {
            ordered
                .iter()
                .map(|(candidate, _)| next_age_boundary(now, now_utc, candidate.spawned_at))
                .min()
        };
        let next_refresh = if rows.is_empty() && self.holds.is_empty() {
            None
        } else {
            lifecycle.into_iter().chain(age_boundary(&ordered)).min()
        };
        let pane_next_refresh = lifecycle
            .into_iter()
            .chain(age_boundary(&all_ordered))
            .min();
        RetainedAgentSnapshot {
            rows,
            all_rows,
            pane_next_refresh,
            next_refresh,
        }
    }

    fn project_retained_rows(
        &self,
        ordered: &[(tree::CandidateEntry, usize)],
        entries: &HashMap<Uuid, &AgentEntry>,
        now_utc: DateTime<Utc>,
    ) -> Vec<RetainedAgentRow> {
        ordered
            .iter()
            .enumerate()
            .filter_map(|(index, (candidate, depth))| {
                // Entries and candidates derive from the same immutable snapshot.
                let entry = entries.get(&candidate.id)?;
                let activity = self.activity.get(&entry.id).map(|entry| &entry.activity);
                let (input, output) = self.tokens.get(&entry.id).copied().unwrap_or((0, 0));
                let prefix = tree_prefix(*depth, is_last_at_depth(ordered, index));
                let mut text = format_status_line(entry, activity, input, output, now_utc, &prefix);
                let idle = matches!(
                    entry.status,
                    AgentStatus::Active | AgentStatus::Completing | AgentStatus::Idle
                ) && matches!(activity, Some(AgentActivity::Idle));
                let mut style = TextStyle::default();
                if idle {
                    text = text.replacen('●', "◌", 1);
                    style.attributes = style.attributes.with(TextAttribute::Dim);
                } else {
                    style.foreground = row_foreground(entry.status, activity)
                        .map(|colour| [colour.red, colour.green, colour.blue]);
                }
                Some(RetainedAgentRow {
                    kind: RetainedAgentRowKind::Agent {
                        id: entry.id,
                        parent_id: entry.parent_id,
                    },
                    text,
                    style,
                })
            })
            .collect()
    }
}

pub(super) fn next_age_boundary(
    now: Instant,
    now_utc: DateTime<Utc>,
    spawned: DateTime<Utc>,
) -> Instant {
    // Match format_elapsed's future-time clamp and second/minute display precision.
    let seconds = now_utc
        .signed_duration_since(spawned)
        .num_seconds()
        .max(0)
        .unsigned_abs();
    let quantum = if seconds < 3600 { 1 } else { 60 };
    let nanos = if now_utc < spawned {
        0
    } else {
        (now_utc.timestamp_subsec_nanos() + 1_000_000_000 - spawned.timestamp_subsec_nanos())
            % 1_000_000_000
    };
    // The next boundary is 1..=60 seconds away before subtracting a subsecond fraction.
    let remaining_nanos = (quantum - seconds % quantum) * 1_000_000_000 - u64::from(nanos);
    now + Duration::from_nanos(remaining_nanos)
}
