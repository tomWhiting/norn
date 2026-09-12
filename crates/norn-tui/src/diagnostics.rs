//! Process diagnostic transport; local notices never become model or channel messages.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

/// One complete formatted tracing event, sequenced by the process writer.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Monotonic sequence within this terminal capture.
    pub sequence: u64,
    /// Actual tracing severity.
    pub level: tracing::Level,
    /// Actual emitting module/target.
    pub target: String,
    /// Full plain-text diagnostic, retained as expandable local detail.
    pub text: Arc<str>,
}

/// Receiver with an acknowledgement frontier owned by the UI.
pub struct DiagnosticReceiver {
    receiver: broadcast::Receiver<Diagnostic>,
    acknowledged: Arc<AtomicU64>,
}

impl DiagnosticReceiver {
    /// Bind a receiver and its exit-drain acknowledgement frontier.
    pub fn new(receiver: broadcast::Receiver<Diagnostic>, acknowledged: Arc<AtomicU64>) -> Self {
        Self {
            receiver,
            acknowledged,
        }
    }

    /// Wait without polling; cancellation does not consume the next event.
    pub async fn recv(&mut self) -> Result<Diagnostic, broadcast::error::RecvError> {
        self.receiver.recv().await
    }

    /// Advance only after the UI has retained the diagnostic and any preceding gap notice.
    pub fn acknowledge(&self, sequence: u64) {
        self.acknowledged.fetch_max(sequence, Ordering::Release);
    }
}
