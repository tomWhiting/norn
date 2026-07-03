//! Deterministic watch records and the per-manager watch registry (NP-002).
//!
//! A [`Watch`] is an agent-authored `sh -c` filter script attached to a
//! manager-owned background process. It runs incrementally over new spool
//! regions — zero model cost — and on a match wakes the owning agent with the
//! matching excerpt (see [`watch_exec`](super::watch_exec)). This module owns
//! the records and their lifecycle; [`watch_exec`](super::watch_exec) owns the
//! incremental filter execution over the [`ProcessHandle`] subscription seam.
//!
//! ## Lifecycle
//!
//! - **attach** — [`WatchRegistry::attach`] assigns a stable short id (`w1`,
//!   `w2`, … monotonic per manager, **no cap**), subscribes over the handle
//!   seam, and spawns one executor task per watch.
//! - **detach** — [`WatchRegistry::detach`] cancels the watch's `detach` token
//!   and drops its task handle: the executor stops after any in-flight filter
//!   run completes; no region is examined after detach (NP-002 R5).
//! - **finalize** — on process exit the manager calls
//!   [`WatchRegistry::finalize_for_process`], which joins the drains (so the
//!   final region is fully committed), then triggers each watch's `finalize`
//!   token so it filters the remaining region **before** the completion notice,
//!   then removes the watches (NP-002 R5).
//! - **shutdown** — [`WatchRegistry::abort_all`] aborts every task with no
//!   final-region alert (a shutdown kill is not a watch event).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tool::context::ProcessEnv;

use super::handle::{ProcessHandle, ProcessStatus};
use super::manager::ProcessNotifier;
use super::watch_exec::{WatchExec, run_watch};

/// A deterministic watch record: an agent-authored filter attached to one
/// manager-owned process.
#[derive(Clone, Debug)]
pub struct Watch {
    /// Stable short id, e.g. `"w1"` (monotonic per manager).
    pub watch_id: String,
    /// The process this watch is attached to, e.g. `"p1"`.
    pub process_id: String,
    /// The agent's human-readable statement of what to watch for.
    pub brief: String,
    /// The agent-authored shell filter script, run via `sh -c`.
    pub filter: String,
}

/// The structured, agent-agnostic payload of a watch alert. The delivery
/// adapter (wired at assembly) turns this into an injected message; a future
/// cheap-model watcher agent (INTERNAL-AGENTS §5 step 2) can consume the same
/// shape unchanged — which is why it is structured data, not prose.
#[derive(Clone, Debug)]
pub struct WatchAlert {
    /// The watch that produced this alert (`"w1"`).
    pub watch_id: String,
    /// The watched process (`"p1"`).
    pub process_id: String,
    /// The agent's brief for this watch.
    pub brief: String,
    /// First byte offset (inclusive) of the examined spool region.
    pub spool_start: u64,
    /// End byte offset (exclusive) of the examined spool region.
    pub spool_end: u64,
    /// Whether this is a match or a filter-execution failure.
    pub kind: WatchAlertKind,
}

/// Whether a [`WatchAlert`] reports a filter match or a filter-execution
/// failure.
#[derive(Clone, Debug)]
pub enum WatchAlertKind {
    /// The filter matched (exit 0). Carries the filter's stdout verbatim as the
    /// excerpt and the moment the match was observed.
    Match {
        /// The filter's stdout for the matching region (byte-equal, not
        /// re-derived from the spool).
        excerpt: String,
        /// When the match was observed.
        matched_at: DateTime<Utc>,
    },
    /// The filter could not be run to a clean match/no-match verdict — a spawn
    /// error, a signal kill, or a stdin write failure. The watch stays attached
    /// (NP-002 R4); the agent decides whether to unwatch or fix the filter.
    Error {
        /// Human-readable description of the failure.
        error: String,
    },
}

/// Failure attaching a watch (NP-002 R1).
#[derive(Clone, Debug)]
pub enum WatchAttachError {
    /// No process with that id is known to this manager.
    ProcessNotFound {
        /// The unknown process id.
        process_id: String,
    },
    /// The named process is already terminal — there is nothing left to watch.
    Terminal {
        /// The process id.
        process_id: String,
        /// Its terminal status.
        status: ProcessStatus,
    },
}

