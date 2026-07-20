//! Publication of one provider response as an ordered transcript group.

use crate::error::SessionError;
use crate::integration::hooks::HookRegistry;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Publish the full provider-response group before any observer hook runs.
pub(super) fn append_response_publication(
    store: &EventStore,
    events: &[SessionEvent],
) -> Result<(), SessionError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| store.append_batch(events))?;
        }
        _ => {
            store.append_batch(events)?;
        }
    }
    Ok(())
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
