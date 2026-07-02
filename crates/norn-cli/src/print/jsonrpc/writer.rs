//! The single serializing stdout writer half of the driven duplex
//! (`DRIVEN-PROTOCOL.md` "Transport and framing"). The stdin reader half
//! lives in [`super::stdin`].
//!
//! A duplex channel carrying interleaved notifications and a run response
//! MUST NOT let two producers interleave-corrupt a frame. Exactly ONE task
//! ([`spawn_writer`]) owns stdout; every outbound frame is enqueued on an
//! `mpsc` channel and written by that task, one complete line at a time.
//! The [`OutboundWriter`] handle is cloneable and is the only way to emit.

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use super::frames::{JsonRpcNotification, JsonRpcResponse, TransportError};

/// A cloneable handle to the single serializing stdout writer.
///
/// All outbound frames funnel through one writer task so notifications and
/// the run response can never interleave-corrupt a line. Cloning the handle
/// clones the `mpsc` sender, not the writer.
#[derive(Clone)]
pub struct OutboundWriter {
    tx: mpsc::UnboundedSender<String>,
}

impl OutboundWriter {
    /// Enqueue a serialised response frame. A send failure means the writer
    /// task has stopped (stdout closed); it is surfaced, never swallowed.
    pub(crate) fn send_response(&self, resp: &JsonRpcResponse) -> Result<(), TransportError> {
        let line = serde_json::to_string(resp)?;
        self.tx.send(line)?;
        Ok(())
    }

    /// Enqueue a serialised notification frame (no `id`; never a response).
    pub(crate) fn send_notification(
        &self,
        note: &JsonRpcNotification,
    ) -> Result<(), TransportError> {
        let line = serde_json::to_string(note)?;
        self.tx.send(line)?;
        Ok(())
    }

    /// A writer over an in-process channel, so tests observe the exact
    /// outbound frame sequence without touching the real process stdout.
    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        (Self { tx }, rx)
    }
}

/// Spawn the single stdout-owning writer task.
///
/// The task holds the ONLY handle to stdout and writes each queued frame as
/// one newline-terminated line, flushing per line. It exits when every
/// [`OutboundWriter`] is dropped (the channel closes) or stdout breaks. The
/// returned [`OutboundWriter`] is the sole way to emit a frame.
#[must_use]
pub fn spawn_writer() -> (OutboundWriter, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                // stdout is gone; nothing more can be delivered. Drain and
                // exit so senders observe the closed channel promptly.
                rx.close();
                return;
            }
        }
    });
    (OutboundWriter { tx }, task)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_task_frames_each_message_as_one_line() {
        // The real stdout-owning writer: verify it exits cleanly when every
        // handle is dropped (the terminal-response shutdown handshake).
        let (writer, writer_task) = spawn_writer();
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(5), writer_task)
            .await
            .expect("writer task must exit when all handles drop")
            .expect("writer task must not panic");
    }
}