impl WatchAttachError {
    pub(super) fn not_found(process_id: &str) -> Self {
        Self::ProcessNotFound {
            process_id: process_id.to_owned(),
        }
    }

    pub(super) fn terminal(process_id: &str, status: ProcessStatus) -> Self {
        Self::Terminal {
            process_id: process_id.to_owned(),
            status,
        }
    }
}

/// Failure detaching a watch (NP-002 R1).
#[derive(Clone, Copy, Debug)]
pub enum WatchError {
    /// No active watch has that id.
    NotFound,
}

/// The inputs for one [`WatchRegistry::attach`] call, bundled so the attach
/// surface stays a small, named request rather than a long positional list.
pub(super) struct NewWatch {
    /// The agent's human-readable brief.
    pub(super) brief: String,
    /// The agent-authored `sh -c` filter script.
    pub(super) filter: String,
    /// The agent's working directory for the filter's executions.
    pub(super) cwd: PathBuf,
    /// The agent's process environment for the filter.
    pub(super) env: Option<ProcessEnv>,
    /// The alert delivery sink (the manager's notifier).
    pub(super) notifier: Option<Arc<dyn ProcessNotifier>>,
}

/// One registry entry: the record, the process it watches, its cancellation
/// tokens, and the executor task handle.
struct WatchEntry {
    watch: Watch,
    /// Numeric id of the watched process (`1` for `"p1"`).
    process_id: u64,
    /// Cancelled by `detach` (unwatch): stop after the in-flight run, no final
    /// region.
    detach: CancellationToken,
    /// Cancelled by `finalize` (exit): filter the remaining region, then end.
    finalize: CancellationToken,
    /// The executor task. Aborted at shutdown; joined at finalize; detached
    /// (dropped, not aborted) at unwatch so the in-flight run still completes.
    /// `None` only during finalization, after the task has been taken out to be
    /// awaited while the entry deliberately stays in the map (so `list` and
    /// `unwatch` remain honest until the final region is filtered).
    task: Option<JoinHandle<()>>,
}

/// The per-manager watch registry: monotonic ids and the active-watch map.
/// Imposes **no cap** on watch count per process or overall.
#[derive(Debug)]
pub struct WatchRegistry {
    next_id: AtomicU64,
    entries: Mutex<BTreeMap<u64, WatchEntry>>,
}

impl std::fmt::Debug for WatchEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchEntry")
            .field("watch", &self.watch)
            .field("process_id", &self.process_id)
            .finish_non_exhaustive()
    }
}

impl Default for WatchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchRegistry {
    /// A fresh registry with the id counter at `w1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Attach a new watch to `handle` and spawn its executor task. No cap is
    /// imposed. Must be called within a Tokio runtime (the caller — a tool
    /// execution — always is).
    pub(super) fn attach(&self, handle: &ProcessHandle, process_id: u64, spec: NewWatch) -> Watch {
        let numeric = self.next_id.fetch_add(1, Ordering::AcqRel);
        let watch = Watch {
            watch_id: format!("w{numeric}"),
            process_id: handle.label().to_owned(),
            brief: spec.brief,
            filter: spec.filter,
        };
        let (len_rx, reader) = handle.subscribe();
        let detach = CancellationToken::new();
        let finalize = CancellationToken::new();
        let task = tokio::spawn(run_watch(WatchExec {
            watch: watch.clone(),
            reader,
            len_rx,
            detach: detach.clone(),
            finalize: finalize.clone(),
            cwd: spec.cwd,
            env: spec.env,
            notifier: spec.notifier,
        }));
        self.entries.lock().insert(
            numeric,
            WatchEntry {
                watch: watch.clone(),
                process_id,
                detach,
                finalize,
                task: Some(task),
            },
        );
        watch
    }

