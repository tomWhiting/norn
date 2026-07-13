//! Subprocess lifecycle for the bash tool: process-group spawning,
//! timeout-boundary migration to the background manager, and bounded
//! draining of the child's output streams.
//!
//! ## Shell-backgrounded child vs manager-owned process
//!
//! Two distinct notions of "background" meet here; keep them separate:
//!
//! - A **shell-backgrounded child of a foreground run** (`server &`) is an
//!   orphan holding *this* tool's pipes after the shell exits. Draining is
//!   bounded by the grace period below; its later output is lost and the
//!   result is annotated `streams_still_open`. This behaviour is unchanged.
//! - A **manager-owned background process**
//!   ([`crate::process::ProcessManager`]) — created by `run_in_background` or
//!   by timeout migration — has its pipes owned and spooled for its whole
//!   life, including its own backgrounded grandchildren's output. No grace
//!   period applies; there is nothing to cut off.
//!
//! When a foreground command reaches its timeout it is **migrated** to the
//! manager (its live child and drain tasks handed off, R4), never killed —
//! slow-but-healthy work is never lost to an arbitrary cutoff. The only
//! timeout-kill left is the degenerate path where no manager is wired.
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
use crate::resource::{DescriptorGovernor, TWO_PIPE_SPAWN_PEAK};
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
    /// Whether the command was killed because it exceeded its timeout. Only
    /// ever `true` on the degenerate no-manager path (migration replaces the
    /// timeout kill when a manager is available).
    pub(super) timed_out: bool,
    /// Whether stdout/stderr were still open when draining was cut off —
    /// a background child is holding the pipe and output may be incomplete.
    pub(super) streams_still_open: bool,
    /// Captured (possibly disk-redirected) stdout/stderr.
    pub(super) captured: CapturedOutput,
}

/// The result of running a foreground shell command: either it completed (on
/// its own, or was killed at its timeout when no manager is wired), or it
/// reached its timeout and is being migrated to the background manager.
pub(super) enum ShellOutcome {
    /// The command finished; build the normal result.
    Completed(ShellExecution),
    /// The command reached its timeout and is handed off for migration.
    Migrated(crate::process::ProcessHandoff),
}

/// Runs `command` via `sh -c` in `cwd`, streaming output into `capture`.
///
/// `timeout_secs == 0` means wait forever (never migrates). When
/// `migrate_on_timeout` is `true` and the command reaches its timeout, the
/// live child and its still-running drains are handed back as
/// [`ShellOutcome::Migrated`] (the caller passes them to the manager) — the
/// process is **not** killed and the drains are **not** settled. When
/// `migrate_on_timeout` is `false` (no manager wired) a timeout falls back to
/// killing the whole process tree and returns a completed result with
/// `timed_out` set.
///
/// `drain_grace` bounds how long the output drains may stay open after the
/// shell exits on the completed path (see [`DEFAULT_DRAIN_GRACE`]); it does not
/// apply to a migrated process (the manager owns its pipes with no grace).
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
    migrate_on_timeout: bool,
    ctx: &ToolContext,
    capture: Arc<OutputCapture>,
) -> Result<ShellOutcome, ToolError> {
    let governor = DescriptorGovernor::global()
        .map_err(|error| ToolError::DescriptorAdmission(Box::new(error)))?;
    let mut launch_permits = governor
        .try_acquire(TWO_PIPE_SPAWN_PEAK)
        .map_err(|error| ToolError::DescriptorAdmission(Box::new(error)))?;
    let stdout_permit = launch_permits
        .split(1)
        .ok_or_else(|| ToolError::ExecutionFailed {
            reason: "shell launch admission did not contain a stdout permit".to_owned(),
        })?;
    let stderr_permit = launch_permits
        .split(1)
        .ok_or_else(|| ToolError::ExecutionFailed {
            reason: "shell launch admission did not contain a stderr permit".to_owned(),
        })?;
    capture.install_auxiliary_permit(launch_permits).await;
    let mut cmd = build_shell_command(command, cwd, ctx);
    let mut child = cmd
        .spawn()
        .map_err(|error| map_shell_io_error(&error, "spawning a foreground shell"))?;

    let stdout_handle = child.stdout.take().ok_or(ToolError::ExecutionFailed {
        reason: "child stdout pipe was not captured".to_owned(),
    })?;
    let stderr_handle = child.stderr.take().ok_or(ToolError::ExecutionFailed {
        reason: "child stderr pipe was not captured".to_owned(),
    })?;

    let stdout_task = tokio::spawn(drain_stdout(
        stdout_handle,
        Arc::clone(&capture),
        stdout_permit,
    ));
    let stderr_task = tokio::spawn(drain_stderr(
        stderr_handle,
        Arc::clone(&capture),
        stderr_permit,
    ));
    let mut handoff = crate::process::ProcessHandoff::new(child, stdout_task, stderr_task);

    let (status, timed_out) = if timeout_secs == 0 {
        let status = handoff
            .child_mut()
            .map_err(ToolError::from)?
            .wait()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to wait on child: {e}"),
            })?;
        (status, false)
    } else {
        tokio::select! {
            wait_result = handoff.child_mut().map_err(ToolError::from)?.wait() => {
                let status = wait_result.map_err(|e| ToolError::ExecutionFailed {
                    reason: format!("failed to wait on child: {e}"),
                })?;
                (status, false)
            }
            () = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                if migrate_on_timeout {
                    // Hand the live child and its still-running drains to the
                    // manager: no kill, no settle, no finalize — the process
                    // keeps running and its output keeps flowing.
                    return Ok(ShellOutcome::Migrated(handoff));
                }
                kill_process_tree(handoff.child_mut().map_err(ToolError::from)?);
                let status = handoff
                    .child_mut()
                    .map_err(ToolError::from)?
                    .wait()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("failed to wait on killed child: {e}"),
                    })?;
                (status, true)
            }
        }
    };
    let crate::process::ProcessHandoffParts {
        child,
        stdout_task,
        stderr_task,
    } = handoff.into_parts().map_err(ToolError::from)?;
    drop(child);

    // Both drains settle concurrently so two held-open pipes cost one
    // grace period, not one per stream.
    let (stdout_open, stderr_open) = tokio::join!(
        settle_drain("stdout", stdout_task, drain_grace),
        settle_drain("stderr", stderr_task, drain_grace),
    );
    let (stdout_open, stderr_open) = (stdout_open?, stderr_open?);
    let captured = capture.finalize().await?;

    Ok(ShellOutcome::Completed(ShellExecution {
        exit_code: status.code().unwrap_or(SIGNAL_KILLED_EXIT_CODE),
        timed_out,
        streams_still_open: stdout_open || stderr_open,
        captured,
    }))
}

