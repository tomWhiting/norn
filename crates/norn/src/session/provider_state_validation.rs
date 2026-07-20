//! Semantic validation of reserved provider-state timeline records.

use std::collections::HashMap;

use thiserror::Error;

use crate::session::events::{ContextMarkKind, EventId, ProviderEpochBoundaryReason, SessionEvent};

use super::{
    PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance, ResponseAudioArtifactLink,
};

/// Provider-state disposition of one response-bearing assistant event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseStateDisposition {
    /// No provenance records existed yet, so the response may be pre-D3.
    Legacy,
    /// The provider durably stored this response.
    Stored,
    /// Provenance explicitly records that the response was not stored.
    NotStored,
    /// The response is unmarked despite occurring after the provenance era began.
    UnmarkedAfterProvenance,
}

/// Validated dispositions for response-bearing assistants in the active epoch.
pub(crate) struct ActiveResponseProvenance {
    dispositions: HashMap<EventId, ResponseStateDisposition>,
}

impl ActiveResponseProvenance {
    pub(crate) fn disposition(
        &self,
        assistant_event_id: &EventId,
    ) -> Option<ResponseStateDisposition> {
        self.dispositions.get(assistant_event_id).copied()
    }
}

/// A reserved provider-state record is malformed or internally inconsistent.
#[derive(Clone, Copy, Debug, Error)]
#[error("provider state provenance is invalid")]
pub struct ProviderStateValidationError;

#[derive(Clone, Copy)]
struct ProvenanceRecord {
    event_index: usize,
    stored: bool,
}

/// Validate every reserved provider-state record in a complete timeline.
pub(crate) fn validate_provider_state_provenance(
    events: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    discover_active_response_provenance(events)?;
    Ok(())
}

/// Validate and classify provenance in the latest provider-state epoch.
pub(crate) fn discover_active_response_provenance(
    events: &[SessionEvent],
) -> Result<ActiveResponseProvenance, ProviderStateValidationError> {
    validate_publication_frames(events)?;
    let global = discover_epoch(events, false, false)?;
    let active_start = events
        .iter()
        .rposition(event_cuts_response_anchor)
        .map_or(0, |boundary| boundary.saturating_add(1));
    if active_start == 0 {
        return Ok(global);
    }
    let starts_after_publication = events
        .get(active_start.saturating_sub(1))
        .is_some_and(is_response_state_publication_boundary);
    discover_epoch(&events[active_start..], true, starts_after_publication)
}

pub(crate) fn event_cuts_response_anchor(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary { .. }
            | SessionEvent::Compaction { .. }
            | SessionEvent::ContextMark {
                mark: ContextMarkKind::Suppress,
                ..
            }
    )
}

pub(crate) fn is_response_state_publication_boundary(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
            ..
        }
    )
}

fn validate_publication_frames(
    events: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    for boundary_index in 0..events.len() {
        if is_response_state_publication_boundary(&events[boundary_index]) {
            response_publication_group_len(events, boundary_index)?;
        }
    }
    Ok(())
}

/// Return the complete framed response-group length at `boundary_index`.
pub(crate) fn response_publication_group_len(
    events: &[SessionEvent],
    boundary_index: usize,
) -> Result<Option<usize>, ProviderStateValidationError> {
    let Some(boundary) = events.get(boundary_index) else {
        return Ok(None);
    };
    if !is_response_state_publication_boundary(boundary) {
        return Ok(None);
    }
    let provenance_index = boundary_index.saturating_add(1);
    let provenance_event = events
        .get(provenance_index)
        .ok_or(ProviderStateValidationError)?;
    if !is_provenance_family(provenance_event)
        || provenance_event.base().parent_id.as_ref() != Some(&boundary.base().id)
    {
        return Err(ProviderStateValidationError);
    }
    let provenance = ProviderStateProvenance::from_event(provenance_event)
        .map_err(|_error| ProviderStateValidationError)?
        .ok_or(ProviderStateValidationError)?;
    let direct_target = boundary_index.saturating_add(2);
    if let Some(target_event) = events.get(direct_target)
        && valid_target_shape(
            events,
            provenance_index,
            direct_target,
            target_event,
            provenance.assistant_event_id(),
        )?
    {
        return Ok(Some(3));
    }
    let audio_target = boundary_index.saturating_add(3);
    let target_event = events
        .get(audio_target)
        .ok_or(ProviderStateValidationError)?;
    if valid_target_shape(
        events,
        provenance_index,
        audio_target,
        target_event,
        provenance.assistant_event_id(),
    )? {
        return Ok(Some(4));
    }
    Err(ProviderStateValidationError)
}

