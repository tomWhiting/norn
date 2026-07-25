//! The background stream renderer for `-f stream-json` (NC-003 R8).
//!
//! Consumes [`norn::provider::AgentEvent`]s from the broadcast channel
//! and writes one NDJSON object per line onto the invocation's
//! [`StreamSink`] as they arrive; the per-event payload shape lives in
//! [`super::output`] (`agent_event_to_ndjson`), which is also the single
//! source of truth for the driven `event/*` notification payloads. Split
//! out of `output.rs` so that module stays within the 500-line budget.
//!
//! The sink is the ONE destination of the whole `stream-json` stream —
//! streamed events, diagnostics, and the terminal `completed` envelope —
//! so `-o PATH` redirects all of it to the file in append-consistent
//! order instead of splitting events onto terminal stdout (retry-forever
//! DESIGN D9 / M3).
//!
//! Write failures are classified, never collapsed (D9 "silent
//! truncation"): [`std::io::ErrorKind::BrokenPipe`] is the conventional
//! quiet stop of a reader that deliberately went away, and suppresses the
//! terminal envelope that would otherwise be written into a dead pipe;
//! every other write error is a typed [`PrintError::StreamTorn`] failure
//! that fails the run with a non-zero exit and a stderr diagnostic.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast::error::RecvError;

use super::error::{PrintError, preserve_run_failure};
use super::output::agent_event_to_ndjson;
use crate::cli::{Cli, OutputFormat};

/// Result of one accepted write onto a [`StreamSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamWrite {
    /// The bytes were written and flushed.
    Written,
    /// The reader deliberately closed the stream (`BrokenPipe`). The
    /// stream stops quietly and every later write is skipped, so no
    /// terminal envelope is aimed at a dead pipe.
    ReaderGone,
}

/// The sized writer handed to a [`StreamSink::write_with`] closure, so
/// generic renderers such as
/// [`emit_stream_completed`](super::output::emit_stream_completed) can be
/// pointed at the sink without being made `?Sized`.
pub struct StreamWriter<'sink> {
    inner: &'sink mut (dyn Write + Send),
}

impl Write for StreamWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl std::fmt::Debug for StreamWriter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamWriter").finish_non_exhaustive()
    }
}

/// The single destination of an invocation's `stream-json` output:
/// process stdout, or the `-o PATH` file.
///
/// Cloneable and shared between the background renderer and the
/// foreground envelope writers; the interior lock serialises whole lines
/// so an event can never interleave with the terminal envelope. Opening
/// the file is fallible and fails LOUDLY — there is no fallback to
/// stdout, which would silently split the stream the flag asked to
/// redirect.
pub struct StreamSink {
    state: Arc<Mutex<SinkState>>,
}

impl Clone for StreamSink {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl std::fmt::Debug for StreamSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSink").finish_non_exhaustive()
    }
}

struct SinkState {
    /// Process stdout or the opened `-o` file.
    writer: Box<dyn Write + Send>,
    /// RAII descriptor-budget guard held for as long as the file handle
    /// above is open (`None` for stdout, which is not admitted).
    _descriptor_permit: Option<norn::resource::FilesystemOperationPermit>,
    /// Latched once the reader closed the pipe; every later write is a
    /// no-op so the terminal envelope is withheld rather than aimed at a
    /// dead pipe.
    reader_gone: bool,
}

impl StreamSink {
    /// The sink this invocation's `stream-json` output belongs on: the
    /// `-o PATH` file when the flag is present, process stdout otherwise.
    ///
    /// # Errors
    ///
    /// [`PrintError::Io`] when the output file cannot be admitted by the
    /// descriptor governor or cannot be created.
    pub fn for_cli(cli: &Cli) -> Result<Self, PrintError> {
        match cli.output.as_deref() {
            Some(path) => Self::to_file(path),
            None => Ok(Self::stdout()),
        }
    }

