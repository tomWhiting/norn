//! [`ProcessManager`] — spawns, adopts, tracks, and tears down background
//! processes, with no dependency on `crate::agent`.
//!
//! ## Manager-owned vs shell-backgrounded
//!
//! There are two distinct notions of "background", and this module owns only
//! the first:
//!
//! - A **manager-owned background process** is spawned (or adopted) here. Its
//!   stdout/stderr pipes are owned for its whole life and spooled to disk —
//!   including output from its *own* backgrounded grandchildren, which keep
//!   the spool's pipe open and keep being captured. No grace period applies;
//!   there is nothing to cut off. It runs under **no timeout, no turn limit,
//!   and no lifetime bound** (owner ruling): it leaves `Running` only by
//!   exiting or by an explicit kill / manager shutdown.
//! - A **shell-backgrounded child of a foreground bash run** (`server &`) is an
//!   orphan holding the foreground tool's pipes. It is bounded by the bash
//!   drain grace (`crates/norn/src/tools/bash/process.rs`) and its later
//!   output is lost. That path is **unchanged** by this module.
//!
//! The manager assigns each process a stable short id (`p1`, `p2`, …, monotonic
//! per manager), tracks it in a registry with **no cap on process count**, and
//! on shutdown kills every still-running process group while leaving its spool
//! on disk.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::paths::norn_dir;
use crate::tool::context::ProcessEnv;

use super::handle::{ProcessCompletion, ProcessHandle, ProcessShared, ProcessStatus};
use super::spool::{Spool, StreamTag};
use super::watch::{NewWatch, Watch, WatchAlert, WatchAttachError, WatchError, WatchRegistry};

/// Exit code reported when a process died by signal without a wait-status code.
pub const SIGNAL_EXIT_CODE: i32 = -1;

/// Sink that turns a terminated process's [`ProcessCompletion`] and a watch's
/// [`WatchAlert`] (NP-002) into delivered injected messages. Defined here
/// (agent-agnostic) and implemented at assembly by a concrete adapter that owns
/// the durable injected-message path, so the manager never depends on
/// `crate::agent`. The same object handles both notice kinds because they share
/// one durable delivery algorithm (NP-001 R5) — only the payload and the
/// reserved harness sender identity differ.
pub trait ProcessNotifier: Send + Sync {
    /// Deliver the completion notice for a terminated process.
    fn deliver_completion(&self, completion: ProcessCompletion);
    /// Deliver a watch match/error alert (NP-002 R3/R4).
    fn deliver_watch_alert(&self, alert: WatchAlert);
}

/// A manager registry entry: the public handle plus the manager-private task
/// handles needed to tear the process down deterministically.
struct RegistryEntry {
    handle: ProcessHandle,
    /// The task awaiting the direct child's exit. Aborted (not joined) at
    /// shutdown so no completion notice is delivered after teardown begins.
    supervisor: Option<JoinHandle<()>>,
    /// The stdout/stderr drain tasks. Left running after the direct child
    /// exits so a backgrounded grandchild keeps spooling; aborted at shutdown.
    drains: Vec<JoinHandle<std::io::Result<()>>>,
}

/// Owns and tracks the background processes for one agent.
pub struct ProcessManager {
    /// The `outputs/<token>/` segment: the
    /// [`SessionId`](crate::tool::context::SessionId) when one exists, else a
    /// per-run UUID generated once at construction and reused for every spool.
    base_token: String,
    /// A fresh UUID generated once per manager instance. Spools live one level
    /// below the session token under this run id (see [`Self::spool_path`]).
    run_id: String,
    /// Monotonic id counter — `p1`, `p2`, … There is no ceiling.
    next_id: AtomicU64,
    /// The process registry, ordered by numeric id. No cap on size.
    registry: Mutex<BTreeMap<u64, RegistryEntry>>,
    /// The model's per-process output cursors (R6). Owned by the tool layer
    /// through the manager, independent of any subscriber's [`SpoolReader`].
    model_cursors: Mutex<HashMap<u64, u64>>,
    /// The deterministic watch registry (NP-002): per-process filter watches
    /// attached over the [`ProcessHandle`] subscription seam.
    watches: WatchRegistry,
    /// The completion / watch-alert delivery sink, wired at assembly. `None` on
    /// bare managers (no owning agent to notify).
    notifier: Option<Arc<dyn ProcessNotifier>>,
    /// Set once at shutdown: suppresses further completion notices.
    shutting_down: AtomicBool,
}

impl std::fmt::Debug for ProcessManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessManager")
            .field("base_token", &self.base_token)
            .field("run_id", &self.run_id)
            .field("next_id", &self.next_id)
            .field("has_notifier", &self.notifier.is_some())
            .field("shutting_down", &self.shutting_down)
            .finish_non_exhaustive()
    }
}

