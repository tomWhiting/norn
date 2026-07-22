//! Semantic validation of reserved provider-state timeline records.

use std::collections::HashMap;

use thiserror::Error;

use crate::session::events::{ContextMarkKind, EventId, ProviderEpochBoundaryReason, SessionEvent};
use crate::system_prompt::PromptSeedFingerprint;

use super::{
    PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance, ResponseAudioArtifactLink,
};

mod publication;
use publication::validate_publication_frames;
pub(crate) use publication::{
    is_response_state_publication_boundary, response_publication_group_len,
    seal_response_publication_group, validate_new_response_publication_batches,
    validate_no_incomplete_legacy_response_publications, validate_response_publication_append,
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
    records: HashMap<EventId, ActiveResponseRecord>,
}

#[derive(Clone, Copy)]
struct ActiveResponseRecord {
    disposition: ResponseStateDisposition,
    prompt_seed_fingerprint: Option<PromptSeedFingerprint>,
}

impl ActiveResponseProvenance {
    pub(crate) fn disposition(
        &self,
        assistant_event_id: &EventId,
    ) -> Option<ResponseStateDisposition> {
        self.records
            .get(assistant_event_id)
            .map(|record| record.disposition)
    }

    pub(crate) fn prompt_seed_fingerprint(
        &self,
        assistant_event_id: &EventId,
    ) -> Option<PromptSeedFingerprint> {
        self.records
            .get(assistant_event_id)
            .and_then(|record| record.prompt_seed_fingerprint)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        dispositions: impl IntoIterator<Item = (EventId, ResponseStateDisposition)>,
    ) -> Self {
        Self {
            records: dispositions
                .into_iter()
                .map(|(event_id, disposition)| {
                    (
                        event_id,
                        ActiveResponseRecord {
                            disposition,
                            prompt_seed_fingerprint: None,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// A reserved provider-state record is malformed or internally inconsistent.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ProviderStateValidationError {
    /// A reserved provider-state frame is malformed or internally inconsistent.
    #[error("provider state provenance is invalid")]
    Provenance,
    /// A V1 response group is uncommitted or does not match its durable commitment.
    #[error("provider state publication commitment is invalid")]
    PublicationCommitment,
}

#[derive(Clone, Copy)]
struct ProvenanceRecord {
    event_index: usize,
    stored: bool,
    prompt_seed_fingerprint: Option<PromptSeedFingerprint>,
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
    // An ordinary historical cut starts the active slice, but does not prove
    // that this pre-D3 timeline had begun publishing provenance records.
    let legacy_closed_before_epoch = events[..active_start].windows(2).any(|pair| {
        is_response_state_publication_boundary(&pair[0]) && is_provenance_family(&pair[1])
    }) || events[..active_start].iter().any(|event| {
        matches!(
            event,
            SessionEvent::ProviderEpochBoundary {
                reason: ProviderEpochBoundaryReason::FilteredFork,
                ..
            }
        )
    });
    let starts_after_publication = events
        .get(active_start.saturating_sub(1))
        .is_some_and(is_response_state_publication_boundary);
    discover_epoch(
        &events[active_start..],
        legacy_closed_before_epoch,
        starts_after_publication,
    )
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
            .map_err(|_error| ProviderStateValidationError::Provenance)?;
        let Some(provenance) = provenance else {
            continue;
        };
        first_provenance_index.get_or_insert(event_index);
        let record = ProvenanceRecord {
            event_index,
            stored: provenance.stored(),
            prompt_seed_fingerprint: provenance.prompt_seed_fingerprint(),
        };
        if records
            .insert(provenance.assistant_event_id().clone(), record)
            .is_some()
        {
            return Err(ProviderStateValidationError::Provenance);
        }
    }

    validate_targets(events, &records)?;

    let mut active_records = HashMap::new();
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
        let provenance_record = records.get(&base.id);
        let disposition = provenance_record.map_or_else(
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
        let record = ActiveResponseRecord {
            disposition,
            prompt_seed_fingerprint: provenance_record
                .and_then(|record| record.prompt_seed_fingerprint),
        };
        if active_records.insert(base.id.clone(), record).is_some() {
            return Err(ProviderStateValidationError::Provenance);
        }
    }

    Ok(ActiveResponseProvenance {
        records: active_records,
    })
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
            return Err(ProviderStateValidationError::Provenance);
        }
    }
    for (target, record) in records {
        let Some((target_index, target_event)) = events_by_id.get(target) else {
            return Err(ProviderStateValidationError::Provenance);
        };
        if !valid_target_shape(
            events,
            record.event_index,
            *target_index,
            target_event,
            target,
        )? {
            return Err(ProviderStateValidationError::Provenance);
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
    if target_base.id != *target {
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
        .map_err(|_error| ProviderStateValidationError::Provenance)?
    else {
        return Ok(false);
    };
    Ok(link_event.base().parent_id.as_ref() == Some(provenance_id)
        && link.assistant_event_id() == target
        && link.response_id() == Some(response_id.as_str())
        && target_base.parent_id.as_ref() == Some(&link_event.base().id))
}

#[cfg(test)]
mod tests;