    /// The sink for one print-mode invocation, or `None` when the
    /// invocation has no NDJSON stream: any other output format, or
    /// driven mode, whose stdout carries JSON-RPC frames only and whose
    /// events ride the `event/*` notifications.
    ///
    /// # Errors
    ///
    /// [`PrintError::Io`] when `-o PATH` cannot be opened.
    pub fn for_invocation(cli: &Cli, is_driven: bool) -> Result<Option<Self>, PrintError> {
        let format = cli.output_format.unwrap_or(OutputFormat::Text);
        if is_driven || !matches!(format, OutputFormat::StreamJson) {
            return Ok(None);
        }
        Self::for_cli(cli).map(Some)
    }

    /// A sink over process stdout.
    #[must_use]
    pub fn stdout() -> Self {
        Self::from_state(SinkState {
            writer: Box::new(std::io::stdout()),
            _descriptor_permit: None,
            reader_gone: false,
        })
    }

    /// A sink over a freshly created file at `path`.
    ///
    /// # Errors
    ///
    /// [`PrintError::Io`] when descriptor admission or file creation
    /// fails. The failure is returned, never downgraded to a stdout
    /// fallback: a stream that silently ignores `-o` is worse than one
    /// that refuses to start.
    pub fn to_file(path: &Path) -> Result<Self, PrintError> {
        let permit = norn::resource::acquire_filesystem_operation().map_err(|err| {
            PrintError::Io(format!(
                "stream-json output file could not be admitted by the descriptor governor: {err}"
            ))
        })?;
        let file = std::fs::File::create(path).map_err(|err| {
            PrintError::Io(format!(
                "stream-json output file could not be created: {err}"
            ))
        })?;
        Ok(Self::from_state(SinkState {
            writer: Box::new(file),
            _descriptor_permit: Some(permit),
            reader_gone: false,
        }))
    }