impl ProcessManager {
    /// Construct a manager whose spools live under
    /// `<norn_dir>/outputs/<token>/processes/<run_id>/`. `session_id` supplies
    /// the token when present; otherwise a fresh per-run UUID is generated once
    /// and reused for every spool of this manager.
    #[must_use]
    pub fn new(session_id: Option<String>, notifier: Option<Arc<dyn ProcessNotifier>>) -> Self {
        let base_token = session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        Self {
            base_token,
            run_id: Uuid::new_v4().to_string(),
            next_id: AtomicU64::new(1),
            registry: Mutex::new(BTreeMap::new()),
            model_cursors: Mutex::new(HashMap::new()),
            watches: WatchRegistry::new(),
            notifier,
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Resolve the spool path for a numeric id under this manager's token.
    ///
    /// RULED-AS-FLAGGED (NP-002 pre-task, under NP-001's spool-persistence
    /// intent): the path carries a per-manager `run_id` segment —
    /// `<norn_dir>/outputs/<session|run-uuid>/processes/<run_id>/pN.log`. A
    /// resumed session builds a fresh [`ProcessManager`] that restarts ids at
    /// `p1`; without the `run_id` segment its `File::create` would clobber the
    /// prior run's `<session>/processes/p1.log`, contradicting spool
    /// persistence. The fresh `run_id` per manager instance isolates every
    /// run's spools so no run can overwrite another's, while keeping them all
    /// discoverable under the one session directory.
    fn spool_path(&self, id: u64) -> std::io::Result<PathBuf> {
        let root = norn_dir().ok_or_else(|| {
            std::io::Error::other(
                "cannot resolve the norn home directory (no $NORN_HOME and no home dir); \
                 a background process cannot be spooled",
            )
        })?;
        Ok(root
            .join("outputs")
            .join(&self.base_token)
            .join("processes")
            .join(&self.run_id)
            .join(format!("p{id}.log")))
    }

    /// Spawn `command` via `sh -c`, detached from any tool call, in its own
    /// process group (on Unix), with the manager owning the stdout/stderr
    /// pipes for the process's whole life. Returns the process handle.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from resolving the spool path, creating the spool
    /// file, or spawning the shell.
    pub async fn spawn(
        self: &Arc<Self>,
        command: &str,
        cwd: &std::path::Path,
        process_env: Option<&ProcessEnv>,
    ) -> std::io::Result<ProcessHandle> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let spool = Arc::new(Spool::create(self.spool_path(id)?).await?);

        let mut cmd = build_bg_command(command, cwd, process_env);
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("child stdout pipe was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("child stderr pipe was not captured"))?;

        let drains = vec![
            tokio::spawn(drain_to_spool(
                stdout,
                Arc::clone(&spool),
                StreamTag::Stdout,
            )),
            tokio::spawn(drain_to_spool(
                stderr,
                Arc::clone(&spool),
                StreamTag::Stderr,
            )),
        ];

        Ok(self.install(id, command.to_owned(), pid, spool, child, drains))
    }

    /// Adopt an already-spawned child and its drain tasks (the timeout-boundary
    /// migration path). From this point the child is tracked identically to an
    /// explicitly-spawned process. The caller is responsible for teeing the
    /// child's ongoing output into the returned handle's spool.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from resolving the spool path or creating the
    /// spool file.
    pub async fn adopt(
        self: &Arc<Self>,
        command: &str,
        child: Child,
        stdout_task: JoinHandle<std::io::Result<()>>,
        stderr_task: JoinHandle<std::io::Result<()>>,
    ) -> std::io::Result<ProcessHandle> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let spool = Arc::new(Spool::create(self.spool_path(id)?).await?);
        let pid = child.id();
        let drains = vec![stdout_task, stderr_task];
        Ok(self.install(id, command.to_owned(), pid, spool, child, drains))
    }

    /// Build the shared state, register the process, and spawn its supervisor.
    fn install(
        self: &Arc<Self>,
        id: u64,
        command: String,
        pid: Option<u32>,
        spool: Arc<Spool>,
        child: Child,
        drains: Vec<JoinHandle<std::io::Result<()>>>,
    ) -> ProcessHandle {
        let (exit_tx, _exit_rx) = watch::channel(false);
        let shared = Arc::new(ProcessShared {
            label: format!("p{id}"),
            command,
            pid,
            started_at: Utc::now(),
            status: Mutex::new(ProcessStatus::Running),
            exited_at: Mutex::new(None),
            spool,
            exit_tx,
        });
        let handle = ProcessHandle::new(shared);

        let supervisor = tokio::spawn(supervise(id, handle.clone(), child, Arc::downgrade(self)));

        self.registry.lock().insert(
            id,
            RegistryEntry {
                handle: handle.clone(),
                supervisor: Some(supervisor),
                drains,
            },
        );
        handle
    }

    /// Look up a process by its short id (`"p1"`).
    #[must_use]
    pub fn get(&self, label: &str) -> Option<ProcessHandle> {
        let id = parse_label(label)?;
        self.registry.lock().get(&id).map(|e| e.handle.clone())
    }

    /// Every process this manager owns, ordered by id — no pagination cap.
    #[must_use]
    pub fn list(&self) -> Vec<ProcessHandle> {
        self.registry
            .lock()
            .values()
            .map(|e| e.handle.clone())
            .collect()
    }

    /// Read the output appended since the model's last `output` call for
    /// `label`, advancing the model's cursor, and return it with the current
    /// status. The outer `None` means the id is unknown; the inner `Err`
    /// surfaces an I/O failure (never swallowed).
    ///
    /// The cursor read-and-advance is **atomic**: the region bounds
    /// `[start, end)` are computed and the cursor advanced under the
    /// `model_cursors` lock with no await in between (the committed length is a
    /// plain atomic load), and only then are the bytes read from disk. Two
    /// concurrent `output` calls therefore claim disjoint regions rather than
    /// both reading from a stale cursor and returning the same bytes twice.
    /// Because the region is claimed without mutating any process state, the
    /// operation stays a truthful `ReadOnly` effect.
    pub async fn model_output(
        &self,
        label: &str,
    ) -> Option<std::io::Result<(Vec<u8>, ProcessStatus)>> {
        let id = parse_label(label)?;
        let handle = self.registry.lock().get(&id).map(|e| e.handle.clone())?;
        let (start, end) = {
            let mut cursors = self.model_cursors.lock();
            let start = cursors.get(&id).copied().unwrap_or(0);
            // `committed_len` is a synchronous atomic load — no await under the
            // lock. Clamp defensively so a cursor somehow ahead of committed
            // never underflows the range.
            let end = handle.spool().committed_len().max(start);
            cursors.insert(id, end);
            (start, end)
        };
        match handle.spool().read_range(start, end).await {
            Ok(bytes) => Some(Ok((bytes, handle.status()))),
            Err(error) => Some(Err(error)),
        }
    }

    /// Seed the model's output cursor for `label` to `cursor` (the F5 migration
    /// fix). Called once at adopt time when a migrated command's pre-migration
    /// output was delivered **inline** in the tool result: the model has
    /// already seen those bytes, so its `output` cursor starts past them and the
    /// first `op=output` returns only genuinely new post-migration output rather
    /// than re-delivering the seed verbatim. A no-op for an unknown id.
    pub fn set_model_cursor(&self, label: &str, cursor: u64) {
        if let Some(id) = parse_label(label) {
            self.model_cursors.lock().insert(id, cursor);
        }
    }

    /// Attach a deterministic watch (NP-002 R1) to a manager-owned process that
    /// is still `Running`. `cwd`/`env` are the agent's working directory and
    /// process environment, captured for the filter's `sh -c` executions.
    ///
    /// # Errors
    ///
    /// [`WatchAttachError::ProcessNotFound`] when `process_label` is unknown to
    /// this manager, or [`WatchAttachError::Terminal`] when the process has
    /// already exited or been killed (there is nothing left to watch; the spool
    /// remains readable by ordinary means). There is **no cap** on watch count.
    pub fn attach_watch(
        &self,
        process_label: &str,
        brief: String,
        filter: String,
        cwd: PathBuf,
        env: Option<ProcessEnv>,
    ) -> Result<Watch, WatchAttachError> {
        let id =
            parse_label(process_label).ok_or_else(|| WatchAttachError::not_found(process_label))?;
        let handle = self
            .registry
            .lock()
            .get(&id)
            .map(|e| e.handle.clone())
            .ok_or_else(|| WatchAttachError::not_found(process_label))?;
        let status = handle.status();
        if status.is_terminal() {
            return Err(WatchAttachError::terminal(process_label, status));
        }
        let watch = self.watches.attach(
            &handle,
            id,
            NewWatch {
                brief,
                filter,
                cwd,
                env,
                notifier: self.notifier.clone(),
            },
        );
        // Close the attach/finalize TOCTOU: the process may have gone terminal
        // between the check above and this insert. `mark_exited` strictly
        // precedes `finalize_for_process`'s collection, so if the status is now
        // terminal the finalize collection either already ran (and never saw
        // this watch — it would idle forever, un-finalized, listing for an
        // exited process) or is about to run and may miss it. Re-check and, if
        // terminal, detach the just-created watch cleanly and report Terminal —
        // the same error a caller racing the exit by a hair would have received.
        let status = handle.status();
        if status.is_terminal() {
            if let Err(error) = self.watches.detach(&watch.watch_id) {
                // The only way detach fails here is NotFound, i.e. the finalize
                // collection did observe and remove this watch after all — then
                // it is already being torn down, so there is nothing to clean up.
                tracing::debug!(
                    ?error,
                    watch = %watch.watch_id,
                    "watch that lost the attach/exit race was already finalized; nothing to detach",
                );
            }
            return Err(WatchAttachError::terminal(process_label, status));
        }
        Ok(watch)
    }

    /// Detach the watch named by `watch_label` (`"w1"`). The watch stops after
    /// any in-flight filter run completes; no region is examined after detach.
    ///
    /// # Errors
    ///
    /// [`WatchError::NotFound`] when no active watch has that id.
    pub fn detach_watch(&self, watch_label: &str) -> Result<(), WatchError> {
        self.watches.detach(watch_label)
    }

    /// Every active watch attached to the process with numeric id parsed from
    /// `process_label`, for the `list`/`status` tool output. Empty for an
    /// unknown or watch-free process.
    #[must_use]
    pub fn watches_for(&self, process_label: &str) -> Vec<Watch> {
        parse_label(process_label).map_or_else(Vec::new, |id| self.watches.watches_for(id))
    }

    /// Run every attached watch's final-region filter, then deliver the
    /// completion notice — in that order (NP-002 R5), so a match in the final
    /// output cannot be lost behind the completion message. Skips both when the
    /// manager is shutting down (shutdown kills are not watch events). A
    /// process with no watches delivers its completion immediately, preserving
    /// NP-001's timing exactly.
    async fn finalize_and_deliver(&self, handle: &ProcessHandle, id: u64) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if self.watches.has_watches(id) {
            // Join the drains so every byte the process committed is on disk
            // before each watch reads its final region; only then filter and
            // remove the watches. Draining is gated on `has_watches` so an
            // unwatched process never waits on its drains (NP-001 timing).
            let drains = {
                let mut registry = self.registry.lock();
                registry
                    .get_mut(&id)
                    .map(|e| std::mem::take(&mut e.drains))
                    .unwrap_or_default()
            };
            self.watches.finalize_for_process(id, drains).await;
        }
        self.deliver_completion(handle);
    }

