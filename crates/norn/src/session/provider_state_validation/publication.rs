//! Response-publication frame shape and commitment validation.

use crate::session::events::{ProviderEpochBoundaryReason, SessionEvent};

use super::ProviderStateValidationError;
use super::valid_target_shape;
use crate::session::provider_state_provenance::{
    PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance,
};
use crate::session::response_publication_commitment::{calculate, verify};

const RESPONSE_PUBLICATION_MAX_GROUP_LEN: usize = 4;

pub(crate) fn is_response_state_publication_boundary(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ResponseStatePublication
                | ProviderEpochBoundaryReason::ResponseStatePublicationV1(_),
            ..
        }
    )
}

pub(super) fn validate_publication_frames(
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
    let Some(group_len) = response_publication_group_shape_len(events, boundary_index)? else {
        return Ok(None);
    };
    let group_end = boundary_index
        .checked_add(group_len)
        .ok_or(ProviderStateValidationError::Provenance)?;
    let group = events
        .get(boundary_index..group_end)
        .ok_or(ProviderStateValidationError::Provenance)?;
    validate_response_publication_commitment(group)?;
    Ok(Some(group_len))
}

fn response_publication_group_shape_len(
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
        .ok_or(ProviderStateValidationError::Provenance)?;
    if !is_provenance_family(provenance_event)
        || provenance_event.base().parent_id.as_ref() != Some(&boundary.base().id)
    {
        return Err(ProviderStateValidationError::Provenance);
    }
    let provenance = ProviderStateProvenance::from_event(provenance_event)
        .map_err(|_error| ProviderStateValidationError::Provenance)?
        .ok_or(ProviderStateValidationError::Provenance)?;
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
        .ok_or(ProviderStateValidationError::Provenance)?;
    if valid_target_shape(
        events,
        provenance_index,
        audio_target,
        target_event,
        provenance.assistant_event_id(),
    )? {
        return Ok(Some(4));
    }
    Err(ProviderStateValidationError::Provenance)
}

fn validate_response_publication_commitment(
    group: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    let SessionEvent::ProviderEpochBoundary { base, reason } = &group[0] else {
        return Err(ProviderStateValidationError::Provenance);
    };
    match reason {
        ProviderEpochBoundaryReason::ResponseStatePublication => Ok(()),
        ProviderEpochBoundaryReason::ResponseStatePublicationV1(commitment) => {
            verify(commitment, base, &group[1..])
                .map_err(|_error| ProviderStateValidationError::PublicationCommitment)
        }
        _ => Err(ProviderStateValidationError::Provenance),
    }
}

/// Seal one complete response-publication group with its V1 commitment.
pub(crate) fn seal_response_publication_group(
    group: &mut [SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    let group_len = response_publication_group_shape_len(group, 0)?
        .ok_or(ProviderStateValidationError::Provenance)?;
    if group_len != group.len() {
        return Err(ProviderStateValidationError::Provenance);
    }
    let boundary_base = group[0].base().clone();
    let commitment = calculate(&boundary_base, &group[1..])
        .map_err(|_error| ProviderStateValidationError::PublicationCommitment)?;
    let SessionEvent::ProviderEpochBoundary { reason, .. } = &mut group[0] else {
        return Err(ProviderStateValidationError::Provenance);
    };
    *reason = ProviderEpochBoundaryReason::ResponseStatePublicationV1(commitment);
    Ok(())
}

/// Reject newly appended legacy response frames and validate every V1 group.
pub(crate) fn validate_new_response_publication_batches(
    events: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    let mut index = 0;
    while index < events.len() {
        if !is_response_state_publication_boundary(&events[index]) {
            index = index.saturating_add(1);
            continue;
        }
        if matches!(
            events[index],
            SessionEvent::ProviderEpochBoundary {
                reason: ProviderEpochBoundaryReason::ResponseStatePublication,
                ..
            }
        ) {
            return Err(ProviderStateValidationError::PublicationCommitment);
        }
        let group_len = response_publication_group_len(events, index)?
            .ok_or(ProviderStateValidationError::Provenance)?;
        index = index
            .checked_add(group_len)
            .ok_or(ProviderStateValidationError::Provenance)?;
    }
    Ok(())
}

/// Validate one store transition without allowing row-wise response framing.
pub(crate) fn validate_response_publication_append(
    existing: &[SessionEvent],
    requested: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    validate_new_response_publication_batches(requested)?;
    let trailing_start = existing
        .len()
        .saturating_sub(RESPONSE_PUBLICATION_MAX_GROUP_LEN.saturating_sub(1));
    let Some(relative_boundary_index) = existing[trailing_start..]
        .iter()
        .rposition(is_response_state_publication_boundary)
    else {
        return Ok(());
    };
    let boundary_index = trailing_start.saturating_add(relative_boundary_index);
    if matches!(
        response_publication_group_len(existing, boundary_index),
        Ok(Some(_))
    ) {
        return Ok(());
    }
    if matches!(
        existing[boundary_index],
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
            ..
        }
    ) {
        return Err(ProviderStateValidationError::PublicationCommitment);
    }

    let mut candidate = existing[boundary_index..].to_vec();
    let remaining = RESPONSE_PUBLICATION_MAX_GROUP_LEN.saturating_sub(candidate.len());
    candidate.extend(requested.iter().take(remaining).cloned());
    response_publication_group_len(&candidate, 0)?
        .ok_or(ProviderStateValidationError::Provenance)?;
    Ok(())
}

/// Reject a durable legacy response boundary whose complete group is absent.
pub(crate) fn validate_no_incomplete_legacy_response_publications(
    events: &[SessionEvent],
) -> Result<(), ProviderStateValidationError> {
    for (index, event) in events.iter().enumerate() {
        if matches!(
            event,
            SessionEvent::ProviderEpochBoundary {
                reason: ProviderEpochBoundaryReason::ResponseStatePublication,
                ..
            }
        ) && !matches!(response_publication_group_len(events, index), Ok(Some(_)))
        {
            return Err(ProviderStateValidationError::PublicationCommitment);
        }
    }
    Ok(())
}

fn is_provenance_family(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Custom { event_type, .. }
            if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
    )
}