    fn from_state(state: SinkState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Run `write` against the sink's writer under the sink lock, then
    /// flush, classifying the outcome.
    ///
    /// # Errors
    ///
    /// [`PrintError::StreamTorn`] when the write or the flush fails for
    /// any reason other than a broken pipe — the bytes already emitted
    /// are an incomplete stream, so the run must fail loudly rather than
    /// report success over truncated output. A poisoned sink lock (a
    /// writer panicked mid-line) is the same class of failure.
    pub fn write_with<F>(&self, write: F) -> Result<StreamWrite, PrintError>
    where
        F: FnOnce(&mut StreamWriter<'_>) -> std::io::Result<()>,
    {
        let mut state = self.state.lock().map_err(|poison| {
            PrintError::StreamTorn(format!(
                "stream-json sink lock poisoned by a panicked writer ({poison}); streamed \
                 output is incomplete"
            ))
        })?;
        if state.reader_gone {
            return Ok(StreamWrite::ReaderGone);
        }
        let mut outcome = write(&mut StreamWriter {
            inner: &mut *state.writer,
        });
        if outcome.is_ok() {
            outcome = state.writer.flush();
        }
        match outcome {
            Ok(()) => Ok(StreamWrite::Written),
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {
                state.reader_gone = true;
                // The conventional quiet stop: the consumer (`| head`,
                // a closed pipe) went away deliberately. Recorded rather
                // than reported so the stop stays quiet, and latched so
                // no terminal envelope is aimed at the dead pipe.
                tracing::debug!(
                    "stream-json consumer closed the pipe; stopping quietly and withholding \
                     the terminal envelope"
                );
                Ok(StreamWrite::ReaderGone)
            }
            Err(err) => Err(PrintError::StreamTorn(format!(
                "stream-json output write failed ({kind:?}): {err}; streamed output is incomplete",
                kind = err.kind(),
            ))),
        }
    }

    /// Write one NDJSON line (payload plus the terminating newline).
    ///
    /// # Errors
    ///
    /// As [`StreamSink::write_with`].
    fn write_line(&self, line: &str) -> Result<StreamWrite, PrintError> {
        self.write_with(|writer| {
            writer.write_all(line.as_bytes())?;
            writer.write_all(b"\n")
        })
    }

    /// Test-only sink over an arbitrary writer, so the write-failure and
    /// broken-pipe paths can be driven without a real pipe or a full
    /// disk.
    #[cfg(test)]
    pub(crate) fn from_writer(writer: Box<dyn Write + Send>) -> Self {
        Self::from_state(SinkState {
            writer,
            _descriptor_permit: None,
            reader_gone: false,
        })
    }
}

/// Handle to the background stream renderer spawned by
/// [`spawn_stream_renderer`].
///
/// The renderer cannot rely on broadcast-channel closure to terminate:
/// the tool registry's shared `ToolContext` holds a
/// [`norn::provider::SharedAgentEventChannel`] extension with an owned
/// `Sender` clone (for subagent event forwarding), so the channel stays
/// open for as long as the runtime exists — awaiting closure alone hangs
/// forever (REVIEW C1). [`Self::finish_run`] sends an explicit shutdown
/// signal; the renderer drains every event already buffered on its
/// receiver, writes them, and exits while preserving the run result's
/// exit authority.
pub struct StreamRendererHandle {
    /// Explicit shutdown trigger consumed by [`Self::finish_run`].
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// The renderer task itself. Its result carries the classified write
    /// outcome: `Ok(())` for a stream that completed or stopped quietly
    /// on a broken pipe, `Err(PrintError::StreamTorn)` for a stream that
    /// was cut short by a write failure.
    task: tokio::task::JoinHandle<Result<(), PrintError>>,
}

impl StreamRendererHandle {
    /// Drain and stop the renderer, then reconcile its shutdown with the run
    /// result.
    ///
    /// Call this only after the step's own senders have been dropped —
    /// events broadcast after the shutdown signal are not rendered.
    ///
    /// # Errors
    ///
    /// Returns the primary run failure when the renderer exits cleanly. A
    /// renderer panic, cancellation, or write failure overrides success, but is
    /// retained as a torn-stream companion to an existing run failure so that
    /// failure keeps its original exit code without emitting a terminal stream
    /// envelope. A quiet stop on a broken pipe is NOT a failure: the reader
    /// went away deliberately, and the withheld envelope is handled by the
    /// sink, not by the exit code.
    pub async fn finish_run<T, E>(self, result: Result<T, E>) -> Result<T, PrintError>
    where
        E: Into<PrintError>,
    {
        let renderer_error = match self.finish_task().await {
            Ok(Ok(())) => None,
            Ok(Err(torn)) => Some(torn),
            Err(join_error) => Some(renderer_failure(&join_error)),
        };
        preserve_run_failure(result, renderer_error)
    }

    async fn finish_task(self) -> Result<Result<(), PrintError>, tokio::task::JoinError> {
        // A failed send means the receiver half is gone, i.e. the task
        // already exited on its own (channel closed / stdout broken) —
        // not an error; the join below still completes.
        let _ = self.shutdown.send(());
        self.task.await
    }
}

/// Map a renderer panic or cancellation onto the torn-stream path. The NDJSON
/// already written to stdout is incomplete, so no terminal envelope may be
/// appended to it.
fn renderer_failure(err: &tokio::task::JoinError) -> PrintError {
    PrintError::StreamTorn(format!(
        "stream renderer task failed ({kind}): {err}; streamed output on stdout is incomplete",
        kind = if err.is_panic() { "panic" } else { "cancelled" },
    ))
}

/// Spawn the streaming renderer for `stream-json` mode (NC-003 R8).
///
/// Subscribes to `tx`, then writes one NDJSON object per line onto `sink`
/// for every [`norn::provider::events::ProviderEvent`]. The task exits
/// when the broadcast sender is dropped, when the
/// [`StreamRendererHandle::finish_run`] shutdown signal fires (after
/// draining the events already buffered), when the reader closes the pipe,
/// or when a write fails. Lagged receivers skip the missed events
/// (best-effort — downstream pipes may miss events; the brief accepts this
/// trade-off).
///
/// `sink` is the SAME sink the terminal `completed` envelope is written
/// to, so `-o PATH` carries the whole stream in order (D9 / M3).
///
/// When `partial` is `false` (the default), only complete events are
/// emitted: `text`, `thinking`, `tool_call`, `tool_result`, `done`.
/// Delta events (`text_delta`, `thinking_delta`, `tool_call_delta`) are
/// silently consumed. When `partial` is `true`, all events are emitted.
///
/// Returns a [`StreamRendererHandle`]; callers MUST terminate the task
/// via [`StreamRendererHandle::finish_run`] rather than awaiting channel
/// closure — the registry's shared-context sender clone keeps the channel
/// open for the lifetime of the runtime (REVIEW C1). Requiring the run result
/// at this boundary prevents renderer shutdown from discarding a primary
/// provider or authentication failure, or a write failure that truncated
/// the stream.
#[must_use]
pub fn spawn_stream_renderer(
    tx: &tokio::sync::broadcast::Sender<norn::provider::AgentEvent>,
    partial: bool,
    sink: StreamSink,
) -> StreamRendererHandle {
    let rx = tx.subscribe();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(render_stream(rx, shutdown_rx, sink, partial));
    StreamRendererHandle {
        shutdown: shutdown_tx,
        task,
    }
}

/// The renderer task body: render every received event until the channel
/// closes, the shutdown signal fires, the reader goes away, or a write
/// fails.
///
/// # Errors
///
/// [`PrintError::StreamTorn`] when a write fails for any reason other
/// than a broken pipe — the caller MUST NOT report success over a
/// truncated stream.
async fn render_stream(
    mut rx: tokio::sync::broadcast::Receiver<norn::provider::AgentEvent>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    sink: StreamSink,
    partial: bool,
) -> Result<(), PrintError> {
    loop {
        tokio::select! {
            // Biased: always drain events that are already ready
            // before observing shutdown, so nothing broadcast ahead
            // of the signal is dropped. The shutdown branch returns
            // immediately, so the completed oneshot is never polled
            // again.
            biased;
            received = rx.recv() => match received {
                Ok(agent_event) => {
                    if write_stream_event(&sink, &agent_event, partial)? == StreamWrite::ReaderGone {
                        return Ok(());
                    }
                }
                Err(RecvError::Closed) => return Ok(()),
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "stream renderer lagged — {n} events dropped");
                }
            },
            // Resolves on the explicit signal AND when the handle is
            // dropped without calling finish() — the renderer must
            // never outlive its orchestrator.
            _ = &mut shutdown_rx => {
                return drain_buffered_events(&sink, &mut rx, partial);
            }
        }
    }
}

