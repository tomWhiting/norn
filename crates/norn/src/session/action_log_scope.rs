//! Federated queries over a set of per-agent [`ActionLog`]s.
//!
//! The `action_log` tool's `scope` argument widens a query from the
//! calling agent's own log to its descendants' logs (see
//! [`ActionLogTree`](crate::session::action_log_tree::ActionLogTree)).
//! This module holds the pure federation layer: given an ordered slice of
//! [`ScopedLog`]s (the caller first, then descendants in tree preorder),
//! it merges Level 1 entries by timestamp, resolves Level 2/3 look-ups
//! across the scope, and collects per-agent mutation ledgers.
//!
//! Attribution is structural: each result carries the index of the
//! [`ScopedLog`] it came from, and the label on that entry is registry
//! ground truth resolved by the tool — never a writable field on the
//! entry itself.

use std::sync::Arc;

use serde::Deserialize;
use uuid::Uuid;

use crate::session::action_log::{ActionLog, ActionLogContext, ActionLogDetail, ActionLogEntry};
use crate::session::mutation_ledger::MutationLedgerEntry;

/// One agent's log inside a federated query scope, with the label the
/// query output attributes its entries to.
pub struct ScopedLog {
    /// The agent whose log this is. `None` when the calling context
    /// carries no agent identity (a standalone runtime with no sub-agent
    /// infrastructure) — the log is then the caller's own, labeled
    /// `"root"`.
    pub agent_id: Option<Uuid>,
    /// Model-facing label: the agent's registry path when registered,
    /// `"root"` for the root agent, or the bare UUID otherwise.
    pub label: String,
    /// The agent's registry role, only when one is actually set.
    pub role: Option<String>,
    /// The agent's action log.
    pub log: Arc<ActionLog>,
}

/// A Level 1 entry paired with the index of the [`ScopedLog`] it came
/// from (an index into the scope slice, not an agent id — the caller owns
/// the slice and resolves labels through it).
pub struct LabeledEntry {
    /// Index into the scope slice identifying the owning agent.
    pub agent_idx: usize,
    /// The entry itself.
    pub entry: ActionLogEntry,
}

/// Optional scoping filter for `list`, `mutations`, and `follow_ups`
/// queries of the `action_log` tool. All fields are optional and combine
/// with AND semantics; an absent filter returns every entry.
#[derive(Debug, Default, Deserialize)]
pub struct ActionLogFilter {
    /// Keep only entries whose `tool_name` equals this value.
    #[serde(default)]
    pub tool: Option<String>,
    /// Keep only entries whose coarse outcome tag equals this value
    /// (`success`, `error`, or `blocked`).
    #[serde(default)]
    pub outcome: Option<String>,
    /// For `list`, keep only entries whose `summary_line` contains this
    /// substring. For `mutations`, keep only the ledger entry whose
    /// `file_path` equals this path exactly.
    #[serde(default)]
    pub file: Option<String>,
    /// Keep only entries recorded strictly after the entry with this
    /// `tool_call_id`. When the id is not present in the (merged) log, no
    /// entries match (there is nothing known to be after an absent
    /// marker).
    #[serde(default)]
    pub since: Option<String>,
    /// After all other filters, keep only the most recent `last` entries.
    #[serde(default)]
    pub last: Option<u32>,
}

impl ActionLogFilter {
    /// Apply the filter to `entries` (chronological order in,
    /// chronological order out).
    #[must_use]
    pub fn apply(&self, entries: Vec<ActionLogEntry>) -> Vec<ActionLogEntry> {
        self.apply_labeled(
            entries
                .into_iter()
                .map(|entry| LabeledEntry {
                    agent_idx: 0,
                    entry,
                })
                .collect(),
        )
        .into_iter()
        .map(|le| le.entry)
        .collect()
    }

