//! The stdin reader half of the driven duplex, off the tokio blocking pool
//! (`DRIVEN-PROTOCOL.md` "Transport and framing" / "Shutdown handshake").
//!
//! The driven contract lets the peer hold stdin open for the whole process
//! life — interventions can arrive mid-run, and closing the write half is
//! deliberately NOT required before the process exits ("After the terminal
//! response is enqueued … the process exits with the CLI exit code"). A
//! `tokio::io::stdin()`-based reader breaks that: tokio stdin runs its
//! `read(2)` as a task on the runtime's blocking pool, and a read left
//! in-flight when the run ends never returns while the peer keeps stdin
//! open. `Runtime::drop` then wedges forever in `BlockingPool::shutdown`
//! waiting for that read, so the process never exits after delivering the
//! terminal response.
//!
//! [`stdin_reader`] therefore keeps stdin off the runtime entirely: a
//! DETACHED std thread owns the blocking `read(2)` loop and forwards chunks
//! over an unbounded channel (mirroring the outbound writer's unbounded
//! queue in [`super::writer`]); [`StdinReader`] adapts the channel to
//! [`AsyncBufRead`] for the pre-run and intervene loops. The thread is
//! never joined — process exit does not wait for detached threads, so a
//! reader parked in `read(2)` on a still-open stdin cannot block teardown.

use std::io::{self, Read};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncBufRead, AsyncRead, ReadBuf};
use tokio::sync::mpsc;

/// Per-`read(2)` buffer size for the stdin reader thread: 8 KiB, the same
/// default capacity `std::io::BufReader` gave the previous
/// `BufReader<tokio::io::Stdin>` reader — reused, not invented.
const STDIN_READ_BUF_SIZE: usize = 8 * 1024;

/// The channel-backed stdin reader owning the inbound JSON-RPC half.
///
/// Receives the chunks the detached reader thread pulls off the process
/// stdin and serves them through [`AsyncBufRead`], so the pre-run loop and
/// the mid-run intervene loop keep their `read_line` framing while no
/// stdin read ever occupies the runtime's blocking pool.
pub struct StdinReader {
    /// Chunks (or the one terminal read error) from the reader thread. A
    /// closed channel is EOF: the thread exited on EOF, error, or because
    /// this receiver was dropped.
    rx: mpsc::UnboundedReceiver<io::Result<Vec<u8>>>,
    /// The chunk currently being served to `poll_fill_buf`.
    chunk: Vec<u8>,
    /// How much of `chunk` has been consumed.
    pos: usize,
}

impl StdinReader {
    /// A reader over an in-process channel, so tests drive the exact chunk
    /// sequence (split lines, errors, EOF) without touching process stdin.
    #[cfg(test)]
    pub(crate) fn test_channel() -> (mpsc::UnboundedSender<io::Result<Vec<u8>>>, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            tx,
            Self {
                rx,
                chunk: Vec::new(),
                pos: 0,
            },
        )
    }
}

impl AsyncRead for StdinReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let amount = {
            let available = ready!(self.as_mut().poll_fill_buf(cx))?;
            let n = available.len().min(buf.remaining());
            buf.put_slice(&available[..n]);
            n
        };
        self.consume(amount);
        Poll::Ready(Ok(()))
    }
}

impl AsyncBufRead for StdinReader {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let this = self.get_mut();
        while this.pos >= this.chunk.len() {
            match ready!(this.rx.poll_recv(cx)) {
                Some(Ok(chunk)) => {
                    this.chunk = chunk;
                    this.pos = 0;
                }
                Some(Err(err)) => return Poll::Ready(Err(err)),
                // Channel closed: the reader thread exited (EOF on stdin,
                // after surfacing an error, or spawn failure already
                // reported through the channel). Report EOF.
                None => return Poll::Ready(Ok(&[])),
            }
        }
        Poll::Ready(Ok(&this.chunk[this.pos..]))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let this = self.get_mut();
        this.pos = (this.pos + amt).min(this.chunk.len());
    }
}

/// Build the driven channel's stdin reader, spawning its detached reader
/// thread.
///
/// Isolated behind a function so the driven loop takes ownership of stdin
/// exactly once, mirroring how the writer task takes stdout. A thread-spawn
/// failure is surfaced as the reader's first (and only) read error — a
/// [`super::TransportError`] on the pre-run loop — never a silent EOF.
#[must_use]
pub fn stdin_reader() -> StdinReader {
    let (tx, rx) = mpsc::unbounded_channel::<io::Result<Vec<u8>>>();
    let spawned = std::thread::Builder::new()
        .name("norn-driven-stdin".to_owned())
        .spawn({
            let tx = tx.clone();
            move || read_stdin_chunks(&tx)
        });
    if let Err(err) = spawned {
        // Deliver the failure through the channel so the driven loop
        // answers with a transport error instead of misreading an
        // unreadable stdin as a clean pre-run EOF (exit 0, nothing to do).
        let _ = tx.send(Err(io::Error::other(format!(
            "failed to spawn the driven-mode stdin reader thread: {err}"
        ))));
    }
    StdinReader {
        rx,
        chunk: Vec::new(),
        pos: 0,
    }
}

/// The detached reader thread's loop: blocking `read(2)` on the process
/// stdin, forwarding each chunk (and at most one terminal error) until EOF
/// or until the [`StdinReader`] receiver is dropped.
fn read_stdin_chunks(tx: &mpsc::UnboundedSender<io::Result<Vec<u8>>>) {
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut buf = [0u8; STDIN_READ_BUF_SIZE];
    loop {
        match handle.read(&mut buf) {
            // EOF: the peer closed its write half. Dropping `tx` closes the
            // channel, which the reader observes as EOF.
            Ok(0) => return,
            Ok(n) => {
                if tx.send(Ok(buf[..n].to_vec())).is_err() {
                    // The reader (and with it the driven loop) is gone;
                    // nothing can consume further input.
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                // Surface the read failure to the driven loop (it becomes
                // a TransportError there), then stop — a broken stdin
                // cannot carry further frames.
                let _ = tx.send(Err(err));
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[tokio::test]
    async fn read_line_reassembles_lines_across_chunk_boundaries() {
        let (tx, mut reader) = StdinReader::test_channel();
        // One frame split across three chunks, plus the start of the next.
        tx.send(Ok(b"{\"jsonrpc\"".to_vec())).unwrap();
        tx.send(Ok(b":\"2.0\"}".to_vec())).unwrap();
        tx.send(Ok(b"\nnext".to_vec())).unwrap();
        drop(tx);

        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0);
        assert_eq!(line, "{\"jsonrpc\":\"2.0\"}\n");

        // The trailing partial line is served up to EOF (channel closed).
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "next");

        line.clear();
        let n = reader.read_line(&mut line).await.unwrap();
        assert_eq!(n, 0, "closed channel reads as EOF");
    }

    #[tokio::test]
    async fn closed_channel_is_immediate_eof() {
        let (tx, mut reader) = StdinReader::test_channel();
        drop(tx);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn read_error_surfaces_then_eof() {
        let (tx, mut reader) = StdinReader::test_channel();
        tx.send(Err(io::Error::other("stdin torn"))).unwrap();
        drop(tx);
        let mut line = String::new();
        let err = reader.read_line(&mut line).await.unwrap_err();
        assert!(err.to_string().contains("stdin torn"));
        // After the terminal error the channel is closed: EOF, not a hang.
        line.clear();
        let n = reader.read_line(&mut line).await.unwrap();
        assert_eq!(n, 0);
    }
}
