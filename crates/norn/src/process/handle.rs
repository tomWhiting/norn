//! [`ProcessHandle`] — status inspection, process-group kill, exit
//! notification, and the subscription seam a watcher (NP-002) attaches to.
//!
//! A handle is a cheap clone of the shared per-process state
//! ([`ProcessShared`]). It exposes everything a consumer needs without
//! reaching into [`ProcessManager`](super::manager::ProcessManager) or
//! [`Spool`](super::spool::Spool) internals:
//!
//! - **status** — [`ProcessHandle::status`] returns `Running`, `Exited { code }`,
//!   or `Killed`;
//! - **kill** — [`ProcessHandle::kill`] signals the whole process group so a
//!   `server &`-style grandchild dies with the process, exactly as the bash
//!   timeout kill does; idempotent on an already-terminal process;
//! - **exit notification** — [`ProcessHandle::exit_receiver`] yields a watch
//!   receiver that flips `true` the moment the process becomes terminal;
//! - **subscription seam** — [`ProcessHandle::subscribe`] returns the spool's
//!   committed-length watch plus a fresh independent [`SpoolReader`], and
//!   [`ProcessHandle::exit_receiver`] the exit watch. This is the attach point
//!   NP-002's deterministic watches consume (INTERNAL-AGENTS §5); this brief
//!   designs it, NP-002 uses it. No watch/filter/matching logic lives here.
//!
//! Multiple subscribers per process are supported: each gets its own reader
//! and its own clone of the two watch receivers, so a watcher reading ahead
//! never consumes the model's unread region (the model's cursor is owned
//! separately by the tool layer).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::sync::watch;

use super::spool::{Spool, SpoolReader};

/// Lifecycle status of a managed process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The process is running. It leaves this state only by exiting or by an
    /// explicit kill — never by a timeout, turn limit, or any other bound.
    Running,
    /// The process exited on its own with this code (the sentinel
    /// [`SIGNAL_EXIT_CODE`](super::manager::SIGNAL_EXIT_CODE) when it died by
    /// signal without a code).
    Exited {
        /// Wait-status exit code.
        code: i32,
    },
    /// The process was killed (via [`ProcessHandle::kill`] or manager
    /// shutdown).
    Killed,
}

impl ProcessStatus {
    /// Whether this is a terminal status.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// The stable `snake_case` label for model-facing payloads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited { .. } => "exited",
            Self::Killed => "killed",
        }
    }
}

/// The completion facts of a terminated process, handed to the delivery sink.
///
/// Deliberately agent-agnostic: it carries no channel, router, or store type,
/// so the process module never depends on `crate::agent`. The concrete sink
/// (wired at assembly) turns this into an injected message.
#[derive(Clone, Debug)]
pub struct ProcessCompletion {
    /// Short process id, e.g. `"p1"`.
    pub process_label: String,
    /// The command line the process ran.
    pub command: String,
    /// Exit code when the process exited on its own; `None` when killed.
    pub exit_code: Option<i32>,
    /// Whether the process was killed rather than exiting on its own.
    pub killed: bool,
    /// When the process started.
    pub started_at: DateTime<Utc>,
    /// When the process became terminal.
    pub exited_at: DateTime<Utc>,
    /// The spool path, rendered for display.
    pub spool_path: String,
}

/// Shared per-process state behind an [`Arc`]. Not constructed directly by
/// consumers — [`ProcessManager`](super::manager::ProcessManager) builds it.
#[derive(Debug)]
pub struct ProcessShared {
    pub(super) label: String,
    pub(super) command: String,
    pub(super) pid: Option<u32>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) status: Mutex<ProcessStatus>,
    pub(super) exited_at: Mutex<Option<DateTime<Utc>>>,
    pub(super) spool: Arc<Spool>,
    pub(super) exit_tx: watch::Sender<bool>,
}

/// A cheap, cloneable handle to one managed process.
#[derive(Clone, Debug)]
pub struct ProcessHandle {
    shared: Arc<ProcessShared>,
}

impl ProcessHandle {
    pub(super) fn new(shared: Arc<ProcessShared>) -> Self {
        Self { shared }
    }