    /// Apply the filter to a merged, timestamp-ordered labeled list. The
    /// `since` marker is positional within the merged list, so "after"
    /// means after in the federated timeline, regardless of which agent's
    /// log holds the marker entry.
    #[must_use]
    pub fn apply_labeled(&self, entries: Vec<LabeledEntry>) -> Vec<LabeledEntry> {
        let mut filtered: Vec<LabeledEntry> = match &self.since {
            Some(since) => match entries
                .iter()
                .position(|le| le.entry.tool_call_id == *since)
            {
                Some(idx) => entries.into_iter().skip(idx + 1).collect(),
                None => Vec::new(),
            },
            None => entries,
        };

        if let Some(tool) = &self.tool {
            filtered.retain(|le| le.entry.tool_name == *tool);
        }
        if let Some(outcome) = &self.outcome {
            filtered.retain(|le| le.entry.outcome.tag() == outcome.as_str());
        }
        if let Some(file) = &self.file {
            filtered.retain(|le| le.entry.summary_line.contains(file.as_str()));
        }
        if let Some(last) = self.last {
            let last = usize::try_from(last).unwrap_or(usize::MAX);
            if filtered.len() > last {
                filtered.drain(0..filtered.len() - last);
            }
        }
        filtered
    }
}

/// Collect the Level 1 entries of every log in `scoped`, in scope order,
/// merging them into one timestamp-ordered timeline only when the scope
/// spans more than one log.
///
/// A single-log scope (`self`, or one specific agent) deliberately keeps
/// the log's own insertion order: insertion order is the audit-trail
/// ground truth, while entry timestamps come from the non-monotonic wall
/// clock (`Utc::now()`) — a clock regression mid-session would otherwise
/// reorder the log and shift the positional `since` filter. The timestamp
/// merge exists solely to interleave several logs into one federated
/// timeline; there the sort is stable, so entries with equal timestamps
/// keep scope order (caller first, then descendants in tree preorder)
/// and, within one agent, the log's own insertion order.
#[must_use]
pub fn collect_labeled_entries(scoped: &[ScopedLog]) -> Vec<LabeledEntry> {
    let mut out = Vec::new();
    for (agent_idx, scoped_log) in scoped.iter().enumerate() {
        out.extend(
            scoped_log
                .log
                .entries()
                .into_iter()
                .map(|entry| LabeledEntry { agent_idx, entry }),
        );
    }
    merge_timeline(out, scoped.len())
}

/// Order collected entries for output given how many logs they came from:
/// more than one log merges by timestamp (stable sort — see
/// [`collect_labeled_entries`] for the tie semantics); one log (or none)
/// is returned untouched, in insertion order, because within a single log
/// insertion order is the audit ground truth and wall-clock timestamps
/// are not monotonic. Split from [`collect_labeled_entries`] so the
/// ordering decision is testable with hand-built timestamps, which
/// [`ActionLog`]'s public API never produces out of order on a healthy
/// clock.
fn merge_timeline(mut entries: Vec<LabeledEntry>, log_count: usize) -> Vec<LabeledEntry> {
    if log_count > 1 {
        entries.sort_by_key(|le| le.entry.timestamp);
    }
    entries
}

/// Resolve a Level 2 detail look-up across the scope, returning the
/// owning agent's index alongside the detail.
///
/// Logs are searched in scope order (caller first, then descendants in
/// tree preorder) and the first match wins — provider-assigned call ids
/// are unique within one agent's stream, so a cross-agent collision can
/// only come from a provider reusing ids across streams; the
/// deterministic scope order makes that case stable.
#[must_use]
pub fn find_detail(scoped: &[ScopedLog], call_id: &str) -> Option<(usize, ActionLogDetail)> {
    scoped
        .iter()
        .enumerate()
        .find_map(|(idx, s)| s.log.get_detail(call_id).map(|d| (idx, d)))
}

/// Resolve a Level 3 context look-up across the scope; same search order
/// and collision semantics as [`find_detail`].
#[must_use]
pub fn find_context(scoped: &[ScopedLog], call_id: &str) -> Option<(usize, ActionLogContext)> {
    scoped
        .iter()
        .enumerate()
        .find_map(|(idx, s)| s.log.get_context(call_id).map(|c| (idx, c)))
}

