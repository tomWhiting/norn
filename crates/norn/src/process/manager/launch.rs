//! Managed-process launch, adoption, and output-drain construction.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use chrono::Utc;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::resource::{DescriptorGovernor, DescriptorPermit, TWO_PIPE_SPAWN_PEAK};
use crate::tool::context::ProcessEnv;
use crate::util::validate_private_component;

use super::{ProcessManager, RegistryEntry, SIGNAL_EXIT_CODE};
use crate::process::ProcessError;
use crate::process::handle::{ProcessHandle, ProcessShared, ProcessStatus};
use crate::process::spool::{Spool, SpoolAppender, StreamTag};

type DrainTask = JoinHandle<std::io::Result<()>>;

pub(crate) struct ProcessHandoffParts {
    pub(crate) child: Child,
    pub(crate) stdout_task: DrainTask,
    pub(crate) stderr_task: DrainTask,
}

/// A governed foreground process awaiting manager adoption.
///
/// Unless [`Self::into_parts`] transfers ownership into a registry entry,
/// dropping the handoff aborts both drains and kills the process group. This
/// prevents cancellation or spool-creation failure from detaching tasks that
/// retain pipe permits indefinitely.
pub(crate) struct ProcessHandoff {
    child: Option<Child>,
    stdout_task: Option<DrainTask>,
    stderr_task: Option<DrainTask>,
}

impl ProcessHandoff {
    pub(crate) fn new(child: Child, stdout_task: DrainTask, stderr_task: DrainTask) -> Self {
        Self {
            child: Some(child),
            stdout_task: Some(stdout_task),
            stderr_task: Some(stderr_task),
        }
    }

    pub(crate) fn child_mut(&mut self) -> Result<&mut Child, ProcessError> {
        self.child.as_mut().ok_or_else(|| ProcessError::Io {
            operation: "using a governed process handoff".to_owned(),
            reason: "handoff child was already transferred".to_owned(),
        })
    }

    pub(crate) fn into_parts(mut self) -> Result<ProcessHandoffParts, ProcessError> {
        let child = self.child.take().ok_or_else(|| ProcessError::Io {
            operation: "adopting a governed process handoff".to_owned(),
            reason: "handoff child was already transferred".to_owned(),
        })?;
        let stdout_task = self.stdout_task.take().ok_or_else(|| ProcessError::Io {
            operation: "adopting a governed process handoff".to_owned(),
            reason: "stdout drain was already transferred".to_owned(),
        })?;
        let stderr_task = self.stderr_task.take().ok_or_else(|| ProcessError::Io {
            operation: "adopting a governed process handoff".to_owned(),
            reason: "stderr drain was already transferred".to_owned(),
        })?;
        Ok(ProcessHandoffParts {
            child,
            stdout_task,
            stderr_task,
        })
    }
}

impl Drop for ProcessHandoff {
    fn drop(&mut self) {
        if let Some(task) = self.stdout_task.take() {
            task.abort();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(unix)]
        if let Some(pid) = child.id()
            && let Err(error) = crate::util::kill_process_group(pid)
        {
            tracing::warn!(pid, %error, "failed to kill an unadopted process group");
        }
        if let Err(error) = child.start_kill() {
            tracing::warn!(%error, "failed to kill an unadopted direct child");
        }
    }
}

impl ProcessManager {
    /// Resolve the spool path for a numeric id under this manager's token.
    fn spool_location(&self, id: u64) -> Result<(PathBuf, PathBuf), ProcessError> {
        let root = self.spool_root.as_ref().ok_or_else(|| {
            self.spool_root_error.clone().unwrap_or(ProcessError::Io {
                operation: "opening the private process-spool root".to_owned(),
                reason: "root was unavailable at manager construction".to_owned(),
            })
        })?;
        validate_private_component(&self.base_token, "process spool session id").map_err(
            |error| ProcessError::from_io("validating the process-spool session id", None, &error),
        )?;
        validate_private_component(&self.run_id, "process spool run id").map_err(|error| {
            ProcessError::from_io("validating the process-spool run id", None, &error)
        })?;
        let relative = PathBuf::from("outputs")
            .join(&self.base_token)
            .join("processes")
            .join(&self.run_id)
            .join(format!("p{id}.log"));
        Ok((root.clone(), relative))
    }

