//! Dedicated blocking terminal reader; the async frontend owns every forwarded event.

use termina::{Event, EventReader};
use tokio::sync::mpsc;

/// Spawn the dedicated OS thread that reads terminal events.
///
/// [`EventReader::read`] blocks the calling thread, so it cannot run
/// inside the tokio runtime. The thread forwards each event onto an
/// unbounded mpsc channel; the returned receiver is the single source
/// of terminal events for both the outer loop and the in-flight turn
/// (Ctrl+C interrupt path).
pub(super) fn spawn_event_reader(
    event_reader: EventReader,
) -> mpsc::UnboundedReceiver<std::io::Result<Event>> {
    let (term_tx, term_rx) = mpsc::unbounded_channel::<std::io::Result<Event>>();
    std::thread::spawn(move || {
        loop {
            match event_reader.read(|_| true) {
                Ok(event) => {
                    if term_tx.send(Ok(event)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    if term_tx.send(Err(err)).is_err() {
                        tracing::debug!("terminal error receiver has closed");
                    }
                    break;
                }
            }
        }
    });
    term_rx
}