/// Write one agent event as an NDJSON line on `sink`, honouring the
/// `partial` delta filter.
///
/// # Errors
///
/// [`PrintError::StreamTorn`] when the write fails for any reason other
/// than a broken pipe.
fn write_stream_event(
    sink: &StreamSink,
    agent_event: &norn::provider::AgentEvent,
    partial: bool,
) -> Result<StreamWrite, PrintError> {
    let Some(line) = agent_event_to_ndjson(agent_event, partial) else {
        // Filtered by the `partial` policy: nothing on the wire for this
        // event, and the stream continues.
        return Ok(StreamWrite::Written);
    };
    sink.write_line(&line)
}

/// Drain and render the events already buffered on `rx` after a
/// shutdown signal. `try_recv` never blocks, so this terminates even
/// while the shared-context sender clone keeps the channel open.
///
/// # Errors
///
/// [`PrintError::StreamTorn`] when a write fails for any reason other
/// than a broken pipe.
fn drain_buffered_events(
    sink: &StreamSink,
    rx: &mut tokio::sync::broadcast::Receiver<norn::provider::AgentEvent>,
    partial: bool,
) -> Result<(), PrintError> {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(agent_event) => {
                if write_stream_event(sink, &agent_event, partial)? == StreamWrite::ReaderGone {
                    return Ok(());
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => return Ok(()),
            Err(TryRecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "stream renderer lagged — {n} events dropped");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use std::io::ErrorKind;

    /// A capturing writer standing in for stdout or the `-o` file.
    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("buffer lock").clone()).expect("utf-8 output")
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that fails every write with a fixed error kind — the
    /// ENOSPC / EIO / broken-pipe conditions the sink must classify.
    struct FailingWriter {
        kind: ErrorKind,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.kind, "simulated write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capturing_sink() -> (StreamSink, SharedBuffer) {
        let buffer = SharedBuffer::new();
        (StreamSink::from_writer(Box::new(buffer.clone())), buffer)
    }

    fn failing_sink(kind: ErrorKind) -> StreamSink {
        StreamSink::from_writer(Box::new(FailingWriter { kind }))
    }

    fn text_event(text: &str) -> norn::provider::AgentEvent {
        norn::provider::AgentEvent {
            agent_id: uuid::Uuid::nil(),
            agent_role: std::sync::Arc::from("root"),
            event: norn::provider::AgentEventKind::Provider(
                norn::provider::events::ProviderEvent::TextComplete {
                    text: text.to_owned(),
                },
            ),
        }
    }

    /// M3: the renderer writes onto the sink it was handed, so `-o PATH`
    /// carries the streamed events and the terminal envelope in the order
    /// they were produced. Pre-fix the renderer hardcoded
    /// `std::io::stdout()` and the file received the envelope alone.
    #[tokio::test]
    async fn renderer_and_terminal_envelope_share_one_sink_in_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("stream.ndjson");
        let sink = StreamSink::to_file(&path).expect("open the output file");

        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let handle = spawn_stream_renderer(&tx, false, sink.clone());
        tx.send(text_event("first")).expect("broadcast event");
        tx.send(text_event("second")).expect("broadcast event");
        handle
            .finish_run::<(), PrintError>(Ok(()))
            .await
            .expect("clean renderer shutdown");

        // The terminal envelope rides the SAME sink, appended after the
        // events instead of truncating the file with a second handle.
        sink.write_with(|writer| writer.write_all(b"{\"type\":\"completed\"}\n"))
            .expect("terminal envelope write");

        let written = std::fs::read_to_string(&path).expect("output file");
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 3, "two events then the envelope: {written:?}");
        assert!(lines[0].contains("first"), "{written:?}");
        assert!(lines[1].contains("second"), "{written:?}");
        assert_eq!(lines[2], "{\"type\":\"completed\"}");
    }

    /// A `-o PATH` that cannot be opened fails LOUDLY with a typed error;
    /// falling back to stdout would silently split the stream the flag
    /// asked to redirect.
    #[test]
    fn output_file_open_failure_is_typed_and_never_falls_back_to_stdout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("missing-directory").join("stream.ndjson");
        let error = StreamSink::to_file(&path).expect_err("a missing parent must fail the open");
        assert!(matches!(&error, PrintError::Io(_)), "error: {error:?}");
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AgentError);
        assert!(
            error.to_string().contains("stream-json output file"),
            "the failure names the surface it is about: {error}"
        );
    }

    /// Silent-truncation regression: a non-pipe write failure (ENOSPC,
    /// EIO, a partial write) must NOT read as clean completion. Pre-fix
    /// `write_stream_event` collapsed every `io::Error` into `false`, the
    /// task returned, and `finish_run` saw `Ok` — exit 0 over truncated
    /// NDJSON.
    #[tokio::test]
    async fn non_pipe_write_failure_tears_the_stream_and_fails_a_successful_run() {
        let sink = failing_sink(ErrorKind::StorageFull);
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let handle = spawn_stream_renderer(&tx, false, sink);
        tx.send(text_event("doomed")).expect("broadcast event");

        let outcome = handle.finish_run::<(), PrintError>(Ok(())).await;
        let Err(error) = outcome else {
            panic!("a torn stream must fail an otherwise successful run");
        };
        assert!(matches!(&error, PrintError::StreamTorn(_)), "{error:?}");
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AgentError);
        assert_eq!(
            error.envelope_class(),
            None,
            "a torn stream never receives a terminal envelope"
        );
        assert!(error.to_string().contains("incomplete"), "{error}");
    }

    /// A non-pipe write failure keeps a primary run failure's exit class
    /// while still suppressing the terminal envelope.
    #[tokio::test]
    async fn write_failure_preserves_a_primary_auth_failure_exit_class() {
        let sink = failing_sink(ErrorKind::Other);
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let handle = spawn_stream_renderer(&tx, false, sink);
        tx.send(text_event("doomed")).expect("broadcast event");

        let outcome = handle
            .finish_run::<(), _>(Err(PrintError::Auth("credential expired".to_owned())))
            .await;
        let Err(error) = outcome else {
            panic!("the primary auth failure must survive");
        };
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AuthError);
        assert_eq!(error.envelope_class(), None);
        let rendered = error.to_string();
        assert!(rendered.contains("credential expired"), "{rendered}");
        assert!(rendered.contains("incomplete"), "{rendered}");
    }

    /// `BrokenPipe` is the conventional quiet stop: the run keeps its own
    /// exit code, and every later write — including the terminal envelope
    /// — is withheld rather than aimed at the dead pipe.
    #[tokio::test]
    async fn broken_pipe_stops_quietly_and_withholds_the_terminal_envelope() {
        let sink = failing_sink(ErrorKind::BrokenPipe);
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let handle = spawn_stream_renderer(&tx, false, sink.clone());
        tx.send(text_event("into the void"))
            .expect("broadcast event");

        let value = handle
            .finish_run::<u8, PrintError>(Ok(7))
            .await
            .expect("a reader that went away is not a run failure");
        assert_eq!(value, 7, "the run result is untouched");

        let mut envelope_written = false;
        let outcome = sink
            .write_with(|_| {
                envelope_written = true;
                Ok(())
            })
            .expect("a closed reader is not an error");
        assert_eq!(outcome, StreamWrite::ReaderGone);
        assert!(
            !envelope_written,
            "no terminal envelope may be aimed at a dead pipe"
        );
    }

    /// The `partial` filter still suppresses delta events without
    /// touching the sink.
    #[tokio::test]
    async fn filtered_delta_events_write_nothing_and_keep_the_stream_open() {
        let (sink, buffer) = capturing_sink();
        let delta = norn::provider::AgentEvent {
            agent_id: uuid::Uuid::nil(),
            agent_role: std::sync::Arc::from("root"),
            event: norn::provider::AgentEventKind::Provider(
                norn::provider::events::ProviderEvent::TextDelta {
                    text: "chunk".to_owned(),
                },
            ),
        };
        let outcome = write_stream_event(&sink, &delta, false).expect("filtered event");
        assert_eq!(outcome, StreamWrite::Written);
        assert_eq!(buffer.text(), "", "a filtered event writes nothing");
    }

    /// REVIEW C1 regression: the renderer must terminate via the
    /// explicit shutdown handle even while an outstanding `Sender`
    /// clone (the registry's `SharedAgentEventChannel` extension in
    /// production) keeps the broadcast channel open. Pre-fix, the task
    /// awaited `RecvError::Closed` forever and this test timed out.
    #[tokio::test]
    async fn renderer_finishes_despite_outstanding_sender_clone() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        // Simulates the SharedAgentEventChannel extension: a clone that
        // outlives the step and is never dropped before the await.
        let registry_clone = tx.clone();

        let (sink, _buffer) = capturing_sink();
        let handle = spawn_stream_renderer(&tx, false, sink);
        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish_task())
            .await
            .expect("renderer must exit via explicit shutdown despite a live sender clone")
            .expect("renderer task must not panic")
            .expect("renderer task must not tear the stream");

        drop(registry_clone);
    }

    /// The private task join retains the panic signal consumed by
    /// `finish_run`; it must never be swallowed as a clean completion.
    #[tokio::test]
    async fn finish_surfaces_renderer_panic_as_join_error() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async {
            panic!("simulated renderer panic");
        });
        let handle = StreamRendererHandle { shutdown, task };
        let err = handle
            .finish_task()
            .await
            .expect_err("finish_task() must report the panicked task");
        assert!(err.is_panic(), "JoinError must carry the panic: {err}");
    }

    /// The legacy termination path still works: with every sender
    /// dropped the channel closes and the task exits without any
    /// shutdown signal (`finish_task()` then joins an already-finished task).
    #[tokio::test]
    async fn renderer_exits_on_channel_closure_without_shutdown_signal() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(16);
        let (sink, _buffer) = capturing_sink();
        let handle = spawn_stream_renderer(&tx, false, sink);
        drop(tx);

        // Give the task a moment to observe Closed, then join via
        // finish_task(); the send side of the shutdown signal failing is
        // tolerated by design.
        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish_task())
            .await
            .expect("renderer must exit when the channel closes")
            .expect("renderer task must not panic")
            .expect("renderer task must not tear the stream");
    }

    #[tokio::test]
    async fn finish_run_preserves_auth_exit_and_suppresses_terminal_envelope() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async {
            panic!("renderer blew up after auth failed");
        });
        let handle = StreamRendererHandle { shutdown, task };
        let outcome = handle
            .finish_run::<(), _>(Err(PrintError::Auth("credential expired".to_owned())))
            .await;
        assert!(outcome.is_err(), "compound auth/renderer failure must fail");
        let Err(error) = outcome else { return };
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AuthError);
        assert_eq!(
            error.envelope_class(),
            None,
            "a torn stream must not receive a terminal auth envelope"
        );
        let rendered = error.to_string();
        assert!(rendered.contains("credential expired"), "error: {rendered}");
        assert!(
            rendered.contains("stream renderer task failed (panic)"),
            "error: {rendered}"
        );
        assert!(rendered.contains("incomplete"), "error: {rendered}");
    }

    #[tokio::test]
    async fn finish_run_renderer_panic_overrides_success() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async {
            panic!("renderer blew up");
        });
        let handle = StreamRendererHandle { shutdown, task };
        let outcome = handle.finish_run::<(), PrintError>(Ok(())).await;
        assert!(
            outcome.is_err(),
            "renderer panic must fail a successful run"
        );
        let Err(error) = outcome else { return };
        assert!(matches!(&error, PrintError::StreamTorn(_)));
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AgentError);
        assert_eq!(error.envelope_class(), None);
        assert!(error.to_string().contains("incomplete"));
    }

    #[tokio::test]
    async fn finish_run_clean_renderer_preserves_primary_result() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async { Ok(()) });
        let handle = StreamRendererHandle { shutdown, task };
        let success = handle.finish_run::<_, PrintError>(Ok(41_u8)).await;
        assert!(matches!(success, Ok(41)));

        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async { Ok(()) });
        let handle = StreamRendererHandle { shutdown, task };
        let failure = handle
            .finish_run::<(), _>(Err(PrintError::Auth("credential expired".to_owned())))
            .await;
        assert!(failure.is_err(), "primary failure must survive shutdown");
        let Err(error) = failure else { return };
        assert!(matches!(&error, PrintError::Auth(_)), "error: {error:?}");
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AuthError);
        assert_eq!(error.envelope_class(), Some("auth"));
    }

    #[tokio::test]
    async fn finish_run_renderer_cancellation_overrides_success() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async { std::future::pending::<Result<(), PrintError>>().await });
        let abort = task.abort_handle();
        let handle = StreamRendererHandle { shutdown, task };
        abort.abort();

        let outcome = handle.finish_run::<(), PrintError>(Ok(())).await;
        assert!(outcome.is_err(), "renderer cancellation must fail the run");
        let Err(error) = outcome else { return };
        let PrintError::StreamTorn(message) = &error else {
            panic!("expected StreamTorn, got {error:?}");
        };
        assert!(message.contains("cancelled"), "message: {message}");
        assert_eq!(error.exit_code(), crate::cli::ExitCode::AgentError);
        assert_eq!(error.envelope_class(), None);
    }
}
