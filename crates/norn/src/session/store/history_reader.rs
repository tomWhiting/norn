//! Read-only access to one actual store instance, with explicit history and body demand.

use std::sync::Arc;

use crate::session_view::ViewSource;

use super::{BodyPage, BodyRead, EventStore, HistoryPage, HistoryRead, HistoryReadError};

/// A selected conversation's read capability, sharing its existing event store.
///
/// Cloning this handle neither copies history nor starts an agent. Each demand
/// sees accepted store events at that read's snapshot, including future appends.
/// The source never changes, even when the runtime rotates to another store.
/// No append, provider, session-resume or mutable store capability is exposed.
#[derive(Clone, Debug)]
pub struct SessionHistoryReader {
    store: Arc<EventStore>,
    source: ViewSource,
}

impl EventStore {
    /// Open a read-only handle without reconstructing or rebinding this store.
    ///
    /// # Errors
    /// Refuses stores without an owner binding or with an invalid managed binding.
    pub fn history_reader(self: &Arc<Self>) -> Result<SessionHistoryReader, HistoryReadError> {
        Ok(SessionHistoryReader {
            source: self.bound_history_source()?,
            store: Arc::clone(self),
        })
    }
}

impl SessionHistoryReader {
    /// The actual source bound by the store's producer, never inferred from a label.
    #[must_use]
    pub const fn source(&self) -> &ViewSource {
        &self.source
    }

    /// Read an explicitly sized page through the existing approved projection.
    /// Keep this potentially blocking operation off frontend input/paint paths.
    ///
    /// # Errors
    /// Refuses foreign sources/cursors and invalid owner or event metadata.
    pub fn history_page(&self, read: &HistoryRead) -> Result<HistoryPage, HistoryReadError> {
        self.store.history_page(read)
    }

    /// Read approved original body bytes for this source, using explicit byte demand.
    /// Spool I/O and projection work belong on a blocking worker, never in paint.
    ///
    /// # Errors
    /// Refuses foreign, stale, unapproved or projection-owned body capabilities.
    pub fn read_body(&self, read: &BodyRead) -> Result<BodyPage, HistoryReadError> {
        self.store.read_body(read)
    }
}
