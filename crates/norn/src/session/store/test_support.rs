//! Explicit invalid-store construction for semantic rejection tests.

use crate::error::SessionError;
use crate::session::events::{EventId, SessionEvent};

use super::EventStore;

impl EventStore {
    pub(crate) fn append_unvalidated_for_test(
        &self,
        event: SessionEvent,
    ) -> Result<EventId, SessionError> {
        if self.sink.is_some() {
            return Err(SessionError::EventAppendFailed {
                reason: "invalid-store test fixtures must be sinkless".to_owned(),
            });
        }
        let id = event.base().id.clone();
        let mut inner = self.inner.write();
        if inner.index.contains_key(&id) {
            return Err(SessionError::EventAppendFailed {
                reason: format!("duplicate event ID: {id}"),
            });
        }
        inner.push(id.clone(), event);
        Ok(id)
    }
}