fn map_shell_io_error(error: &std::io::Error, operation: &str) -> ToolError {
    match crate::resource::classify_descriptor_error(error, operation, None) {
        Some(exhaustion) => ToolError::DescriptorExhausted(Box::new(exhaustion)),
        None => ToolError::ExecutionFailed {
            reason: format!("{operation}: {error}"),
        },
    }
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
/// The group is signalled directly with SIGKILL without spawning another
/// process; the direct child is additionally killed through the tokio handle
/// as a fallback. Descendants that moved themselves into a new
/// process group (`setsid`) escape the sweep — an accepted limitation.
///
/// On non-Unix targets only the direct child is killed.
fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id()
        && let Err(error) = crate::util::kill_process_group(pid)
    {
        tracing::warn!(
            pid,
            %error,
            "failed to signal timed-out bash process group",
        );
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
                .map_err(|error| {
                    map_shell_io_error(&error, &format!("reading {stream} from a shell"))
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
                Ok(Err(error)) => Err(map_shell_io_error(
                    &error,
                    &format!("reading {stream} from a shell"),
                )),
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

#[cfg(test)]
mod admission_tests {
    use std::io;

    use super::*;
    use crate::tool::envelope::ToolEnvelope;

    const CHILD_ENV: &str = "NORN_BASH_ADMISSION_CHILD";

    #[tokio::test]
    async fn cancelling_foreground_shell_releases_child_drains_and_capacity()
    -> Result<(), Box<dyn std::error::Error>> {
        const NAME: &str = "tools::bash::process::admission_tests::cancelling_foreground_shell_releases_child_drains_and_capacity";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", NAME, "--nocapture"])
                .env(CHILD_ENV, "1")
                .output()?;
            if output.status.success() {
                return Ok(());
            }
            return Err(io::Error::other(format!(
                "low-limit cancellation child failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ))
            .into());
        }

        lower_nofile_limit()?;
        let governor = DescriptorGovernor::global()?;
        let baseline = governor.available();
        let ctx = ToolContext::empty();
        let envelope = ToolEnvelope {
            tool_call_id: "cancel-admission".to_owned(),
            tool_name: "bash".to_owned(),
            model_args: serde_json::Value::Null,
            metadata: serde_json::Value::Null,
        };
        let capture = OutputCapture::new(&ctx, &envelope);
        let cwd = std::env::current_dir()?;
        let task = tokio::spawn(async move {
            run_shell(
                "sleep 30",
                &cwd,
                0,
                DEFAULT_DRAIN_GRACE,
                false,
                &ctx,
                capture,
            )
            .await
        });
        wait_for_available(&governor, baseline.saturating_sub(7)).await?;
        task.abort();
        let _cancelled = task.await;
        wait_for_available(&governor, baseline).await
    }

    fn lower_nofile_limit() -> io::Result<()> {
        let inherited = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        let target = inherited.maximum.map_or(48, |hard| hard.min(48));
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(target),
                maximum: inherited.maximum,
            },
        )
        .map_err(io::Error::from)
    }

    async fn wait_for_available(
        governor: &DescriptorGovernor,
        expected: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if governor.available() == expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|elapsed| {
            io::Error::other(format!(
                "descriptor capacity did not return to {expected}; observed {}: {elapsed}",
                governor.available(),
            ))
        })?;
        Ok(())
    }
}
