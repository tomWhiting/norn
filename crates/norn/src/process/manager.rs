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

/// Exit code reported when a process died by signal without a wait-status code.
pub const SIGNAL_EXIT_CODE: i32 = -1;

/// Sink that turns a terminated process's [`ProcessCompletion`] into a
/// delivered notice. Defined here (agent-agnostic) and implemented at assembly
/// by a concrete adapter that owns the durable injected-message path, so the
/// manager never depends on `crate::agent`.
pub trait ProcessCompletionSink: Send + Sync {
    /// Deliver the completion notice for a terminated process.
    fn deliver(&self, completion: ProcessCompletion);
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
    /// Monotonic id counter — `p1`, `p2`, … There is no ceiling.
    next_id: AtomicU64,
    /// The process registry, ordered by numeric id. No cap on size.
    registry: Mutex<BTreeMap<u64, RegistryEntry>>,
    /// The model's per-process output cursors (R6). Owned by the tool layer
    /// through the manager, independent of any subscriber's [`SpoolReader`].
    model_cursors: Mutex<HashMap<u64, u64>>,
    /// The completion-delivery sink, wired at assembly. `None` on bare
    /// managers (no owning agent to notify).
    sink: Option<Arc<dyn ProcessCompletionSink>>,
    /// Set once at shutdown: suppresses further completion notices.
    shutting_down: AtomicBool,
}

impl std::fmt::Debug for ProcessManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessManager")
            .field("base_token", &self.base_token)
            .field("next_id", &self.next_id)
            .field("has_sink", &self.sink.is_some())
            .field("shutting_down", &self.shutting_down)
            .finish_non_exhaustive()
    }
}

impl ProcessManager {
    /// Construct a manager whose spools live under
    /// `<norn_dir>/outputs/<token>/processes/`. `session_id` supplies the
    /// token when present; otherwise a fresh per-run UUID is generated once and
    /// reused for every spool of this manager.
    #[must_use]
    pub fn new(session_id: Option<String>, sink: Option<Arc<dyn ProcessCompletionSink>>) -> Self {
        let base_token = session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        Self {
            base_token,
            next_id: AtomicU64::new(1),
            registry: Mutex::new(BTreeMap::new()),
            model_cursors: Mutex::new(HashMap::new()),
            sink,
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Resolve the spool path for a numeric id under this manager's token.
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

        let supervisor = tokio::spawn(supervise(handle.clone(), child, Arc::downgrade(self)));

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

    /// Deliver a terminated process's completion notice through the sink,
    /// unless the manager is shutting down.
    fn deliver_completion(&self, handle: &ProcessHandle) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let Some(sink) = self.sink.as_ref() else {
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
        sink.deliver(completion);
    }

    /// Kill every still-running process group and finalize their spools.
    /// Idempotent, synchronous (safe from `Drop` with no async runtime): a
    /// process that already exited is left untouched and never re-killed; its
    /// spool persists on disk. Each kill is logged with id and command.
    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
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

/// Await the direct child's exit, record its status, and deliver the
/// completion notice. The drain tasks are deliberately left running (owned by
/// the registry) so a backgrounded grandchild keeps spooling after the direct
/// child exits.
async fn supervise(
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
        manager.deliver_completion(&handle);
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

        // The spool path is under <norn_home>/outputs/sess/processes/p1.log.
        let expected = dir.path().join("outputs/sess/processes/p1.log");
        assert_eq!(handle.spool().path(), expected);
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
        assert_eq!(a_dir, b_dir, "both spools share the one run-uuid dir");
        // The token segment (…/outputs/<token>/processes/) is a generated UUID.
        let token = a_dir
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

    /// R9 / F4: a fresh manager (as session resume constructs it) starts with an
    /// empty registry even when spool files from a "previous run" already sit on
    /// disk — processes are in-session state; nothing is resurrected. Numbering
    /// restarts at p1.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_fresh_manager_ignores_prior_spool_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        // Simulate a previous run: spool files already exist under this
        // session's processes directory.
        let processes_dir = dir.path().join("outputs/sess/processes");
        std::fs::create_dir_all(&processes_dir).unwrap();
        std::fs::write(processes_dir.join("p1.log"), b"out stale\n").unwrap();
        std::fs::write(processes_dir.join("p2.log"), b"out stale-two\n").unwrap();

        let mgr = manager(Some("sess"));
        assert!(
            mgr.list().is_empty(),
            "a resumed session starts with an empty registry",
        );
        assert!(mgr.get("p1").is_none(), "no prior process is resurrected");

        // The next spawn numbers fresh from p1 (monotonic per manager, not
        // continued from the on-disk files).
        let cwd = std::env::current_dir().unwrap();
        let handle = mgr.spawn("sleep 30", &cwd, None).await.unwrap();
        assert_eq!(handle.label(), "p1");
        mgr.shutdown();
    }

    #[test]
    fn parse_label_round_trip() {
        assert_eq!(parse_label("p1"), Some(1));
        assert_eq!(parse_label("p42"), Some(42));
        assert_eq!(parse_label("q1"), None);
        assert_eq!(parse_label("p"), None);
    }
}
