use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::error::ToolError;
use crate::process::spool::SpoolAppender;
use crate::process::{Spool, StreamTag};
use crate::resource::DescriptorPermit;
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::util::{PrivateRoot, validate_private_component};

use super::tool::INLINE_OUTPUT_THRESHOLD_CHARS;

#[derive(Debug)]
pub(super) enum CapturedOutput {
    Inline {
        stdout: String,
        stderr: String,
    },
    Redirected {
        output_path: String,
        output_chars: usize,
    },
}

/// The outcome of migrating a foreground capture onto a background spool
/// ([`OutputCapture::attach_spool`]): the pre-migration snapshot returned in
/// the tool result, plus the model's initial `output` cursor for the adopted
/// process (the F5 double-delivery fix).
#[derive(Debug)]
pub(super) struct MigrationSnapshot {
    /// The pre-migration output, shaped exactly as a completed run's result
    /// (inline stdout/stderr, or a redirect reference for large output).
    pub(super) output: CapturedOutput,
    /// Where the model's `output` cursor for the adopted process must start:
    ///
    /// - **Inline snapshot** → the committed spool length at attach time. The
    ///   model already saw these bytes inline in the migrated tool result, so
    ///   its first `op=output` must skip them and return only new
    ///   post-migration output.
    /// - **Redirect snapshot** → `0`. The model saw only a spool path, never
    ///   the bytes, so `op=output` should return the full spool from the start.
    pub(super) model_cursor_seed: u64,
}

#[derive(Debug)]
pub(super) struct OutputCapture {
    session_id: String,
    call_id: String,
    inner: AsyncMutex<OutputCaptureState>,
}

#[derive(Debug, Default)]
struct OutputCaptureState {
    stdout_inline: String,
    stderr_inline: String,
    log_file: Option<File>,
    output_chars: usize,
    output_path: Option<PathBuf>,
    output_root: Option<Arc<PrivateRoot>>,
    output_relative: Option<PathBuf>,
    redirected: bool,
    /// Once a command migrates to the background (R4), the capture switches to
    /// teeing every subsequent line into the process manager's spool, tagged
    /// with its stream. Set by [`OutputCapture::attach_spool`].
    spool: Option<SpoolAppender>,
    /// Launch capacity not owned by the two pipe drains. It covers redirect
    /// storage and migration peaks, then transfers one permit to the active
    /// spool writer when migration succeeds.
    auxiliary_permit: Option<DescriptorPermit>,
}

impl OutputCapture {
    pub(super) fn new(ctx: &ToolContext, envelope: &ToolEnvelope) -> Arc<Self> {
        let session_id = ctx
            .get_extension::<SessionId>()
            .map_or_else(|| Uuid::new_v4().to_string(), |session| session.0.clone());
        Arc::new(Self {
            session_id,
            call_id: envelope.tool_call_id.clone(),
            inner: AsyncMutex::new(OutputCaptureState::default()),
        })
    }

    async fn append_stdout(&self, text: &str) -> std::io::Result<()> {
        let mut state = self.inner.lock().await;
        state
            .append(StreamKind::Stdout, text, &self.session_id, &self.call_id)
            .await
    }

    async fn append_stderr(&self, text: &str) -> std::io::Result<()> {
        let mut state = self.inner.lock().await;
        state
            .append(StreamKind::Stderr, text, &self.session_id, &self.call_id)
            .await
    }

    pub(super) async fn install_auxiliary_permit(&self, permit: DescriptorPermit) {
        self.inner.lock().await.auxiliary_permit = Some(permit);
    }

    pub(super) async fn take_auxiliary_permit(&self) -> Result<DescriptorPermit, ToolError> {
        self.inner
            .lock()
            .await
            .auxiliary_permit
            .take()
            .ok_or_else(|| ToolError::ExecutionFailed {
                reason: "shell launch admission did not retain migration capacity".to_owned(),
            })
    }