fn discover_epoch(
    events: &[SessionEvent],
    legacy_closed_before_epoch: bool,
    starts_after_publication: bool,
) -> Result<ActiveResponseProvenance, ProviderStateValidationError> {
    let mut first_provenance_index = None;
    let mut records = HashMap::new();

    for (event_index, event) in events.iter().enumerate() {
        let framed = (event_index == 0 && starts_after_publication)
            || event_index
                .checked_sub(1)
                .and_then(|previous| events.get(previous))
                .is_some_and(is_response_state_publication_boundary);
        if !framed || !is_provenance_family(event) {
            continue;
        }
        let provenance = ProviderStateProvenance::from_event(event)
            .map_err(|_error| ProviderStateValidationError)?;
        let Some(provenance) = provenance else {
            continue;
        };
        first_provenance_index.get_or_insert(event_index);
        let record = ProvenanceRecord {
            event_index,
            stored: provenance.stored(),
        };
        if records
            .insert(provenance.assistant_event_id().clone(), record)
            .is_some()
        {
            return Err(ProviderStateValidationError);
        }
    }

    validate_targets(events, &records)?;

    let mut dispositions = HashMap::new();
    for (event_index, event) in events.iter().enumerate() {
        let SessionEvent::AssistantMessage {
            base, response_id, ..
        } = event
        else {
            continue;
        };
        if response_id.as_ref().is_none_or(String::is_empty) {
            continue;
        }
        let disposition = records.get(&base.id).map_or_else(
            || {
                if !legacy_closed_before_epoch
                    && first_provenance_index.is_none_or(|first| event_index < first)
                {
                    ResponseStateDisposition::Legacy
                } else {
                    ResponseStateDisposition::UnmarkedAfterProvenance
                }
            },
            |record| {
                if record.stored {
                    ResponseStateDisposition::Stored
                } else {
                    ResponseStateDisposition::NotStored
                }
            },
        );
        if dispositions.insert(base.id.clone(), disposition).is_some() {
            return Err(ProviderStateValidationError);
        }
    }

    Ok(ActiveResponseProvenance { dispositions })
}

fn is_provenance_family(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Custom { event_type, .. }
            if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
    )
}

fn validate_targets(
    events: &[SessionEvent],
    records: &HashMap<EventId, ProvenanceRecord>,
) -> Result<(), ProviderStateValidationError> {
    let mut events_by_id = HashMap::with_capacity(events.len());
    for (event_index, event) in events.iter().enumerate() {
        let event_id = event.base().id.clone();
        if events_by_id
            .insert(event_id.clone(), (event_index, event))
            .is_some()
            && records.contains_key(&event_id)
        {
            return Err(ProviderStateValidationError);
        }
    }
    for (target, record) in records {
        let Some((target_index, target_event)) = events_by_id.get(target) else {
            return Err(ProviderStateValidationError);
        };
        if !valid_target_shape(
            events,
            record.event_index,
            *target_index,
            target_event,
            target,
        )? {
            return Err(ProviderStateValidationError);
        }
    }
    Ok(())
}

fn valid_target_shape(
    events: &[SessionEvent],
    record_index: usize,
    target_index: usize,
    target_event: &SessionEvent,
    target: &EventId,
) -> Result<bool, ProviderStateValidationError> {
    let SessionEvent::AssistantMessage {
        base: target_base,
        response_id: Some(response_id),
        ..
    } = target_event
    else {
        return Ok(false);
    };
    if response_id.is_empty() {
        return Ok(false);
    }

    let provenance_id = &events[record_index].base().id;
    if target_index == record_index.saturating_add(1) {
        return Ok(target_base.parent_id.as_ref() == Some(provenance_id));
    }
    if target_index != record_index.saturating_add(2) {
        return Ok(false);
    }

    let link_event = &events[record_index.saturating_add(1)];
    let Some(link) = ResponseAudioArtifactLink::from_event(link_event)
        .map_err(|_error| ProviderStateValidationError)?
    else {
        return Ok(false);
    };
    Ok(link_event.base().parent_id.as_ref() == Some(provenance_id)
        && link.assistant_event_id() == target
        && link.response_id() == Some(response_id.as_str())
        && target_base.parent_id.as_ref() == Some(&link_event.base().id))
}
