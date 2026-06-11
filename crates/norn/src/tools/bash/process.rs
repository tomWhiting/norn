//! Subprocess lifecycle for the bash tool: process-group spawning,
//! timeout enforcement that kills the whole process tree, and bounded
//! draining of the child's output streams.
//!
//! On Unix the shell is spawned into its **own process group**
//! (`process_group(0)`, a safe std/tokio API), so a timeout can kill
//! the shell *and* everything it spawned by signalling the group. On
//! non-Unix targets there is no process-group concept to target, so the
//! timeout falls back to killing only the direct child; grandchildren
//! may survive (documented limitation).
//!
//! The stdout/stderr drain tasks normally finish as soon as the shell
//! exits and the pipes close. A command that *backgrounds* a child
//! (`server &`) leaves that child holding the pipe open, which used to
//! block the tool until the grandchild exited. Draining is therefore
//! bounded by a grace period after process exit (configurable per
//! [`super::BashTool`], defaulting to [`DEFAULT_DRAIN_GRACE`]): on
//! expiry the drains are aborted, whatever was buffered is returned,
//! and the result is annotated with `streams_still_open` so the caller
//! knows output may be incomplete.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::error::ToolError;
use crate::tool::context::{ProcessEnv, ToolContext};

use super::output::{CapturedOutput, OutputCapture, drain_stderr, drain_stdout};

/// Sentinel exit code reported when a process was killed by signal or timeout.
pub(super) const SIGNAL_KILLED_EXIT_CODE: i32 = -1;

/// Default grace period granted to the stdout/stderr drain tasks *after*
/// the shell process has exited (or been killed). Owner-approved default;
/// override per tool via [`super::BashTool::with_drain_grace`].
///
/// At this point the shell itself can produce no more output; the only
/// thing that can keep a pipe open is an orphaned background child the
/// command deliberately left running. Two seconds is ample for the OS
/// pipe buffers of an exited process to flush, while keeping a
/// `server &`-style command from stalling the agent until the
/// grandchild exits.
pub(super) const DEFAULT_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Outcome of one shell execution.
pub(super) struct ShellExecution {
    /// Child exit code, or [`SIGNAL_KILLED_EXIT_CODE`] when killed by signal.
    pub(super) exit_code: i32,
    /// Whether the command was killed because it exceeded its timeout.
    pub(super) timed_out: bool,
    /// Whether stdout/stderr were still open when draining was cut off —
    /// a background child is holding the pipe and output may be incomplete.
    pub(super) streams_still_open: bool,
    /// Captured (possibly disk-redirected) stdout/stderr.
    pub(super) captured: CapturedOutput,
}

/// Runs `command` via `sh -c` in `cwd`, streaming output into `capture`.
///
/// `timeout_secs == 0` means wait forever. On timeout the entire
/// process tree is killed (see module docs) and `timed_out` is set.
/// `drain_grace` bounds how long the output drains may stay open after
/// the shell exits (see [`DEFAULT_DRAIN_GRACE`]).
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when the shell cannot be
/// spawned/waited on, a drain task panics, or output capture fails.
pub(super) async fn run_shell(
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
    drain_grace: Duration,
    ctx: &ToolContext,
    capture: Arc<OutputCapture>,
) -> Result<ShellExecution, ToolError> {
    let mut cmd = build_shell_command(command, cwd, ctx);
    let mut child = cmd.spawn().map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to spawn `sh`: {e}"),
    })?;

    let stdout_handle = child.stdout.take().ok_or(ToolError::ExecutionFailed {
        reason: "child stdout pipe was not captured".to_owned(),
    })?;
    let stderr_handle = child.stderr.take().ok_or(ToolError::ExecutionFailed {
        reason: "child stderr pipe was not captured".to_owned(),
    })?;

    let stdout_task = tokio::spawn(drain_stdout(stdout_handle, Arc::clone(&capture)));
    let stderr_task = tokio::spawn(drain_stderr(stderr_handle, Arc::clone(&capture)));

    let (status, timed_out) = if timeout_secs == 0 {
        let status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
            reason: format!("failed to wait on child: {e}"),
        })?;
        (status, false)
    } else {
        tokio::select! {
            wait_result = child.wait() => {
                let status = wait_result.map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("failed to wait on child: {e}"),
                })?;
                (status, false)
            }
            () = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                kill_process_tree(&mut child).await;
                let status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("failed to wait on killed child: {e}"),
                })?;
                (status, true)
            }
        }
    };

    let stdout_open = settle_drain("stdout", stdout_task, drain_grace).await?;
    let stderr_open = settle_drain("stderr", stderr_task, drain_grace).await?;
    let captured = capture.finalize().await?;

    Ok(ShellExecution {
        exit_code: status.code().unwrap_or(SIGNAL_KILLED_EXIT_CODE),
        timed_out,
        streams_still_open: stdout_open || stderr_open,
        captured,
    })
}