    /// Detach the watch named `watch_label` (`"w1"`). Cancels its `detach`
    /// token and drops the task handle, so the executor stops after any
    /// in-flight run — nothing after detach is examined.
    ///
    /// # Errors
    ///
    /// [`WatchError::NotFound`] when no active watch has that id.
    pub(super) fn detach(&self, watch_label: &str) -> Result<(), WatchError> {
        let numeric = parse_watch_label(watch_label).ok_or(WatchError::NotFound)?;
        match self.entries.lock().remove(&numeric) {
            Some(entry) => {
                // Cancel, but do not abort: dropping the JoinHandle detaches the
                // task, so an in-flight filter run still completes before the
                // executor observes the cancel and ends (NP-002 R5).
                entry.detach.cancel();
                Ok(())
            }
            None => Err(WatchError::NotFound),
        }
    }

    /// Whether any watch is attached to the process with numeric id `id`.
    #[must_use]
    pub(super) fn has_watches(&self, id: u64) -> bool {
        self.entries.lock().values().any(|e| e.process_id == id)
    }

    /// Every active watch attached to the process with numeric id `id`.
    #[must_use]
    pub(super) fn watches_for(&self, id: u64) -> Vec<Watch> {
        self.entries
            .lock()
            .values()
            .filter(|e| e.process_id == id)
            .map(|e| e.watch.clone())
            .collect()
    }

    /// Finalize every watch attached to the process with numeric id `id`
    /// (NP-002 R5): join `drains` so the final spool region is fully committed,
    /// then trigger each watch's `finalize` token and await its task so the
    /// final-region alert is delivered **before** the caller delivers the
    /// completion notice. Removes the finalized watches from the registry.
    pub(super) async fn finalize_for_process(
        &self,
        id: u64,
        drains: Vec<JoinHandle<std::io::Result<()>>>,
    ) {
        // Snapshot the finalize tokens WITHOUT removing the entries. The drain
        // join below is unbounded (a pipe-holding grandchild keeps the drains
        // alive indefinitely), so removing entries first would make every watch
        // a phantom for that whole window — alerting and live, yet invisible to
        // `list` and unstoppable by `unwatch` (NotFound), leaving `op=kill` the
        // only escape. Keeping them in the map through the join keeps both
        // truthful.
        let targets: Vec<(u64, CancellationToken)> = self
            .entries
            .lock()
            .iter()
            .filter(|(_, e)| e.process_id == id)
            .map(|(k, e)| (*k, e.finalize.clone()))
            .collect();
        if targets.is_empty() {
            return;
        }
        // Join the drains: once both drain tasks finish, every byte the process
        // wrote is committed to the spool, so each watch's final read sees the
        // complete final region rather than racing the drain flush.
        for drain in drains {
            if let Err(error) = drain.await
                && !error.is_cancelled()
            {
                tracing::warn!(%error, "a spool drain task panicked during watch finalization");
            }
        }
        // Finalize each watch in turn, removing it from the map only once its
        // final region has been filtered and its task joined — never before.
        // The task is taken out (leaving the entry in place) so `list`/`unwatch`
        // stay honest right up to the join; a concurrent `unwatch` that removed
        // the entry first simply leaves nothing to take, which is fine.
        for (numeric, finalize) in targets {
            finalize.cancel();
            let task = self
                .entries
                .lock()
                .get_mut(&numeric)
                .and_then(|entry| entry.task.take());
            if let Some(task) = task
                && let Err(error) = task.await
                && !error.is_cancelled()
            {
                tracing::warn!(
                    watch = %numeric,
                    %error,
                    "a watch executor task panicked during finalization",
                );
            }
            self.entries.lock().remove(&numeric);
        }
    }

    /// Abort every active watch task without a final-region alert (shutdown).
    pub(super) fn abort_all(&self) {
        for (_, entry) in self.entries.lock().split_off(&0) {
            if let Some(task) = entry.task {
                task.abort();
            }
        }
    }
}

/// Parse a `w<n>` watch label into its numeric id.
fn parse_watch_label(label: &str) -> Option<u64> {
    label
        .trim()
        .strip_prefix('w')
        .and_then(|n| n.parse::<u64>().ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_watch_label_round_trip() {
        assert_eq!(parse_watch_label("w1"), Some(1));
        assert_eq!(parse_watch_label("w42"), Some(42));
        assert_eq!(parse_watch_label(" w7 "), Some(7));
        assert_eq!(parse_watch_label("p1"), None);
        assert_eq!(parse_watch_label("w"), None);
    }
}