/// Collect every scope member's mutation ledger, each entry paired with
/// its owning agent's index. Entries appear in scope order (caller first,
/// then descendants in tree preorder), preserving each ledger's own
/// per-file ordering; revert statuses are evaluated lazily by the ledgers
/// at this call, exactly as in a self-scoped query.
#[must_use]
pub fn collect_mutations(scoped: &[ScopedLog]) -> Vec<(usize, MutationLedgerEntry)> {
    let mut out = Vec::new();
    for (agent_idx, scoped_log) in scoped.iter().enumerate() {
        out.extend(
            scoped_log
                .log
                .mutation_entries()
                .into_iter()
                .map(|entry| (agent_idx, entry)),
        );
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::action_log::{CompletionRecord, Outcome};
    use crate::session::store::EventStore;

    fn log_with(ids: &[&str], tool: &str) -> Arc<ActionLog> {
        let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        for id in ids {
            log.record_completion(CompletionRecord {
                tool_name: tool,
                tool_call_id: id,
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 1,
                follow_ups: Vec::new(),
                post_validate_outcome: None,
                level_1_only: false,
            });
        }
        log
    }

    fn scoped(label: &str, log: Arc<ActionLog>) -> ScopedLog {
        ScopedLog {
            agent_id: Some(Uuid::new_v4()),
            label: label.to_owned(),
            role: None,
            log,
        }
    }

    #[test]
    fn merged_entries_are_timestamp_ordered_and_attributed() {
        // Interleave by recording alternately into two logs: timestamps
        // are monotonically non-decreasing across the two, so the merged
        // order must alternate (stable sort keeps scope order on ties).
        let parent = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let child = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let record = |log: &ActionLog, id: &str| {
            log.record_completion(CompletionRecord {
                tool_name: "read",
                tool_call_id: id,
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({}),
                args: serde_json::json!({}),
                duration_ms: 1,
                follow_ups: Vec::new(),
                post_validate_outcome: None,
                level_1_only: false,
            });
        };
        // Force strictly increasing timestamps so the assertion is
        // deterministic even on coarse clocks.
        record(&parent, "p-1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(&child, "c-1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(&parent, "p-2");

        let scope = vec![scoped("root", parent), scoped("/spawn/child", child)];
        let merged = collect_labeled_entries(&scope);
        let order: Vec<&str> = merged
            .iter()
            .map(|le| le.entry.tool_call_id.as_str())
            .collect();
        assert_eq!(order, vec!["p-1", "c-1", "p-2"]);
        assert_eq!(merged[0].agent_idx, 0);
        assert_eq!(merged[1].agent_idx, 1);
        assert_eq!(merged[2].agent_idx, 0);
    }

    /// Regression: within one log, insertion order is the audit ground
    /// truth and must survive a wall-clock regression (`Utc::now()` is
    /// not monotonic); only a multi-log federation merges by timestamp.
    /// Built on hand-made entries because `ActionLog` stamps entries
    /// itself and offers no clock injection.
    #[test]
    fn single_log_keeps_insertion_order_despite_clock_regression() {
        let entry = |id: &str, timestamp| ActionLogEntry {
            tool_name: "read".to_owned(),
            tool_call_id: id.to_owned(),
            tool_use_description: String::new(),
            timestamp,
            outcome: Outcome::Success,
            summary_line: String::new(),
        };
        let now = chrono::Utc::now();
        // The clock regressed between insertions: the SECOND entry
        // carries the EARLIER timestamp.
        let out_of_order = || {
            vec![
                LabeledEntry {
                    agent_idx: 0,
                    entry: entry("first-inserted", now),
                },
                LabeledEntry {
                    agent_idx: 0,
                    entry: entry("second-inserted", now - chrono::Duration::seconds(30)),
                },
            ]
        };
        let ids = |entries: &[LabeledEntry]| -> Vec<String> {
            entries
                .iter()
                .map(|le| le.entry.tool_call_id.clone())
                .collect()
        };

        // One scoped log (scope=self or one specific agent): insertion
        // order preserved, positional `since` semantics intact.
        let kept = merge_timeline(out_of_order(), 1);
        assert_eq!(ids(&kept), ["first-inserted", "second-inserted"]);

        // Federation over several logs: the merged timeline sorts by
        // timestamp.
        let sorted = merge_timeline(out_of_order(), 2);
        assert_eq!(ids(&sorted), ["second-inserted", "first-inserted"]);
    }

    #[test]
    fn since_marker_applies_across_the_merged_timeline() {
        let parent = log_with(&["p-1"], "read");
        let child = log_with(&["c-1"], "edit");
        let scope = vec![scoped("root", parent), scoped("/spawn/child", child)];
        let merged = collect_labeled_entries(&scope);

        let filter = ActionLogFilter {
            since: Some("p-1".to_owned()),
            ..ActionLogFilter::default()
        };
        let after = filter.apply_labeled(merged);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].entry.tool_call_id, "c-1");

        // Absent marker matches nothing.
        let filter = ActionLogFilter {
            since: Some("missing".to_owned()),
            ..ActionLogFilter::default()
        };
        let scope2 = vec![scoped("root", log_with(&["p-1"], "read"))];
        assert!(
            filter
                .apply_labeled(collect_labeled_entries(&scope2))
                .is_empty()
        );
    }

    #[test]
    fn detail_and_context_resolve_in_scope_order() {
        let parent = log_with(&["shared-id"], "read");
        let child = log_with(&["shared-id", "child-only"], "edit");
        let scope = vec![scoped("root", parent), scoped("/spawn/child", child)];

        // Collision: the caller's own log wins (scope order).
        let (idx, detail) = find_detail(&scope, "shared-id").expect("found");
        assert_eq!(idx, 0);
        assert_eq!(detail.entry.tool_name, "read");

        // Child-only ids resolve into the child's log.
        let (idx, detail) = find_detail(&scope, "child-only").expect("found");
        assert_eq!(idx, 1);
        assert_eq!(detail.entry.tool_name, "edit");
        let (idx, _ctx) = find_context(&scope, "child-only").expect("found");
        assert_eq!(idx, 1);

        assert!(find_detail(&scope, "nope").is_none());
        assert!(find_context(&scope, "nope").is_none());
    }

    #[test]
    fn mutations_collect_per_agent_ledgers_with_attribution() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let parent_file = dir.path().join("parent.rs");
        let child_file = dir.path().join("child.rs");
        std::fs::write(&parent_file, "p\n").unwrap();
        std::fs::write(&child_file, "c\n").unwrap();

        let mutate = |log: &ActionLog, id: &str, path: &std::path::Path| {
            log.record_completion(CompletionRecord {
                tool_name: "edit",
                tool_call_id: id,
                tool_use_description: "",
                outcome: Outcome::Success,
                output: &serde_json::json!({
                    "path": path.to_string_lossy(),
                    "blast_radius": { "lines_added": 1, "lines_removed": 0 },
                }),
                args: serde_json::json!({}),
                duration_ms: 1,
                follow_ups: Vec::new(),
                post_validate_outcome: None,
                level_1_only: false,
            });
        };
        let parent = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        let child = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
        mutate(&parent, "p-1", &parent_file);
        mutate(&child, "c-1", &child_file);

        let scope = vec![scoped("root", parent), scoped("/spawn/child", child)];
        let collected = collect_mutations(&scope);
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].0, 0);
        assert_eq!(collected[0].1.file_path, parent_file);
        assert_eq!(collected[1].0, 1);
        assert_eq!(collected[1].1.file_path, child_file);
    }

    #[test]
    fn plain_apply_matches_labeled_apply() {
        let log = log_with(&["a", "b", "c"], "read");
        let filter = ActionLogFilter {
            last: Some(2),
            ..ActionLogFilter::default()
        };
        let plain = filter.apply(log.entries());
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[0].tool_call_id, "b");
        assert_eq!(plain[1].tool_call_id, "c");
    }
}