    /// Migrate this capture to a background spool (R4): seed the spool with the
    /// output captured so far, switch every subsequent line to tee into the
    /// spool, and return the pre-migration snapshot for the tool result plus the
    /// model's initial `output` cursor (see [`MigrationSnapshot`]).
    ///
    /// After this call the drain tasks keep running (the process is now
    /// manager-owned) and their output flows to the spool, not the inline or
    /// redirect buffers.
    ///
    /// The model-cursor seed is measured **while the capture lock is still
    /// held**, before the newly-attached spool is published to the drains, so
    /// it captures exactly the seeded length: no post-migration line can have
    /// teed yet (a drain's `append` blocks on this same lock until it is
    /// released below).
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::ExecutionFailed`] when the buffered output cannot
    /// be read from the redirect file or seeded into the spool.
    pub(super) async fn attach_spool(
        &self,
        spool: Arc<Spool>,
        mut private_fs_permit: DescriptorPermit,
    ) -> Result<MigrationSnapshot, ToolError> {
        let mut state = self.inner.lock().await;
        let snapshot = if state.redirected {
            state.close_log_file().await.map_err(|error| {
                output_io_error(
                    &error,
                    "flushing redirected bash output before migration",
                    state.output_path.as_deref(),
                )
            })?;
            let output_root =
                state
                    .output_root
                    .as_ref()
                    .ok_or_else(|| ToolError::ExecutionFailed {
                        reason: "bash output was redirected without a private root".to_owned(),
                    })?;
            let output_relative =
                state
                    .output_relative
                    .as_ref()
                    .ok_or_else(|| ToolError::ExecutionFailed {
                        reason: "bash output was redirected without a relative path".to_owned(),
                    })?;
            let output_path = output_root.display_path(output_relative);
            let mut redirected =
                File::from_std(output_root.open_read(output_relative).map_err(|error| {
                    output_io_error(
                        &error,
                        "opening pre-migration bash output",
                        Some(&output_path),
                    )
                })?);
            let mut bytes = Vec::new();
            redirected.read_to_end(&mut bytes).await.map_err(|error| {
                output_io_error(
                    &error,
                    "reading pre-migration bash output",
                    Some(&output_path),
                )
            })?;
            private_fs_permit = spool
                .append_raw_reserved(&bytes, private_fs_permit)
                .await
                .map_err(|error| {
                    output_io_error(&error, "seeding a managed-process spool", None)
                })?;
            CapturedOutput::Redirected {
                output_path: state.tilde_output_path()?,
                output_chars: state.output_chars,
            }
        } else {
            let stdout = std::mem::take(&mut state.stdout_inline);
            let stderr = std::mem::take(&mut state.stderr_inline);
            for buffered in [stdout.as_bytes(), stderr.as_bytes()] {
                private_fs_permit = spool
                    .append_raw_reserved(buffered, private_fs_permit)
                    .await
                    .map_err(|error| {
                        output_io_error(&error, "seeding a managed-process spool", None)
                    })?;
            }
            CapturedOutput::Inline { stdout, stderr }
        };
        // Decide the model's initial cursor from *how* the seed was delivered.
        // For an inline snapshot the model already saw the seeded bytes, so its
        // cursor starts past them (the committed length as of now — still under
        // the lock, so equal to exactly the seeded length). For a redirect the
        // model saw only a path, so the cursor stays at 0 and `op=output`
        // returns the full spool.
        let model_cursor_seed = match &snapshot {
            CapturedOutput::Inline { .. } => spool.committed_len(),
            CapturedOutput::Redirected { .. } => 0,
        };
        let (appender, _surplus) = spool.appender(private_fs_permit).await.map_err(|error| {
            output_io_error(&error, "opening the migrated process-spool writer", None)
        })?;
        state.output_root = None;
        state.output_relative = None;
        state.spool = Some(appender);
        Ok(MigrationSnapshot {
            output: snapshot,
            model_cursor_seed,
        })
    }

    pub(super) async fn finalize(self: Arc<Self>) -> Result<CapturedOutput, ToolError> {
        let mut state = self.inner.lock().await;
        if !state.redirected {
            state.auxiliary_permit = None;
            return Ok(CapturedOutput::Inline {
                stdout: std::mem::take(&mut state.stdout_inline),
                stderr: std::mem::take(&mut state.stderr_inline),
            });
        }

        state.close_log_file().await.map_err(|error| {
            output_io_error(
                &error,
                "flushing redirected bash output",
                state.output_path.as_deref(),
            )
        })?;

        let output_chars = state.output_chars;
        let output_path = state.tilde_output_path()?;
        state.output_root = None;
        state.output_relative = None;
        state.auxiliary_permit = None;
        Ok(CapturedOutput::Redirected {
            output_path,
            output_chars,
        })
    }
}

fn output_io_error(error: &std::io::Error, operation: &str, path: Option<&Path>) -> ToolError {
    match crate::resource::classify_descriptor_error(error, operation, path) {
        Some(exhaustion) => ToolError::DescriptorExhausted(Box::new(exhaustion)),
        None => ToolError::ExecutionFailed {
            reason: format!("{operation}: {error}"),
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamKind {
    Stdout,
    Stderr,
}

impl OutputCaptureState {
    async fn append(
        &mut self,
        stream: StreamKind,
        text: &str,
        session_id: &str,
        call_id: &str,
    ) -> std::io::Result<()> {
        // Post-migration: the spool is the sink. Every line tees there,
        // tagged with its stream, and the inline/redirect buffers are no
        // longer touched (they were flushed into the spool at attach time).
        if let Some(spool) = self.spool.clone() {
            let tag = match stream {
                StreamKind::Stdout => StreamTag::Stdout,
                StreamKind::Stderr => StreamTag::Stderr,
            };
            spool.append_tagged(tag, text).await?;
            return Ok(());
        }
        let chars = text.chars().count();
        if !self.redirected
            && self.output_chars.saturating_add(chars) > INLINE_OUTPUT_THRESHOLD_CHARS
        {
            self.start_redirect(session_id, call_id).await?;
        }

        self.output_chars = self.output_chars.saturating_add(chars);
        if self.redirected {
            self.log_file_mut()?.write_all(text.as_bytes()).await?;
            return Ok(());
        }

        match stream {
            StreamKind::Stdout => self.stdout_inline.push_str(text),
            StreamKind::Stderr => self.stderr_inline.push_str(text),
        }
        Ok(())
    }

    async fn start_redirect(&mut self, session_id: &str, call_id: &str) -> std::io::Result<()> {
        validate_private_component(session_id, "bash output session id")?;
        validate_private_component(call_id, "bash output tool-call id")?;
        let norn_home = crate::config::paths::norn_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "bash output requires an absolute NORN_HOME or user home directory",
            )
        })?;
        let root = Arc::new(PrivateRoot::create(&norn_home)?);
        let output_relative = PathBuf::from("outputs")
            .join(session_id)
            .join(format!("{call_id}.log"));
        if let Some(parent) = output_relative.parent() {
            root.create_dir_all(parent)?;
        }
        let output_path = root.display_path(&output_relative);
        let mut log_file = File::from_std(root.create_new(&output_relative)?);
        if !self.stdout_inline.is_empty() {
            log_file.write_all(self.stdout_inline.as_bytes()).await?;
        }
        if !self.stderr_inline.is_empty() {
            log_file.write_all(self.stderr_inline.as_bytes()).await?;
        }
        self.stdout_inline.clear();
        self.stderr_inline.clear();
        self.log_file = Some(log_file);
        self.output_path = Some(output_path);
        self.output_root = Some(root);
        self.output_relative = Some(output_relative);
        self.redirected = true;
        Ok(())
    }

    async fn close_log_file(&mut self) -> std::io::Result<()> {
        if let Some(mut file) = self.log_file.take() {
            file.flush().await?;
            file.shutdown().await?;
        }
        Ok(())
    }

    fn log_file_mut(&mut self) -> std::io::Result<&mut File> {
        self.log_file.as_mut().ok_or_else(|| {
            std::io::Error::other("bash redirect log file missing after redirect started")
        })
    }

    fn tilde_output_path(&self) -> Result<String, ToolError> {
        let path = self
            .output_path
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed {
                reason: "bash output was marked redirected without an output path".to_owned(),
            })?;
        if let Some(home) = crate::config::paths::trusted_home_dir()
            && let Ok(stripped) = path.strip_prefix(&home)
        {
            return Ok(format!("~/{}", stripped.to_string_lossy()));
        }
        Ok(path.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[tokio::test]
    async fn provider_tool_call_id_cannot_escape_private_output_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = OutputCaptureState::default();
        let error = state
            .start_redirect("session", "../outside")
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("hostile tool-call id unexpectedly accepted"))?;

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(state.output_path.is_none());
        Ok(())
    }
}

pub(super) async fn drain_stdout(
    handle: ChildStdout,
    capture: Arc<OutputCapture>,
    _permit: DescriptorPermit,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(handle).lines();
    while let Some(line) = reader.next_line().await? {
        tracing::debug!(stream = "stdout", line = %line, "bash output line");
        capture.append_stdout(&format!("{line}\n")).await?;
    }
    Ok(())
}

pub(super) async fn drain_stderr(
    handle: ChildStderr,
    capture: Arc<OutputCapture>,
    _permit: DescriptorPermit,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(handle).lines();
    while let Some(line) = reader.next_line().await? {
        tracing::debug!(stream = "stderr", line = %line, "bash output line");
        capture.append_stderr(&format!("{line}\n")).await?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// F6 (attach seam): a failed `attach_spool` surfaces a named
    /// `ExecutionFailed` — it never silently succeeds. This is the error the
    /// tool's Migrated branch converts into "kill the adoptee + named error"
    /// rather than propagating a bare `?` that leaves a half-migrated process.
    ///
    /// Driving the full tool-level kill-on-failure path would require injecting
    /// an I/O failure *between* adopt and attach during a live migration, which
    /// is not cleanly injectable; the tool code path is instead kept obviously
    /// correct (it matches on this error and kills the adoptee), and this test
    /// pins the failure carrier attach produces.
    #[tokio::test]
    async fn attach_spool_surfaces_a_named_error_on_unreadable_redirect()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let output_root = Arc::new(PrivateRoot::open(dir.path())?);
        let missing_relative = PathBuf::from("does-not-exist.log");
        let missing = output_root.display_path(&missing_relative);
        // A capture in the redirected state whose backing file is absent: the
        // pre-migration read must fail with a named error.
        let capture = OutputCapture {
            session_id: "sess".to_owned(),
            call_id: "call".to_owned(),
            inner: AsyncMutex::new(OutputCaptureState {
                redirected: true,
                output_path: Some(missing),
                output_root: Some(output_root),
                output_relative: Some(missing_relative),
                output_chars: 123,
                ..OutputCaptureState::default()
            }),
        };
        let spool = Arc::new(Spool::create(dir.path().join("p1.log")).await?);
        let private_fs_permit = crate::resource::DescriptorGovernor::global()?
            .try_acquire(crate::resource::PRIVATE_FS_OPERATION_PEAK)?;

        let Err(ToolError::ExecutionFailed { reason }) =
            capture.attach_spool(spool, private_fs_permit).await
        else {
            return Err(std::io::Error::other(
                "an unreadable redirect file did not return ExecutionFailed",
            )
            .into());
        };
        assert!(
            reason.contains("pre-migration"),
            "the error names the failed pre-migration read: {reason}",
        );
        Ok(())
    }
}