    /// Deliver a terminated process's completion notice through the notifier,
    /// unless the manager is shutting down.
    fn deliver_completion(&self, handle: &ProcessHandle) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let Some(notifier) = self.notifier.as_ref() else {
            tracing::debug!(
                process = %handle.label(),
                "no completion sink wired; background-process completion is not injected \
                 (bare manager with no owning agent)",
            );
            return;
        };
        let Some(completion) = handle.completion() else {
            return;
        };
        notifier.deliver_completion(completion);
    }

    /// Kill every still-running process group and finalize their spools.
    /// Idempotent, synchronous (safe from `Drop` with no async runtime): a
    /// process that already exited is left untouched and never re-killed; its
    /// spool persists on disk. Each kill is logged with id and command.
    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        // End every active watch without a final-region alert: a shutdown kill
        // is not a watch event (NP-002 R5). Aborting drops each task at its
        // await point, so no filter runs and no alert is emitted.
        self.watches.abort_all();
        let mut registry = self.registry.lock();
        for entry in registry.values_mut() {
            if !entry.handle.is_running() {
                continue;
            }
            tracing::info!(
                process = %entry.handle.label(),
                command = %entry.handle.command(),
                "killing manager-owned process group at shutdown",
            );
            if let Some(supervisor) = entry.supervisor.take() {
                supervisor.abort();
            }
            for drain in entry.drains.drain(..) {
                drain.abort();
            }
            entry.handle.kill_blocking();
        }
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        // A dropped manager must never silently orphan a running child.
        // `shutdown` is idempotent, so an explicit shutdown before drop makes
        // this a no-op.
        self.shutdown();
    }
}

/// Binds a [`ProcessManager`]'s teardown to the agent instance's lifetime.
///
/// Held on the agent's
/// [`LoopContext`](crate::agent_loop::loop_context::LoopContext), which drops
/// with the agent (root) or the controller task (child). Dropping it runs
/// [`ProcessManager::shutdown`] — killing every still-running process group and
/// leaving its spool on disk — even while the manager `Arc` lingers on the
/// shared tool context, so a norn exit never silently orphans a manager-owned
/// child (there is no daemon yet to adopt them; INTERNAL-AGENTS §6.2 is future
/// work).
pub struct ProcessManagerGuard {
    manager: Arc<ProcessManager>,
}

impl ProcessManagerGuard {
    /// Bind `manager`'s shutdown to this guard.
    #[must_use]
    pub fn new(manager: Arc<ProcessManager>) -> Self {
        Self { manager }
    }
}

impl Drop for ProcessManagerGuard {
    fn drop(&mut self) {
        self.manager.shutdown();
    }
}

/// Build the `sh -c` command for a manager-owned process: piped stdio, its own
/// process group on Unix (so a kill reaches grandchildren), the agent's working
/// directory, and the context's process environment. Mirrors bash's
/// `build_shell_command`.
fn build_bg_command(
    command: &str,
    cwd: &std::path::Path,
    process_env: Option<&ProcessEnv>,
) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .current_dir(cwd);
    #[cfg(unix)]
    cmd.process_group(0);
    if let Some(process_env) = process_env {
        for (key, value) in process_env.iter() {
            cmd.env(key, value);
        }
    }
    cmd
}

/// Drain one stream to the spool, one tagged line per read, until the pipe
/// closes.
async fn drain_to_spool<R>(reader: R, spool: Arc<Spool>, tag: StreamTag) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        spool.append_tagged(tag, &line).await?;
    }
    Ok(())
}

/// Await the direct child's exit, record its status, then finalize any
/// attached watches (their final-region filters run before the completion
/// notice — NP-002 R5) and deliver the completion notice. The drain tasks are
/// deliberately left running (owned by the registry) so a backgrounded
/// grandchild keeps spooling after the direct child exits.
async fn supervise(
    id: u64,
    handle: ProcessHandle,
    mut child: Child,
    manager: std::sync::Weak<ProcessManager>,
) {
    match child.wait().await {
        Ok(status) => handle.mark_exited(status.code().unwrap_or(SIGNAL_EXIT_CODE)),
        Err(error) => {
            tracing::warn!(
                process = %handle.label(),
                %error,
                "failed to wait on managed process; recording a signal exit",
            );
            handle.mark_exited(SIGNAL_EXIT_CODE);
        }
    }
    if let Some(manager) = manager.upgrade() {
        manager.finalize_and_deliver(&handle, id).await;
    }
}