    /// The short process id, e.g. `"p1"`.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.shared.label
    }

    /// The command line the process ran.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.shared.command
    }

    /// The OS process id (and, on Unix, the process-group id), when known.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.shared.pid
    }

    /// When the process started.
    #[must_use]
    pub fn started_at(&self) -> DateTime<Utc> {
        self.shared.started_at
    }

    /// When the process became terminal, if it has.
    #[must_use]
    pub fn exited_at(&self) -> Option<DateTime<Utc>> {
        *self.shared.exited_at.lock()
    }

    /// The current lifecycle status.
    #[must_use]
    pub fn status(&self) -> ProcessStatus {
        *self.shared.status.lock()
    }

    /// Whether the process is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.shared.status.lock().is_terminal()
    }

    /// The backing spool.
    #[must_use]
    pub fn spool(&self) -> &Arc<Spool> {
        &self.shared.spool
    }

    /// The subscription seam (R8): the spool's committed-length watch plus a
    /// fresh independent [`SpoolReader`]. Call again for another independent
    /// subscriber — NP-002 attaches its watches here.
    #[must_use]
    pub fn subscribe(&self) -> (watch::Receiver<u64>, SpoolReader) {
        (
            self.shared.spool.subscribe_len(),
            SpoolReader::new(Arc::clone(&self.shared.spool)),
        )
    }

    /// A receiver over the exit-notification watch: it holds `false` while the
    /// process runs and flips to `true` when it becomes terminal. Usable
    /// alongside the append subscription (R8).
    #[must_use]
    pub fn exit_receiver(&self) -> watch::Receiver<bool> {
        self.shared.exit_tx.subscribe()
    }

    /// Build the completion facts, once the process is terminal.
    #[must_use]
    pub fn completion(&self) -> Option<ProcessCompletion> {
        let status = self.status();
        if !status.is_terminal() {
            return None;
        }
        Some(ProcessCompletion {
            process_label: self.shared.label.clone(),
            command: self.shared.command.clone(),
            exit_code: match status {
                ProcessStatus::Exited { code } => Some(code),
                ProcessStatus::Running | ProcessStatus::Killed => None,
            },
            killed: matches!(status, ProcessStatus::Killed),
            started_at: self.shared.started_at,
            exited_at: self.exited_at().unwrap_or_else(Utc::now),
            spool_path: self.shared.spool.display_path(),
        })
    }

    /// Record a self-exit with `code`. Only advances a still-running process;
    /// a process already `Killed` keeps that disposition (the kill won the
    /// race).
    pub(super) fn mark_exited(&self, code: i32) {
        let mut status = self.shared.status.lock();
        if status.is_terminal() {
            return;
        }
        *status = ProcessStatus::Exited { code };
        *self.shared.exited_at.lock() = Some(Utc::now());
        // `send_replace`, not `send`: a tokio watch `send` fails and stores
        // nothing when every receiver has been dropped, so a subscriber that
        // attaches *after* exit would read the stale initial `false` forever
        // and hang on `changed()`. `send_replace` stores the terminal value
        // unconditionally, so a late `exit_receiver()` borrows `true` at once.
        self.shared.exit_tx.send_replace(true);
    }

    /// Mark the process killed. Idempotent: a process that already exited on
    /// its own keeps that disposition. Returns the resulting terminal status.
    pub(super) fn mark_killed(&self) -> ProcessStatus {
        let mut status = self.shared.status.lock();
        if status.is_terminal() {
            return *status;
        }
        *status = ProcessStatus::Killed;
        *self.shared.exited_at.lock() = Some(Utc::now());
        // `send_replace` (not `send`): stores the terminal value unconditionally
        // so a subscriber attaching after the kill sees `true` — see
        // `mark_exited` for the watch-with-no-receivers rationale.
        self.shared.exit_tx.send_replace(true);
        ProcessStatus::Killed
    }

    /// Kill the process group and mark the process `Killed`. Idempotent on an
    /// already-terminal process (returns its terminal status without
    /// re-killing). Mirrors the bash timeout kill: on Unix the whole group is
    /// signalled directly with SIGKILL, so a
    /// `server &`-style grandchild sharing the group dies too.
    pub async fn kill(&self) -> ProcessStatus {
        if self.shared.status.lock().is_terminal() {
            return self.status();
        }
        // Mark first so the supervising task observes `Killed` (not a natural
        // exit) when the group death makes `child.wait()` return.
        let resulting = self.mark_killed();
        self.signal_group().await;
        resulting
    }

    /// Best-effort group kill without marking status. Used by manager
    /// shutdown, which marks the status itself.
    pub(super) async fn signal_group(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.shared.pid {
            let result =
                tokio::task::spawn_blocking(move || crate::util::kill_process_group(pid)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(
                        pid,
                        %error,
                        process = %self.shared.label,
                        "failed to signal managed process group",
                    );
                }
                Err(error) => tracing::warn!(
                    pid,
                    %error,
                    process = %self.shared.label,
                    "managed process-group signal task failed",
                ),
            }
        }
        #[cfg(not(unix))]
        tracing::warn!(
            process = %self.shared.label,
            "process-group kill is not available on this platform; a backgrounded \
             grandchild may survive (documented limitation)",
        );
    }

    /// Synchronous group kill for the drop/shutdown path (no async runtime is
    /// guaranteed there). Marks the process `Killed` and signals the group.
    pub(super) fn kill_blocking(&self) {
        if self.mark_killed() != ProcessStatus::Killed {
            // Already terminal on its own — leave it be.
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.shared.pid
            && let Err(error) = crate::util::kill_process_group(pid)
        {
            tracing::warn!(
                pid,
                %error,
                process = %self.shared.label,
                "failed to signal managed process group at shutdown",
            );
        }
        #[cfg(not(unix))]
        tracing::warn!(
            process = %self.shared.label,
            "blocking process-group kill is not available on this platform",
        );
    }
}
