use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::error::ToolError;
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::envelope::ToolEnvelope;

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
    redirected: bool,
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

    pub(super) async fn finalize(self: Arc<Self>) -> Result<CapturedOutput, ToolError> {
        let mut state = self.inner.lock().await;
        if !state.redirected {
            return Ok(CapturedOutput::Inline {
                stdout: std::mem::take(&mut state.stdout_inline),
                stderr: std::mem::take(&mut state.stderr_inline),
            });
        }

        state
            .close_log_file()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to flush redirected bash output: {e}"),
            })?;

        let output_chars = state.output_chars;
        let output_path = state.tilde_output_path()?;
        Ok(CapturedOutput::Redirected {
            output_path,
            output_chars,
        })
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
        let home = dirs::home_dir().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found")
        })?;
        let dir = home.join(".norn").join("outputs").join(session_id);
        tokio::fs::create_dir_all(&dir).await?;

        let output_path = dir.join(format!("{call_id}.log"));
        let mut log_file = File::create(&output_path).await?;
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
        let home = dirs::home_dir().ok_or_else(|| ToolError::ExecutionFailed {
            reason: "home directory not found while formatting bash output path".to_owned(),
        })?;
        if let Ok(stripped) = path.strip_prefix(&home) {
            Ok(format!("~/{}", stripped.to_string_lossy()))
        } else {
            Ok(path.to_string_lossy().into_owned())
        }
    }
}

pub(super) async fn drain_stdout(
    handle: ChildStdout,
    capture: Arc<OutputCapture>,
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
) -> std::io::Result<()> {
    let mut reader = BufReader::new(handle).lines();
    while let Some(line) = reader.next_line().await? {
        tracing::debug!(stream = "stderr", line = %line, "bash output line");
        capture.append_stderr(&format!("{line}\n")).await?;
    }
    Ok(())
}
