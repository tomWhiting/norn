//! Test fixtures for framed provider-state response publications.

use crate::error::SessionError;

use super::events::{EventBase, EventId, ProviderEpochBoundaryReason, SessionEvent};
use super::{ProviderStateProvenance, seal_response_publication_group};

pub(crate) struct ResponsePublicationFixture {
    pub(crate) boundary: SessionEvent,
    pub(crate) provenance: SessionEvent,
    pub(crate) assistant_base: EventBase,
}

pub(crate) fn response_publication_fixture(
    parent_id: Option<EventId>,
    stored: bool,
) -> Result<ResponsePublicationFixture, serde_json::Error> {
    let boundary_base = EventBase::new(parent_id);
    let provenance_base = EventBase::new(Some(boundary_base.id.clone()));
    let assistant_base = EventBase::new(Some(provenance_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), stored)
        .into_custom_event(provenance_base)?;

    Ok(ResponsePublicationFixture {
        boundary: SessionEvent::ProviderEpochBoundary {
            base: boundary_base,
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
        },
        provenance,
        assistant_base,
    })
}

pub(crate) fn committed_response_publication(
    boundary: SessionEvent,
    provenance: SessionEvent,
    assistant: SessionEvent,
) -> Result<Vec<SessionEvent>, SessionError> {
    let mut group = vec![boundary, provenance, assistant];
    seal_response_publication_group(&mut group).map_err(|_error| SessionError::StorageError {
        reason: "failed to commit the provider-state publication fixture".to_owned(),
    })?;
    Ok(group)
}
