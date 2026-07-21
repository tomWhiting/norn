//! Non-interleaving event-group publication.

use std::collections::HashSet;

use crate::error::SessionError;
use crate::session::events::{EventId, SessionEvent};

use super::{EventStore, StoreInner};

impl EventStore {
    /// Append one ordered group without another writer inserting a row between
    /// its members.
    ///
    /// Sink-backed stores delegate the external transaction to
    /// [`PersistenceSink::persist_batch`](super::PersistenceSink::persist_batch)
    /// and publish the group to memory only after the complete sink operation
    /// succeeds. An I/O failure may leave an exact durable prefix on disk, but
    /// never a partially visible in-memory group. The exact group can be
    /// retried safely; reopening an incomplete semantic prefix fails closed
    /// until that recovery is completed.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] before publication when an
    /// ID duplicates the store or another member of the group. Persistence
    /// failures retain the same prefix semantics as [`Self::append`].
    pub(crate) fn append_batch(
        &self,
        events: &[SessionEvent],
    ) -> Result<Vec<EventId>, SessionError> {
        crate::session::validate_new_response_publication_batches(events).map_err(|_error| {
            SessionError::EventAppendFailed {
                reason: "response publication commitment is invalid".to_owned(),
            }
        })?;
        let ids = event_ids(events);
        if let Some(sink) = &self.sink {
            let mut sink = sink.lock();
            validate_ids(&self.inner.read(), &ids)?;
            sink.persist_batch(events).map_err(SessionError::from)?;
            let mut inner = self.inner.write();
            for (event, id) in events.iter().zip(&ids) {
                inner.push(id.clone(), event.clone());
            }
        } else {
            let mut inner = self.inner.write();
            validate_ids(&inner, &ids)?;
            for (event, id) in events.iter().zip(&ids) {
                inner.push(id.clone(), event.clone());
            }
        }
        Ok(ids)
    }
}

fn event_ids(events: &[SessionEvent]) -> Vec<EventId> {
    events.iter().map(|event| event.base().id.clone()).collect()
}

fn validate_ids(inner: &StoreInner, ids: &[EventId]) -> Result<(), SessionError> {
    let mut batch = HashSet::with_capacity(ids.len());
    for id in ids {
        if inner.index.contains_key(id) || !batch.insert(id) {
            return Err(SessionError::EventAppendFailed {
                reason: format!("duplicate event ID: {id}"),
            });
        }
    }
    Ok(())
}