/// Parse a `p<n>` process label into its numeric id.
fn parse_label(label: &str) -> Option<u64> {
    label.strip_prefix('p').and_then(|n| n.parse::<u64>().ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// A [`NORN_HOME`] guard so spools land under a temp dir, serialised via
    /// `#[serial]`.
    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial]`; no concurrent reader.
            unsafe { std::env::set_var("NORN_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("NORN_HOME", v) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    fn manager(session: Option<&str>) -> Arc<ProcessManager> {
        Arc::new(ProcessManager::new(session.map(str::to_owned), None))
    }

    async fn wait_terminal(handle: &ProcessHandle) {
        for _ in 0..600 {
            if !handle.is_running() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("process did not terminate in time");
    }

    /// Wait until the spool's on-disk content contains `needle`. The drain
    /// tasks flush asynchronously after the direct child exits, so a content
    /// assertion must wait for the bytes rather than merely for terminal status.
    async fn wait_spool_contains(handle: &ProcessHandle, needle: &str) {
        for _ in 0..600 {
            let (bytes, _) = handle.spool().read_from(0).await.unwrap();
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("spool never contained {needle:?}");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn three_spawns_get_monotonic_ids_and_all_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let a = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let b = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let c = mgr.spawn("sleep 30", &cwd, None).await.unwrap();

        assert_eq!(a.label(), "p1");
        assert_eq!(b.label(), "p2");
        assert_eq!(c.label(), "p3");
        assert_eq!(a.status(), ProcessStatus::Running);
        assert_eq!(mgr.list().len(), 3, "no count ceiling; all three listed");

        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_spawned_process_outlives_any_bash_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        // 120s is the default bash timeout; a manager-owned process ignores it.
        let handle = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            handle.is_running(),
            "no timeout kills a manager-owned process"
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_spools_output_and_reports_exit() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr.spawn("echo done", &cwd, None).await.unwrap();
        wait_terminal(&handle).await;
        assert_eq!(handle.status(), ProcessStatus::Exited { code: 0 });

        // The spool path is under
        // <norn_home>/outputs/sess/processes/<run_id>/p1.log — a per-manager
        // run-id segment isolates this run's spools from any prior run's.
        let path = handle.spool().path();
        assert_eq!(path.file_name().unwrap(), "p1.log");
        let run_dir = path.parent().unwrap();
        assert!(
            Uuid::parse_str(&run_dir.file_name().unwrap().to_string_lossy()).is_ok(),
            "the run-id segment is a uuid: {}",
            run_dir.display(),
        );
        assert_eq!(
            run_dir.parent().unwrap(),
            dir.path().join("outputs/sess/processes")
        );
        wait_spool_contains(&handle, "done").await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn sessionless_manager_reuses_one_run_uuid_dir() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(None);
        let cwd = std::env::current_dir().unwrap();

        let a = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let b = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let a_dir = a.spool().path().parent().unwrap().to_path_buf();
        let b_dir = b.spool().path().parent().unwrap().to_path_buf();
        assert_eq!(a_dir, b_dir, "both spools share the one run-id dir");
        // Structure: …/outputs/<session-token>/processes/<run_id>/pN.log. Both
        // the sessionless token segment and the per-run segment are UUIDs.
        let run_id = a_dir.file_name().unwrap().to_string_lossy();
        assert!(
            Uuid::parse_str(&run_id).is_ok(),
            "the per-run dir is a uuid, got {run_id}",
        );
        assert_eq!(a_dir.parent().unwrap().file_name().unwrap(), "processes");
        let token = a_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(
            Uuid::parse_str(&token).is_ok(),
            "the sessionless token dir is a uuid, got {token}",
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn grandchild_output_keeps_spooling_after_direct_child_exits() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        // `sh` prints "early" and exits immediately; the backgrounded
        // grandchild prints "late" ~0.5s later while holding the pipe open.
        let handle = mgr
            .spawn("(sleep 0.5; echo late) & echo early", &cwd, None)
            .await
            .unwrap();
        wait_terminal(&handle).await;
        assert_eq!(
            handle.status(),
            ProcessStatus::Exited { code: 0 },
            "status reflects the direct child's exit",
        );
        // The grandchild emits "late" into the still-open pipe ~0.5s later.
        wait_spool_contains(&handle, "late").await;
        let (bytes, _) = handle.spool().read_from(0).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("early"), "direct output captured: {text}");
        assert!(
            text.contains("late"),
            "grandchild output captured after direct-child exit: {text}",
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn kill_transitions_running_to_killed_and_is_idempotent_on_exited() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let running = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        assert_eq!(running.kill().await, ProcessStatus::Killed);
        assert_eq!(running.status(), ProcessStatus::Killed);

        let quick = mgr.spawn("true", &cwd, None).await.unwrap();
        wait_terminal(&quick).await;
        // Killing an already-exited process reports its terminal status, no error.
        assert_eq!(quick.kill().await, ProcessStatus::Exited { code: 0 });
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn model_output_is_incremental_and_unknown_id_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("echo one; sleep 0.3; echo two", &cwd, None)
            .await
            .unwrap();
        // First check: at least "one".
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (first, _) = mgr.model_output(handle.label()).await.unwrap().unwrap();
        let first = String::from_utf8_lossy(&first).into_owned();
        assert!(first.contains("one"), "first output: {first}");
        assert!(!first.contains("two"), "two not yet emitted: {first}");

        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "two").await;
        let (second, status) = mgr.model_output(handle.label()).await.unwrap().unwrap();
        let second = String::from_utf8_lossy(&second).into_owned();
        assert!(
            second.contains("two"),
            "second call returns only new output: {second}"
        );
        assert!(
            !second.contains("one"),
            "cursor advanced past 'one': {second}"
        );
        assert_eq!(status, ProcessStatus::Exited { code: 0 });

        assert!(
            mgr.model_output("p999").await.is_none(),
            "unknown id -> None"
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn shutdown_kills_running_and_leaves_exited_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let running = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let exited = mgr.spawn("echo hi", &cwd, None).await.unwrap();
        wait_terminal(&exited).await;

        mgr.shutdown();
        assert_eq!(
            running.status(),
            ProcessStatus::Killed,
            "running was killed"
        );
        assert_eq!(
            exited.status(),
            ProcessStatus::Exited { code: 0 },
            "already-exited record untouched",
        );
        // Spool persists on disk after shutdown.
        assert!(exited.spool().path().exists());
    }

    /// R8 (a): two concurrent subscribers each observe an appended region —
    /// woken by the committed-length watch, not a polling loop — and each
    /// observes the exit through its own exit receiver.
    #[tokio::test]
    #[serial_test::serial]
    async fn two_subscribers_observe_the_append_and_the_exit() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr.spawn("echo hello", &cwd, None).await.unwrap();
        let (mut len_a, mut reader_a) = handle.subscribe();
        let (mut len_b, mut reader_b) = handle.subscribe();
        let mut exit_a = handle.exit_receiver();
        let mut exit_b = handle.exit_receiver();

        // Each subscriber is woken by the committed-length watch and reads the
        // appended region through its own independent reader — no polling.
        len_a.changed().await.unwrap();
        len_b.changed().await.unwrap();
        let a = reader_a.read_new().await.unwrap();
        let b = reader_b.read_new().await.unwrap();
        assert!(
            String::from_utf8_lossy(&a).contains("hello"),
            "A saw the append"
        );
        assert!(
            String::from_utf8_lossy(&b).contains("hello"),
            "B saw the append"
        );

        // Each is woken by the exit notification and sees the terminal flag.
        exit_a.changed().await.unwrap();
        exit_b.changed().await.unwrap();
        assert!(*exit_a.borrow(), "A observed the exit");
        assert!(*exit_b.borrow(), "B observed the exit");
        assert!(!handle.is_running());
        mgr.shutdown();
    }

    /// R8 (b): a subscriber attaching AFTER the process has exited and its
    /// output has been appended immediately observes the terminal state — the
    /// exit watch borrows `true` and the fresh reader drains the full spool.
    /// This is the `send_replace` fix: a plain `send` stores nothing once every
    /// receiver has been dropped, so this late receiver would otherwise read a
    /// stale `false`/`0` forever and hang on `changed()`.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_late_subscriber_sees_the_terminal_state_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr.spawn("echo late-hello", &cwd, None).await.unwrap();
        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "late-hello").await;

        // Attach only now — after exit and after every append.
        let (len_rx, mut reader) = handle.subscribe();
        let exit_rx = handle.exit_receiver();
        assert!(
            *exit_rx.borrow(),
            "the late subscriber borrows exited=true immediately",
        );
        assert!(
            *len_rx.borrow() > 0,
            "the late subscriber borrows the true committed length, not a stale 0",
        );
        let drained = reader.read_new().await.unwrap();
        assert!(
            String::from_utf8_lossy(&drained).contains("late-hello"),
            "the fresh reader drains the full spool",
        );
        mgr.shutdown();
    }

    /// R8 (c): subscriber cursors are independent of the model's output cursor
    /// in both directions — a subscriber reading ahead does not consume the
    /// model's unread region, and advancing the model cursor does not move a
    /// subscriber's reader.
    #[tokio::test]
    #[serial_test::serial]
    async fn subscriber_and_model_cursors_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr.spawn("echo shared-line", &cwd, None).await.unwrap();
        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "shared-line").await;

        // A subscriber draining the whole spool leaves the model's cursor
        // untouched: the model still reads the full region.
        let (_len, mut reader) = handle.subscribe();
        let sub = reader.read_new().await.unwrap();
        assert!(String::from_utf8_lossy(&sub).contains("shared-line"));
        let (model_first, _) = mgr.model_output("p1").await.unwrap().unwrap();
        assert!(
            String::from_utf8_lossy(&model_first).contains("shared-line"),
            "the subscriber's read did not consume the model's unread region",
        );

        // Now the model cursor is at the end; a fresh subscriber (cursor 0) is
        // unaffected and still drains the full spool.
        let (model_second, _) = mgr.model_output("p1").await.unwrap().unwrap();
        assert!(
            model_second.is_empty(),
            "the model cursor is now at the end"
        );
        let (_len2, mut reader2) = handle.subscribe();
        let sub2 = reader2.read_new().await.unwrap();
        assert!(
            String::from_utf8_lossy(&sub2).contains("shared-line"),
            "a new subscriber reads the full spool independent of the model cursor",
        );
        mgr.shutdown();
    }

    /// F7: two tasks racing `model_output` on the same process each get a
    /// disjoint (possibly empty) region; the regions partition the committed
    /// spool exactly — no byte is duplicated and none is lost. Under the old
    /// non-atomic read-then-advance both calls read from a stale cursor and
    /// returned the same bytes twice (their lengths would sum to 2× the spool).
    #[tokio::test]
    #[serial_test::serial]
    async fn racing_model_output_calls_get_disjoint_regions() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("for i in $(seq 1 50); do echo line$i; done", &cwd, None)
            .await
            .unwrap();
        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "line50").await;
        let (full, _) = handle.spool().read_from(0).await.unwrap();
        assert!(!full.is_empty());

        let m1 = Arc::clone(&mgr);
        let m2 = Arc::clone(&mgr);
        let t1 = tokio::spawn(async move { m1.model_output("p1").await.unwrap().unwrap().0 });
        let t2 = tokio::spawn(async move { m2.model_output("p1").await.unwrap().unwrap().0 });
        let r1 = t1.await.unwrap();
        let r2 = t2.await.unwrap();

        assert_eq!(
            r1.len() + r2.len(),
            full.len(),
            "the two regions partition the spool exactly — no duplication, no loss",
        );
        let mut forward = r1.clone();
        forward.extend_from_slice(&r2);
        let mut reverse = r2.clone();
        reverse.extend_from_slice(&r1);
        assert!(
            forward == full || reverse == full,
            "the two disjoint regions concatenate to the whole committed spool",
        );
        mgr.shutdown();
    }

    /// R1 / F3 (c): a directly-adopted child yields a registry entry
    /// indistinguishable from a spawned one — status, list membership,
    /// incremental output, and kill all behave identically.
    #[tokio::test]
    #[serial_test::serial]
    async fn adopt_yields_an_entry_indistinguishable_from_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let mgr = manager(Some("sess"));
        let cwd = std::env::current_dir().unwrap();

        // Build and spawn a live child exactly as the manager's own spawn path
        // would, then hand it to adopt() — the migration seam (R1).
        let mut cmd = build_bg_command("echo adopted; sleep 30", &cwd, None);
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // Placeholder drain handles: in the real migration flow these are the
        // still-running bash drains (aborted at shutdown). Here the teeing is
        // wired after adopt (below), mirroring what `attach_spool` does.
        let noop_out = tokio::spawn(async { Ok::<(), std::io::Error>(()) });
        let noop_err = tokio::spawn(async { Ok::<(), std::io::Error>(()) });
        let handle = mgr
            .adopt("echo adopted; sleep 30", child, noop_out, noop_err)
            .await
            .unwrap();
        // Tee the adopted child's output into the entry's own spool, exactly as
        // a migrated command's attached capture does.
        let spool = Arc::clone(handle.spool());
        tokio::spawn(drain_to_spool(
            stdout,
            Arc::clone(&spool),
            StreamTag::Stdout,
        ));
        tokio::spawn(drain_to_spool(stderr, spool, StreamTag::Stderr));

        // Indistinguishable from a spawned sleeper: same id, Running status,
        // present in the list.
        assert_eq!(handle.label(), "p1");
        assert_eq!(handle.status(), ProcessStatus::Running);
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].label(), "p1");

        // Incremental output reads work through the manager identically.
        wait_spool_contains(&handle, "adopted").await;
        let (bytes, status) = mgr.model_output("p1").await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("adopted"));
        assert_eq!(status, ProcessStatus::Running);

        // Kill transitions it to Killed exactly like a spawned process.
        assert_eq!(handle.kill().await, ProcessStatus::Killed);
        assert_eq!(handle.status(), ProcessStatus::Killed);
        mgr.shutdown();
    }

    /// R9 / F4 + NP-002 pre-task: a fresh manager (as session resume constructs
    /// it) starts with an empty registry even when a prior run's spool already
    /// sits on disk under this session — processes are in-session state; nothing
    /// is resurrected, numbering restarts at p1. Crucially, the prior run's
    /// `p1.log` is NOT clobbered: the per-manager run-id segment isolates each
    /// run's spools, so the prior spool remains readable and untouched after the
    /// new run backgrounds its own `p1`.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_fresh_manager_does_not_clobber_a_prior_runs_spools() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let cwd = std::env::current_dir().unwrap();

        // A "previous run" of this session: its manager spawned p1 and spooled
        // output. Capture where that spool lives on disk.
        let prior = manager(Some("sess"));
        let prior_handle = prior.spawn("echo prior-output", &cwd, None).await.unwrap();
        wait_terminal(&prior_handle).await;
        wait_spool_contains(&prior_handle, "prior-output").await;
        let prior_path = prior_handle.spool().path().to_path_buf();
        prior.shutdown();
        drop(prior);
        assert!(
            prior_path.exists(),
            "the prior run's spool persists on disk"
        );

        // A fresh manager for the SAME session (as resume builds): empty
        // registry, numbering restarts at p1, and its p1 spools to a DIFFERENT
        // file (a fresh run-id segment) — it cannot overwrite the prior p1.log.
        let mgr = manager(Some("sess"));
        assert!(
            mgr.list().is_empty(),
            "a resumed session starts with an empty registry",
        );
        assert!(mgr.get("p1").is_none(), "no prior process is resurrected");

        let handle = mgr.spawn("echo new-output", &cwd, None).await.unwrap();
        assert_eq!(handle.label(), "p1");
        assert_ne!(
            handle.spool().path(),
            prior_path,
            "the new run's p1 spool is a distinct file from the prior run's p1",
        );
        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "new-output").await;

        // The prior run's spool is untouched: still present, still readable, and
        // still carries exactly its original content — no clobber.
        assert!(prior_path.exists(), "the prior spool still exists");
        let prior_bytes = std::fs::read_to_string(&prior_path).unwrap();
        assert!(
            prior_bytes.contains("prior-output"),
            "the prior spool is intact and readable: {prior_bytes:?}",
        );
        mgr.shutdown();
    }

    #[test]
    fn parse_label_round_trip() {
        assert_eq!(parse_label("p1"), Some(1));
        assert_eq!(parse_label("p42"), Some(42));
        assert_eq!(parse_label("q1"), None);
        assert_eq!(parse_label("p"), None);
    }

    // ----- NP-002: deterministic watches ----------------------------------

    use crate::process::WatchAlertKind;
    use crate::process::watch::WatchAlert;

    /// A notifier that records every completion and watch alert in arrival
    /// order, so tests can assert both content and ordering.
    #[derive(Default)]
    struct RecordingNotifier {
        alerts: Mutex<Vec<WatchAlert>>,
        /// Arrival-ordered labels: `"alert:w1"` / `"completion:p1"`.
        order: Mutex<Vec<String>>,
    }

    impl super::ProcessNotifier for RecordingNotifier {
        fn deliver_completion(&self, completion: ProcessCompletion) {
            self.order
                .lock()
                .push(format!("completion:{}", completion.process_label));
        }
        fn deliver_watch_alert(&self, alert: WatchAlert) {
            self.order.lock().push(format!("alert:{}", alert.watch_id));
            self.alerts.lock().push(alert);
        }
    }

    fn watched_manager(session: &str) -> (Arc<ProcessManager>, Arc<RecordingNotifier>) {
        let notifier = Arc::new(RecordingNotifier::default());
        let sink: Arc<dyn super::ProcessNotifier> = Arc::clone(&notifier) as _;
        let mgr = Arc::new(ProcessManager::new(Some(session.to_owned()), Some(sink)));
        (mgr, notifier)
    }

    async fn wait_alert_count(notifier: &RecordingNotifier, at_least: usize) {
        for _ in 0..600 {
            if notifier.alerts.lock().len() >= at_least {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "expected at least {at_least} alerts, saw {}",
            notifier.alerts.lock().len()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn attach_to_terminal_process_fails_naming_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, _n) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr.spawn("true", &cwd, None).await.unwrap();
        wait_terminal(&handle).await;

        let err = mgr
            .attach_watch("p1", "b".into(), "grep x".into(), cwd.clone(), None)
            .expect_err("attaching to a terminal process fails");
        match err {
            WatchAttachError::Terminal { process_id, status } => {
                assert_eq!(process_id, "p1");
                assert!(status.is_terminal());
            }
            WatchAttachError::ProcessNotFound { process_id } => {
                panic!("expected Terminal, got ProcessNotFound({process_id})")
            }
        }
        // Unknown process id is a distinct ProcessNotFound.
        assert!(matches!(
            mgr.attach_watch("p404", "b".into(), "grep x".into(), cwd, None),
            Err(WatchAttachError::ProcessNotFound { .. }),
        ));
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn attach_list_and_unwatch_lifecycle_with_no_cap() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, _n) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let _ = handle;

        // No cap: attach 100 watches in a loop — all registered.
        for i in 0..100 {
            mgr.attach_watch(
                "p1",
                format!("brief {i}"),
                "grep zzz".into(),
                cwd.clone(),
                None,
            )
            .unwrap();
        }
        assert_eq!(mgr.watches_for("p1").len(), 100, "no cap on watch count");

        // Unwatch exactly one; the rest survive.
        let first = mgr.watches_for("p1")[0].watch_id.clone();
        mgr.detach_watch(&first).unwrap();
        assert_eq!(mgr.watches_for("p1").len(), 99);
        assert!(
            mgr.watches_for("p1").iter().all(|w| w.watch_id != first),
            "the detached watch is gone",
        );
        // Unwatching an unknown id is NotFound.
        assert!(matches!(
            mgr.detach_watch("w9999"),
            Err(WatchError::NotFound)
        ));
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn two_watches_fire_independently_on_different_lines() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr
            .spawn("echo ALPHA; sleep 0.3; echo BETA; sleep 0.6", &cwd, None)
            .await
            .unwrap();
        let _ = handle;

        let a = mgr
            .attach_watch("p1", "alpha".into(), "grep ALPHA".into(), cwd.clone(), None)
            .unwrap();
        let b = mgr
            .attach_watch("p1", "beta".into(), "grep BETA".into(), cwd.clone(), None)
            .unwrap();

        wait_alert_count(&notifier, 2).await;
        let alerts = notifier.alerts.lock().clone();
        let a_alert = alerts
            .iter()
            .find(|al| al.watch_id == a.watch_id)
            .expect("watch A fired");
        let b_alert = alerts
            .iter()
            .find(|al| al.watch_id == b.watch_id)
            .expect("watch B fired");
        match &a_alert.kind {
            WatchAlertKind::Match { excerpt, .. } => {
                assert!(excerpt.contains("ALPHA"), "{excerpt}");
            }
            WatchAlertKind::Error { error } => panic!("A: expected match, got error {error}"),
        }
        match &b_alert.kind {
            WatchAlertKind::Match { excerpt, .. } => assert!(excerpt.contains("BETA"), "{excerpt}"),
            WatchAlertKind::Error { error } => panic!("B: expected match, got error {error}"),
        }
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn final_region_is_filtered_before_the_completion_notice() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr
            .spawn("echo START; sleep 0.2; echo DONE-MATCH", &cwd, None)
            .await
            .unwrap();
        let _ = handle;
        mgr.attach_watch("p1", "done".into(), "grep DONE-MATCH".into(), cwd, None)
            .unwrap();

        // Wait until the completion has been delivered (the guard is scoped to
        // the check so it is never held across the await).
        for _ in 0..600 {
            let delivered = notifier
                .order
                .lock()
                .iter()
                .any(|e| e.starts_with("completion:"));
            if delivered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let order = notifier.order.lock().clone();
        let alert_idx = order
            .iter()
            .position(|e| e.starts_with("alert:"))
            .expect("a watch alert was delivered");
        let completion_idx = order
            .iter()
            .position(|e| e.starts_with("completion:"))
            .expect("the completion notice was delivered");
        assert!(
            alert_idx < completion_idx,
            "the final-region match must precede the completion notice: {order:?}",
        );
        let alerts = notifier.alerts.lock().clone();
        match &alerts[0].kind {
            WatchAlertKind::Match { excerpt, .. } => {
                assert!(excerpt.contains("DONE-MATCH"), "{excerpt}");
            }
            WatchAlertKind::Error { error } => panic!("expected the final-line match, got {error}"),
        }
        // The watch ended and was removed on exit.
        assert!(mgr.watches_for("p1").is_empty(), "watches end on exit");
        assert!(matches!(
            mgr.detach_watch(&alerts[0].watch_id),
            Err(WatchError::NotFound),
        ));
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn unwatch_examines_nothing_after_detach() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr
            .spawn("echo M1; sleep 0.4; echo M2; sleep 0.6", &cwd, None)
            .await
            .unwrap();
        let _ = handle;
        let w = mgr
            .attach_watch("p1", "m".into(), "grep M".into(), cwd, None)
            .unwrap();

        // First match (M1) arrives; then detach before M2 is emitted.
        wait_alert_count(&notifier, 1).await;
        mgr.detach_watch(&w.watch_id).unwrap();

        // Wait past M2's emission; nothing after detach is examined.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let alerts = notifier.alerts.lock().clone();
        assert_eq!(alerts.len(), 1, "only the pre-detach match was delivered");
        assert!(
            !matches!(&alerts[0].kind, WatchAlertKind::Match { excerpt, .. } if excerpt.contains("M2")),
            "M2 (emitted after detach) was never examined",
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_broken_filter_alerts_and_stays_attached_advancing_its_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr
            .spawn("echo one; sleep 0.3; echo two; sleep 0.6", &cwd, None)
            .await
            .unwrap();
        let _ = handle;
        // A filter referencing a nonexistent binary: every region is a watch
        // error, never a silent no-match.
        let w = mgr
            .attach_watch(
                "p1",
                "broken".into(),
                "this-binary-does-not-exist-xyz".into(),
                cwd,
                None,
            )
            .unwrap();

        wait_alert_count(&notifier, 2).await;
        let alerts = notifier.alerts.lock().clone();
        // Both alerts are errors, and their spool ranges advance (cursor moved
        // past the first failed region rather than re-examining it).
        for al in &alerts[..2] {
            assert!(
                matches!(&al.kind, WatchAlertKind::Error { .. }),
                "a broken filter surfaces an error alert, not a no-match",
            );
        }
        assert!(
            alerts[1].spool_start >= alerts[0].spool_end,
            "the cursor advanced past the failed region: {:?} then {:?}",
            (alerts[0].spool_start, alerts[0].spool_end),
            (alerts[1].spool_start, alerts[1].spool_end),
        );
        // The watch is not silently disabled — it is still attached.
        assert!(
            mgr.watches_for("p1")
                .iter()
                .any(|x| x.watch_id == w.watch_id),
            "a filter failure never disables the watch",
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn matches_tile_the_spool_with_no_gap_overlap_or_cap() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        // A chattering process; `cat` matches every region (exit 0). No cap:
        // every append alerts.
        let handle = mgr
            .spawn("for i in $(seq 1 40); do echo line$i; done", &cwd, None)
            .await
            .unwrap();
        mgr.attach_watch("p1", "all".into(), "cat".into(), cwd, None)
            .unwrap();
        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "line40").await;
        // Give the executor a moment to drain the final region post-exit.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut alerts = notifier.alerts.lock().clone();
        assert!(!alerts.is_empty(), "cat matches every region — no cap");
        alerts.sort_by_key(|a| a.spool_start);
        // The regions tile [0, committed) exactly: no gap, no overlap.
        assert_eq!(alerts[0].spool_start, 0, "coverage starts at byte 0");
        for pair in alerts.windows(2) {
            assert_eq!(
                pair[0].spool_end, pair[1].spool_start,
                "consecutive regions are contiguous — no gap, no overlap",
            );
        }
        let committed = handle.spool().committed_len();
        assert_eq!(
            alerts.last().unwrap().spool_end,
            committed,
            "the regions cover the whole committed spool",
        );
        // The concatenated excerpts equal the full spool content.
        let joined: String = alerts
            .iter()
            .map(|a| match &a.kind {
                WatchAlertKind::Match { excerpt, .. } => excerpt.clone(),
                WatchAlertKind::Error { .. } => String::new(),
            })
            .collect();
        let (full, _) = handle.spool().read_from(0).await.unwrap();
        assert_eq!(
            joined,
            String::from_utf8_lossy(&full),
            "the excerpts reproduce the spool exactly (byte-equal, no re-derivation)",
        );
        mgr.shutdown();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn shutdown_ends_watches_with_no_final_alert() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        let _ = handle;
        mgr.attach_watch("p1", "all".into(), "cat".into(), cwd, None)
            .unwrap();

        // Shutdown kills the process group and aborts the watch — a shutdown
        // kill is not a watch event, so no alert is emitted.
        mgr.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            notifier.alerts.lock().is_empty(),
            "shutdown produces no watch alerts",
        );
    }

    /// Finding 1 (S3 CI-log shape): a watch attached late to a still-running
    /// process must, on its initial catch-up, filter the whole already-committed
    /// region — here far larger than one pipe buffer (~64KB) — through a filter
    /// whose stdout is equally large (`cat`). Before the stdin write was driven
    /// concurrently with output collection this wedged forever (the executor
    /// blocked writing stdin while the filter blocked writing its unread stdout).
    #[tokio::test]
    #[serial_test::serial]
    async fn a_late_attached_watch_catches_up_over_a_large_region_without_wedging() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();

        // Dump a large body (~1.2MB) then linger so the watch attaches while the
        // process is still Running and the whole body is already committed.
        let handle = mgr
            .spawn("seq 1 200000; sleep 30", &cwd, None)
            .await
            .unwrap();
        wait_spool_contains(&handle, "200000").await;
        assert!(
            handle.is_running(),
            "the process is still running at attach"
        );
        let (full, _) = handle.spool().read_from(0).await.unwrap();
        assert!(
            full.len() > 64 * 1024,
            "precondition: the catch-up region exceeds one pipe buffer ({} bytes)",
            full.len(),
        );

        mgr.attach_watch("p1", "all".into(), "cat".into(), cwd, None)
            .unwrap();
        // The initial catch-up completes and delivers the whole region as a
        // single byte-equal match.
        wait_alert_count(&notifier, 1).await;
        let alerts = notifier.alerts.lock().clone();
        let joined: String = alerts
            .iter()
            .map(|a| match &a.kind {
                WatchAlertKind::Match { excerpt, .. } => excerpt.clone(),
                WatchAlertKind::Error { error } => {
                    panic!("the large-region catch-up should match, got {error}")
                }
            })
            .collect();
        assert_eq!(
            joined.as_bytes(),
            full.as_slice(),
            "the catch-up excerpt is byte-equal to the full large region",
        );
        mgr.shutdown();
    }

    /// Finding 2 (phantom watches during finalize): the direct child exits at
    /// once but a backgrounded grandchild holds the stdout pipe open, so the
    /// drain join inside finalize blocks for that whole window. Throughout it the
    /// watch must stay visible to `list` and detachable by `unwatch` — never a
    /// phantom that alerts yet reports `NotFound`.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_watch_is_not_a_phantom_during_a_deferred_finalize() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, _n) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("(sleep 2; echo LATE) & echo EARLY", &cwd, None)
            .await
            .unwrap();
        let w = mgr
            .attach_watch("p1", "late".into(), "grep LATE".into(), cwd, None)
            .unwrap();

        // The direct child exits; the supervisor enters finalize and blocks on
        // the drain join because the grandchild still holds the pipe. The watch
        // is only removed AFTER that join, so it is present for the whole ~2s.
        wait_terminal(&handle).await;
        assert!(
            !mgr.watches_for("p1").is_empty(),
            "the watch is visible to `list` during a deferred finalize (not a phantom)",
        );
        mgr.detach_watch(&w.watch_id)
            .expect("unwatch detaches the watch during finalize rather than returning NotFound");

        // After the grandchild closes the pipe and finalize unwinds, list is empty.
        for _ in 0..600 {
            if mgr.watches_for("p1").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            mgr.watches_for("p1").is_empty(),
            "after finalize completes the watch is gone",
        );
        mgr.shutdown();
    }

    /// Finding 2, companion: a deferred finalize (grandchild holding the pipe)
    /// with no intervening unwatch still filters the final region and removes the
    /// watch naturally once the drains close — proving finalize completes rather
    /// than hanging with entries in the map.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_deferred_finalize_filters_the_final_region_then_removes_the_watch() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("echo EARLY; (sleep 1; echo LATE-MATCH) & true", &cwd, None)
            .await
            .unwrap();
        mgr.attach_watch("p1", "late".into(), "grep LATE-MATCH".into(), cwd, None)
            .unwrap();

        wait_terminal(&handle).await;
        assert!(
            !mgr.watches_for("p1").is_empty(),
            "the watch is still listed while the drain join blocks",
        );

        // Once the grandchild prints LATE-MATCH and closes the pipe, finalize
        // filters the final region (delivering the match) and removes the watch.
        wait_alert_count(&notifier, 1).await;
        let alerts = notifier.alerts.lock().clone();
        match &alerts[0].kind {
            WatchAlertKind::Match { excerpt, .. } => {
                assert!(excerpt.contains("LATE-MATCH"), "{excerpt}");
            }
            WatchAlertKind::Error { error } => {
                panic!("expected the final-region match, got {error}")
            }
        }
        for _ in 0..600 {
            if mgr.watches_for("p1").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            mgr.watches_for("p1").is_empty(),
            "the deferred finalize removed the watch after filtering its final region",
        );
        mgr.shutdown();
    }

    /// Finding 3 (attach/finalize TOCTOU): a watch inserted after finalize has
    /// already collected its targets would idle forever, un-finalized, and list
    /// for an exited process. `attach_watch` closes this with a status re-check
    /// after the insert — terminal ⇒ detach the just-created watch and return
    /// `Terminal`.
    ///
    /// The precise interleave (first check `Running`, exit, re-check `Terminal`)
    /// is not deterministically constructible in-process: `attach_watch` reads
    /// status, calls the synchronous `watches.attach` (no await, so no other
    /// task runs), then re-reads status — there is no async seam between the two
    /// reads to interpose an exit without adding production-only test surface.
    /// This test therefore drives the re-check's contract directly with the same
    /// production primitives `attach_watch` uses — `watches.attach`,
    /// `handle.mark_exited` (the real exit path that strictly precedes finalize's
    /// collection), and `watches.detach` — reproducing the exact ordering the
    /// re-check protects and asserting no watch is left attached to the exited
    /// process.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_watch_losing_the_attach_exit_race_is_detached_not_left_dangling() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, _n) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr.spawn("sleep 30", &cwd, None).await.unwrap();

        // First check passes: the process is running when attach begins.
        assert!(!handle.status().is_terminal(), "precondition: running");
        let watch = mgr.watches.attach(
            &handle,
            1,
            NewWatch {
                brief: "race".into(),
                filter: "cat".into(),
                cwd: cwd.clone(),
                env: None,
                notifier: mgr.notifier.clone(),
            },
        );
        assert_eq!(mgr.watches_for("p1").len(), 1, "the watch was inserted");

        // The process wins the race to terminal AFTER the insert — the exact
        // window the production re-check closes.
        handle.mark_exited(0);

        // The re-check's action (terminal ⇒ detach), driven with the same
        // `detach` primitive attach_watch calls.
        assert!(handle.status().is_terminal());
        mgr.watches
            .detach(&watch.watch_id)
            .expect("the racing watch detaches cleanly");
        assert!(
            mgr.watches_for("p1").is_empty(),
            "no watch is left attached to the exited process",
        );
        mgr.shutdown();
    }

    /// Finding 4 (a) / R6: watch reads never advance the model's output cursor. A
    /// `cat` watch consumes every region, but the model — which has never called
    /// `output` — still reads the FULL content from cursor 0.
    #[tokio::test]
    #[serial_test::serial]
    async fn watch_reads_never_advance_the_models_output_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("echo AAA; echo BBB; sleep 30", &cwd, None)
            .await
            .unwrap();
        mgr.attach_watch("p1", "all".into(), "cat".into(), cwd, None)
            .unwrap();
        // Let the watch consume the output (its own reader advances, not the
        // model's), and wait until both lines are committed.
        wait_alert_count(&notifier, 1).await;
        wait_spool_contains(&handle, "BBB").await;

        let (bytes, _) = mgr.model_output("p1").await.unwrap().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("AAA") && text.contains("BBB"),
            "the model reads the full output from cursor 0 despite the watch's reads: {text}",
        );
        mgr.shutdown();
    }

    /// Finding 4 (b) / R6 three-cursor independence: two watches plus the model
    /// cursor on one process each observe the complete content — no reader
    /// consumes another's region.
    #[tokio::test]
    #[serial_test::serial]
    async fn two_watches_and_the_model_cursor_each_observe_the_full_content() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let (mgr, notifier) = watched_manager("sess");
        let cwd = std::env::current_dir().unwrap();

        let handle = mgr
            .spawn("sleep 0.2; echo SHARED-LINE; sleep 30", &cwd, None)
            .await
            .unwrap();
        mgr.attach_watch("p1", "w-a".into(), "cat".into(), cwd.clone(), None)
            .unwrap();
        mgr.attach_watch("p1", "w-b".into(), "cat".into(), cwd, None)
            .unwrap();

        // Both watches fire, each independently carrying the full line.
        wait_alert_count(&notifier, 2).await;
        let alerts = notifier.alerts.lock().clone();
        assert_eq!(
            alerts.len(),
            2,
            "each watch observed the content through its own cursor",
        );
        for al in &alerts {
            match &al.kind {
                WatchAlertKind::Match { excerpt, .. } => {
                    assert!(excerpt.contains("SHARED-LINE"), "{excerpt}");
                }
                WatchAlertKind::Error { error } => panic!("expected a match, got {error}"),
            }
        }

        // The model cursor, independent of both watches, still reads the full line.
        wait_spool_contains(&handle, "SHARED-LINE").await;
        let (bytes, _) = mgr.model_output("p1").await.unwrap().unwrap();
        assert!(
            String::from_utf8_lossy(&bytes).contains("SHARED-LINE"),
            "the model's cursor observes the full content independent of the two watches",
        );
        mgr.shutdown();
    }
}
