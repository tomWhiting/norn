//! Publication of one provider response as an ordered transcript group.

use crate::error::SessionError;
use crate::integration::hooks::HookRegistry;
use crate::provider::agent_event::PublicationOwner;
use crate::session::events::{EventId, SessionEvent};
use crate::session::store::EventStore;
use crate::session::validate_new_response_publication_batches;

/// Publish the full provider-response group before any observer hook runs.
pub(super) fn append_response_publication(
    store: &EventStore,
    events: &[SessionEvent],
    assistant_id: &EventId,
    observation: Option<PublicationOwner>,
) -> Result<(), SessionError> {
    validate_new_response_publication_batches(events).map_err(|_error| {
        SessionError::StorageError {
            reason: "provider response publication commitment is invalid".to_owned(),
        }
    })?;
    let append = move || {
        let result = store.append_batch(events);
        let observation = match observation {
            Some(owner) => owner
                .appended(store, result.as_ref().map(|_| assistant_id))
                .map_err(SessionError::from),
            None => Ok(()),
        };
        match (result, observation) {
            (Ok(_), result) => result,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(observation)) => {
                tracing::error!(event_id = %assistant_id, error = %observation, "failed response publication observation also failed");
                Err(error)
            }
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(append)
        }
        _ => append(),
    }
}

/// Notify observers only after the complete response group is durable.
pub(super) async fn notify_response_publication(
    events: &[SessionEvent],
    hooks: Option<&HookRegistry>,
) {
    if let Some(registry) = hooks {
        for event in events {
            registry.run_on_event(event).await;
        }
    }
}
