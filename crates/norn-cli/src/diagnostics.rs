//! Process-wide tracing route, scoped to terminal ownership with an explicit exit drain.

use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use norn_tui::diagnostics::{Diagnostic, DiagnosticReceiver};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing_subscriber::fmt::MakeWriter;

static ROUTER: OnceLock<Router> = OnceLock::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Only a successful global subscriber install grants terminal capture authority.
pub(crate) fn installed() {
    INSTALLED.store(true, Ordering::Release);
}

#[derive(Clone, Default)]
pub(crate) struct Router(Arc<Mutex<Option<Capture>>>);

struct Capture {
    sender: broadcast::Sender<Diagnostic>,
    // Kept under the same owner lock: a writer cannot publish to a closed receiver.
    keeper: broadcast::Receiver<Diagnostic>,
    acknowledged: Arc<AtomicU64>,
    sequence: u64,
}

pub(crate) fn writer() -> Router {
    ROUTER.get_or_init(Router::default).clone()
}

/// Capture before terminal entry; finish after `run_app` restores the terminal.
pub(crate) fn begin(capacity: NonZeroUsize) -> io::Result<(CaptureGuard, DiagnosticReceiver)> {
    if !INSTALLED.load(Ordering::Acquire) {
        let won = crate::print::ensure_stderr_tracing();
        if !won && !INSTALLED.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "TUI diagnostics require Norn's tracing router; another subscriber owns this process",
            ));
        }
    }
    writer().begin(capacity)
}

impl Router {
    fn begin(&self, capacity: NonZeroUsize) -> io::Result<(CaptureGuard, DiagnosticReceiver)> {
        // Tokio rounds to a power of two and requires this representable bound.
        if capacity.get() > usize::MAX >> 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "tui.diagnostics.capacity {} exceeds the broadcast representation",
                    capacity.get()
                ),
            ));
        }
        let mut route = self.0.lock();
        if route.is_some() {
            return Err(io::Error::other(
                "another TUI already owns process diagnostics",
            ));
        }
        let (sender, keeper) = broadcast::channel(capacity.get());
        let acknowledged = Arc::new(AtomicU64::new(0));
        let receiver = DiagnosticReceiver::new(sender.subscribe(), Arc::clone(&acknowledged));
        *route = Some(Capture {
            sender,
            keeper,
            acknowledged,
            sequence: 0,
        });
        Ok((
            CaptureGuard {
                router: Some(self.clone()),
            },
            receiver,
        ))
    }

    fn write(
        &self,
        level: tracing::Level,
        target: &str,
        bytes: &[u8],
        stderr: &mut impl Write,
    ) -> io::Result<usize> {
        let mut route = self.0.lock();
        if let Some(capture) = route.as_mut() {
            let text = std::str::from_utf8(bytes)
                .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
            let sequence = capture
                .sequence
                .checked_add(1)
                .ok_or_else(|| io::Error::other("TUI diagnostic sequence exhausted"))?;
            capture
                .sender
                .send(Diagnostic {
                    sequence,
                    level,
                    target: target.to_owned(),
                    text: Arc::from(text),
                })
                .map_err(|source| io::Error::new(io::ErrorKind::BrokenPipe, source))?;
            capture.sequence = sequence;
        } else {
            stderr.write_all(bytes)?;
        }
        Ok(bytes.len())
    }
}

/// Scope guard lives outside `run_app`, hence outlives its terminal guard on every return.
pub(crate) struct CaptureGuard {
    router: Option<Router>,
}

impl CaptureGuard {
    /// Report unretained events after terminal restoration. Propagate stderr failures.
    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.drain(&mut io::stderr().lock())
    }

    fn drain(&mut self, stderr: &mut impl Write) -> io::Result<()> {
        // Retire this guard before releasing ownership: its Drop must never drain
        // a later TUI capture that begins between finish and destruction.
        let Some(router) = self.router.take() else {
            return Ok(());
        };
        let mut route = router.0.lock();
        let Some(mut capture) = route.take() else {
            return Ok(());
        };
        let mut seen = capture.acknowledged.load(Ordering::Acquire);
        loop {
            match capture.keeper.try_recv() {
                Ok(event) if event.sequence > seen => {
                    let missing = event.sequence - seen - 1;
                    if missing > 0 {
                        writeln!(
                            stderr,
                            "Norn diagnostics: {missing} pending events overwritten before display"
                        )?;
                    }
                    stderr.write_all(event.text.as_bytes())?;
                    seen = event.sequence;
                }
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => break,
            }
        }
        stderr.flush()
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        // Unwinding must restore routing too. Normal paths call finish and propagate I/O errors.
        if let Err(error) = self.drain(&mut io::stderr().lock()) {
            eprintln!("Norn could not drain terminal diagnostics: {error}");
        }
    }
}

pub(crate) struct EventWriter {
    router: Router,
    level: tracing::Level,
    target: String,
}

impl<'writer> MakeWriter<'writer> for Router {
    type Writer = EventWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        EventWriter {
            router: self.clone(),
            level: tracing::Level::WARN,
            target: "tracing".to_owned(),
        }
    }

    fn make_writer_for(&'writer self, metadata: &tracing::Metadata<'_>) -> Self::Writer {
        EventWriter {
            router: self.clone(),
            level: *metadata.level(),
            target: metadata.target().to_owned(),
        }
    }
}

impl Write for EventWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // tracing-subscriber formats a complete event before calling write_all once.
        self.router
            .write(self.level, &self.target, bytes, &mut io::stderr().lock())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
