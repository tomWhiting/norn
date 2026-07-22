//! Exact single-event retry for caller-owned stable event IDs.

use crate::error::SessionError;
use crate::session::events::{EventId, SessionEvent};

use super::EventStore;

impl EventStore {
    /// Clone the event currently associated with `id`, when present.
    ///
    /// Stable protocol appenders use this to distinguish an already-consumed
    /// idempotent operation from a fresh one without cloning the full timeline.
    pub(crate) fn event_by_id(&self, id: &EventId) -> Option<SessionEvent> {
        let inner = self.inner.read();
        inner
            .index
            .get(id)
            .map(|position| inner.events[*position].clone())
    }

    /// Append one stable-ID event, adopting an exact existing event and
    /// reconciling one ambiguous sink result before releasing append order.
    ///
    /// This is deliberately crate-private. Stable IDs are persistence
    /// protocol, and ordinary callers must continue using [`Self::append`].
    /// A sink that can return an ambiguous error is already required by
    /// [`super::PersistenceSink`] to reconcile an exact retry. Keeping that
    /// retry beneath the sink mutex prevents another local append from
    /// interposing between the uncertain write and its reconciliation.
    pub(crate) fn append_idempotent(&self, event: SessionEvent) -> Result<EventId, SessionError> {
        let id = event.base().id.clone();
        if let Some(sink) = &self.sink {
            let mut sink = sink.lock();
            let inner = self.inner.read();
            if let Some(position) = inner.index.get(&id).copied() {
                return exact_existing(&inner.events[position], &event, id);
            }
            super::append_batch::validate_response_publication_transition(
                &inner,
                std::slice::from_ref(&event),
            )?;
            drop(inner);

            if let Err(first_error) = sink.persist(&event) {
                tracing::debug!(
                    event_id = %id,
                    %first_error,
                    "stable event append returned an uncertain result; reconciling exact retry",
                );
                sink.persist(&event).map_err(SessionError::from)?;
            }
            self.inner.write().push(id.clone(), event);
            return Ok(id);
        }

        let mut inner = self.inner.write();
        if let Some(position) = inner.index.get(&id).copied() {
            return exact_existing(&inner.events[position], &event, id);
        }
        super::append_batch::validate_response_publication_transition(
            &inner,
            std::slice::from_ref(&event),
        )?;
        inner.push(id.clone(), event);
        Ok(id)
    }
}

fn exact_existing(
    existing: &SessionEvent,
    proposed: &SessionEvent,
    id: EventId,
) -> Result<EventId, SessionError> {
    let existing =
        serde_json::to_vec(existing).map_err(|error| SessionError::EventAppendFailed {
            reason: format!("failed to compare existing stable event {id}: {error}"),
        })?;
    let proposed =
        serde_json::to_vec(proposed).map_err(|error| SessionError::EventAppendFailed {
            reason: format!("failed to compare proposed stable event {id}: {error}"),
        })?;
    if existing == proposed {
        return Ok(id);
    }
    Err(SessionError::EventAppendFailed {
        reason: format!("stable event ID {id} already exists with conflicting content"),
    })
}
