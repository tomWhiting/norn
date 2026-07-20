//! The single serializing stdout writer half of the driven duplex
//! (`DRIVEN-PROTOCOL.md` "Transport and framing"). The stdin reader half
//! lives in [`super::stdin`].
//!
//! A duplex channel carrying interleaved notifications and a run response
//! MUST NOT let two producers interleave-corrupt a frame. Exactly ONE task
//! ([`spawn_writer`]) owns stdout; every outbound frame is enqueued on an
//! `mpsc` channel and written by that task, one complete line at a time.
//! The [`OutboundWriter`] handle is cloneable and is the only way to emit.

use tokio::io::{AsyncWrite, AsyncWriteExt};
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
pub fn spawn_writer() -> (
    OutboundWriter,
    tokio::task::JoinHandle<Result<(), TransportError>>,
) {
    spawn_writer_to(tokio::io::stdout())
}

fn spawn_writer_to<W>(
    mut output: W,
) -> (
    OutboundWriter,
    tokio::task::JoinHandle<Result<(), TransportError>>,
)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let task = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            output.write_all(line.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
        Ok(())
    });
    (OutboundWriter { tx }, task)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use super::*;

    struct RecordingSink {
        bytes: Arc<parking_lot::Mutex<Vec<u8>>>,
        flushes: Arc<AtomicUsize>,
    }

    impl AsyncWrite for RecordingSink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.bytes.lock().extend_from_slice(buffer);
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct BrokenSink {
        fail_flush: bool,
    }

    impl AsyncWrite for BrokenSink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.fail_flush {
                Poll::Ready(Ok(buf.len()))
            } else {
                Poll::Ready(Err(ErrorKind::BrokenPipe.into()))
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            if self.fail_flush {
                Poll::Ready(Err(ErrorKind::BrokenPipe.into()))
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn broken_sink_result(
        fail_flush: bool,
    ) -> Result<Result<(), TransportError>, tokio::task::JoinError> {
        let (writer, task) = spawn_writer_to(BrokenSink { fail_flush });
        let response = JsonRpcResponse::ok(serde_json::json!(1), serde_json::Value::Null);
        assert!(writer.send_response(&response).is_ok());
        drop(writer);
        task.await
    }

    #[tokio::test]
    async fn writer_task_frames_and_flushes_each_message() -> Result<(), Box<dyn std::error::Error>>
    {
        let bytes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let flushes = Arc::new(AtomicUsize::new(0));
        let (writer, writer_task) = spawn_writer_to(RecordingSink {
            bytes: Arc::clone(&bytes),
            flushes: Arc::clone(&flushes),
        });
        let response = JsonRpcResponse::ok(serde_json::json!(7), serde_json::json!({"ok": true}));
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "event/test",
            params: serde_json::json!({"n": 2}),
        };
        writer.send_response(&response)?;
        writer.send_notification(&notification)?;
        drop(writer);
        writer_task.await??;

        let expected = format!(
            "{}\n{}\n",
            serde_json::to_string(&response)?,
            serde_json::to_string(&notification)?,
        );
        assert_eq!(bytes.lock().as_slice(), expected.as_bytes());
        assert_eq!(flushes.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn writer_task_surfaces_write_failure() {
        let outcome = broken_sink_result(false).await;
        assert!(
            matches!(outcome, Ok(Err(TransportError::Io(ref error))) if error.kind() == ErrorKind::BrokenPipe),
            "outcome: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn writer_task_surfaces_flush_failure() {
        let outcome = broken_sink_result(true).await;
        assert!(
            matches!(outcome, Ok(Err(TransportError::Io(ref error))) if error.kind() == ErrorKind::BrokenPipe),
            "outcome: {outcome:?}"
        );
    }
}