    /// Spawn a manager-owned shell process and capture both output pipes.
    ///
    /// # Errors
    ///
    /// Returns a typed failure from spool creation or process construction.
    pub async fn spawn(
        self: &Arc<Self>,
        command: &str,
        cwd: &Path,
        process_env: Option<&ProcessEnv>,
    ) -> Result<ProcessHandle, ProcessError> {
        let governor = DescriptorGovernor::global()
            .map_err(|error| ProcessError::DescriptorAdmission(Box::new(error)))?;
        let launch_permits = governor
            .try_acquire(TWO_PIPE_SPAWN_PEAK)
            .map_err(|error| ProcessError::DescriptorAdmission(Box::new(error)))?;
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let (spool_root, spool_relative) = self.spool_location(id)?;
        let (spool, launch_permits) =
            Spool::create_in_reserved(&spool_root, &spool_relative, launch_permits)
                .await
                .map_err(|error| {
                    ProcessError::from_io(
                        "creating a background-process spool",
                        Some(&spool_root.join(&spool_relative)),
                        &error,
                    )
                })?;
        let spool = Arc::new(spool);

        let mut child = build_bg_command(command, cwd, process_env)
            .spawn()
            .map_err(|error| {
                ProcessError::from_io("spawning a managed background process", None, &error)
            })?;
        let pid = child.id();
        let stdout = child.stdout.take().ok_or_else(|| ProcessError::Io {
            operation: "capturing managed-process stdout".to_owned(),
            reason: "child stdout pipe was not captured".to_owned(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| ProcessError::Io {
            operation: "capturing managed-process stderr".to_owned(),
            reason: "child stderr pipe was not captured".to_owned(),
        })?;
        let (appender, mut launch_permits) =
            spool.appender(launch_permits).await.map_err(|error| {
                ProcessError::from_io("opening the active process-spool writer", None, &error)
            })?;
        let stdout_permit = launch_permits.split(1).ok_or_else(|| ProcessError::Io {
            operation: "splitting managed-process descriptor admission".to_owned(),
            reason: "launch admission did not contain a stdout permit".to_owned(),
        })?;
        let stderr_permit = launch_permits.split(1).ok_or_else(|| ProcessError::Io {
            operation: "splitting managed-process descriptor admission".to_owned(),
            reason: "launch admission did not contain a stderr permit".to_owned(),
        })?;
        let drains = vec![
            tokio::spawn(drain_to_spool(
                stdout,
                appender.clone(),
                StreamTag::Stdout,
                stdout_permit,
            )),
            tokio::spawn(drain_to_spool(
                stderr,
                appender,
                StreamTag::Stderr,
                stderr_permit,
            )),
        ];
        Ok(self.install(id, command.to_owned(), pid, spool, child, drains))
    }

    /// Adopt a child whose output drains have already been established.
    ///
    /// # Errors
    ///
    /// Returns a typed failure from spool construction.
    pub(crate) async fn adopt(
        self: &Arc<Self>,
        command: &str,
        handoff: ProcessHandoff,
        private_fs_permit: DescriptorPermit,
    ) -> Result<(ProcessHandle, DescriptorPermit), ProcessError> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let (spool_root, spool_relative) = self.spool_location(id)?;
        let (spool, private_fs_permit) =
            Spool::create_in_reserved(&spool_root, &spool_relative, private_fs_permit)
                .await
                .map_err(|error| {
                    ProcessError::from_io(
                        "creating an adopted-process spool",
                        Some(&spool_root.join(&spool_relative)),
                        &error,
                    )
                })?;
        let spool = Arc::new(spool);
        let ProcessHandoffParts {
            child,
            stdout_task,
            stderr_task,
        } = handoff.into_parts()?;
        let pid = child.id();
        let drains = vec![stdout_task, stderr_task];
        Ok((
            self.install(id, command.to_owned(), pid, spool, child, drains),
            private_fs_permit,
        ))
    }

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
}

/// Build the `sh -c` command used by manager-owned processes.
pub(super) fn build_bg_command(
    command: &str,
    cwd: &Path,
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

/// Drain one child stream to the persistent spool.
pub(super) async fn drain_to_spool<R>(
    reader: R,
    appender: SpoolAppender,
    tag: StreamTag,
    _permit: DescriptorPermit,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        appender.append_tagged(tag, &line).await?;
    }
    Ok(())
}

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