/// Builds the `sh -c` command: piped stdio, the agent's working
/// directory, the context's process environment, and — on Unix — its
/// own process group so a timeout can kill the whole tree.
fn build_shell_command(command: &str, cwd: &Path, ctx: &ToolContext) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .current_dir(cwd);
    // Own process group: the child's PID becomes the process-group ID,
    // so `kill_process_tree` can signal `-pid` to reach every
    // descendant that has not detached into a new group. Safe API — no
    // `unsafe` pre_exec needed.
    #[cfg(unix)]
    cmd.process_group(0);
    if let Some(process_env) = ctx.get_extension::<ProcessEnv>() {
        for (key, value) in &process_env.0 {
            cmd.env(key, value);
        }
    }
    cmd
}

/// Kills a timed-out shell and (on Unix) its entire process group.
///
/// The group is signalled with SIGKILL via the external `kill` utility
/// (`kill -9 -- -<pgid>`), which keeps this crate free of `unsafe` libc
/// calls; the direct child is additionally killed through the tokio
/// handle as a fallback. Descendants that moved themselves into a new
/// process group (`setsid`) escape the sweep — an accepted limitation.
///
/// On non-Unix targets only the direct child is killed.
async fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let group_kill = Command::new("kill")
            .arg("-9")
            .arg("--")
            .arg(format!("-{pid}"))
            .status()
            .await;
        match group_kill {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(
                pid,
                code = ?status.code(),
                "kill -9 on timed-out bash process group returned non-zero",
            ),
            Err(e) => tracing::warn!(
                pid,
                error = %e,
                "failed to run `kill` for timed-out bash process group",
            ),
        }
    }
    // Fallback / non-Unix path: kill the direct child through tokio.
    if let Err(e) = child.start_kill() {
        tracing::warn!(error = %e, "failed to send kill signal to timed-out bash child");
    }
}

/// Awaits a drain task, bounded by `grace` (the per-tool drain grace
/// period, [`DEFAULT_DRAIN_GRACE`] unless overridden).
///
/// Returns `Ok(true)` when the stream was still open at expiry (the
/// drain was aborted; buffered output is preserved in the capture) and
/// `Ok(false)` when the stream closed normally.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when the drain task panicked
/// or its underlying read failed.
async fn settle_drain(
    stream: &str,
    task: JoinHandle<std::io::Result<()>>,
    grace: Duration,
) -> Result<bool, ToolError> {
    let mut task = task;
    match tokio::time::timeout(grace, &mut task).await {
        Ok(join_result) => {
            join_result
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("{stream} drain task failed: {e}"),
                })?
                .map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("{stream} read failed: {e}"),
                })?;
            Ok(false)
        }
        Err(_expired) => {
            // A writer (an orphaned background child inheriting the pipe)
            // is keeping the stream open. Abort the drain and surface what
            // was buffered; the await below ensures the task has actually
            // stopped before the capture is finalized.
            task.abort();
            match (&mut task).await {
                // Completed in the abort race: the stream closed after all.
                Ok(Ok(())) => Ok(false),
                Ok(Err(e)) => Err(ToolError::ExecutionFailed {
                    reason: format!("{stream} read failed: {e}"),
                }),
                Err(join_err) if join_err.is_cancelled() => {
                    tracing::warn!(
                        stream,
                        grace_ms = grace.as_millis(),
                        "bash output stream still open after process exit; \
                         a background child is holding the pipe — returning buffered output",
                    );
                    Ok(true)
                }
                Err(join_err) => Err(ToolError::ExecutionFailed {
                    reason: format!("{stream} drain task failed: {join_err}"),
                }),
            }
        }
    }
}
