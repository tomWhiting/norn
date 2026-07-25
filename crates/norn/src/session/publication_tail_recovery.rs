//! Classification and durable record of a crash-torn publication tail.
//!
//! A response publication is written as one ordered group — the epoch
//! boundary, its `provider.state.provenance` marker, an optional response
//! audio link, and finally the assistant event the marker names. The rows
//! are appended one at a time (see
//! [`JsonlSink::persist_batch`](crate::session::PersistenceSink::persist_batch)),
//! so a writer that dies mid-group — the disk filling with `ENOSPC` is the
//! observed case — can leave a durable strict prefix of that group whose
//! every row is valid JSON but whose provenance marker names an assistant
//! event that never landed.
//!
//! [`validate_provider_state_provenance`] refuses such a timeline, which is
//! correct for corruption in the interior of a history but fatal at its
//! tail: the whole session becomes unresumable because of the last two rows.
//! [`torn_publication_tail`] separates the two cases. It classifies a
//! timeline as tail-torn only when
//!
//! 1. the final response-publication boundary opens an incomplete group,
//! 2. the rows after that boundary are exactly a strict prefix of the group
//!    shape, each linked to its predecessor, naming a target assistant event
//!    that appears nowhere in the timeline, and
//! 3. the history *before* that boundary validates on its own.
//!
//! Anything else — a violation in the interior, a complete group whose
//! commitment does not verify, unrelated rows after the boundary, a
//! "dangling" target that actually exists earlier — is real corruption and
//! keeps the typed hard failure.
//!
//! Recovery itself (quarantining the torn bytes, truncating to the healthy
//! prefix, and appending a [`PublicationTailRecovery`] event) lives in
//! `session::persistence::timeline_tail_recovery`, which owns the file.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::response_audio::ResponseAudioArtifactLink;

use super::provider_state_provenance::{
    PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance,
};
use super::provider_state_validation::{
    is_response_state_publication_boundary, response_publication_group_len,
    validate_provider_state_provenance,
};

/// Custom-event discriminator for a durable publication-tail recovery.
pub const PUBLICATION_TAIL_RECOVERY_EVENT_TYPE: &str = "session.publication_tail.recovery";

const PUBLICATION_TAIL_RECOVERY_VERSION: u32 = 1;

/// Longest incomplete tail a torn publication group can leave.
///
/// A complete group is at most `[boundary, provenance, audio link,
/// assistant]`; a torn one is a strict prefix of that, so at most three rows.
const MAX_INCOMPLETE_TAIL_LEN: usize = 3;

/// Durable record of the rows quarantined out of a crash-torn timeline tail.
///
/// Appended to the healthy prefix so the recovery is visible in the history
/// itself, not only in a log line: the event names every quarantined row, the
/// sidecar file holding their exact bytes, and the assistant event whose
/// absence proved the tear.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PublicationTailRecoveryWire")]
pub struct PublicationTailRecovery {
    version: u32,
    quarantine_file: String,
    quarantined_event_ids: Vec<EventId>,
    quarantined_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    orphaned_assistant_event_id: Option<EventId>,
}

impl PublicationTailRecovery {
    /// Record `quarantined_bytes` of rows preserved in `quarantine_file`.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationTailRecoveryError::Invalid`] when the record
    /// would not describe a concrete quarantine (no rows, no bytes, or no
    /// sidecar name) — an empty recovery record is never written.
    pub(crate) fn new(
        quarantine_file: String,
        quarantined_event_ids: Vec<EventId>,
        quarantined_bytes: u64,
        orphaned_assistant_event_id: Option<EventId>,
    ) -> Result<Self, PublicationTailRecoveryError> {
        let record = Self {
            version: PUBLICATION_TAIL_RECOVERY_VERSION,
            quarantine_file,
            quarantined_event_ids,
            quarantined_bytes,
            orphaned_assistant_event_id,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), PublicationTailRecoveryError> {
        let invalid = |reason: &'static str| PublicationTailRecoveryError::Invalid { reason };
        if self.version != PUBLICATION_TAIL_RECOVERY_VERSION {
            return Err(invalid("unsupported publication tail recovery version"));
        }
        if self.quarantine_file.is_empty() {
            return Err(invalid("a recovery record must name its quarantine file"));
        }
        if self.quarantined_event_ids.is_empty() {
            return Err(invalid("a recovery record must name every quarantined row"));
        }
        if self.quarantined_bytes == 0 {
            return Err(invalid(
                "a recovery record must record the quarantined byte count",
            ));
        }
        Ok(())
    }

    /// Sidecar file holding the exact quarantined bytes.
    #[must_use]
    pub fn quarantine_file(&self) -> &str {
        &self.quarantine_file
    }

    /// Every event id removed from the timeline tail, in file order.
    #[must_use]
    pub fn quarantined_event_ids(&self) -> &[EventId] {
        &self.quarantined_event_ids
    }

    /// Exact number of bytes moved into the quarantine sidecar.
    #[must_use]
    pub const fn quarantined_bytes(&self) -> u64 {
        self.quarantined_bytes
    }

    /// Assistant event the torn provenance marker named and that never landed.
    #[must_use]
    pub const fn orphaned_assistant_event_id(&self) -> Option<&EventId> {
        self.orphaned_assistant_event_id.as_ref()
    }

    /// Wrap the typed payload in the format-2-compatible custom-event shape.
    pub(crate) fn into_custom_event(
        self,
        base: EventBase,
    ) -> Result<SessionEvent, serde_json::Error> {
        Ok(SessionEvent::Custom {
            base,
            event_type: PUBLICATION_TAIL_RECOVERY_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(self)?,
        })
    }

    /// Parse this exact custom-event family and ignore every other event.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationTailRecoveryError::InvalidPayload`] when the row
    /// carries this discriminator with a payload that does not decode.
    pub fn from_event(event: &SessionEvent) -> Result<Option<Self>, PublicationTailRecoveryError> {
        let SessionEvent::Custom {
            event_type, data, ..
        } = event
        else {
            return Ok(None);
        };
        if event_type != PUBLICATION_TAIL_RECOVERY_EVENT_TYPE {
            return Ok(None);
        }
        serde_json::from_value(data.clone())
            .map(Some)
            .map_err(|source| PublicationTailRecoveryError::InvalidPayload { source })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationTailRecoveryWire {
    version: u32,
    quarantine_file: String,
    quarantined_event_ids: Vec<EventId>,
    quarantined_bytes: u64,
    orphaned_assistant_event_id: Option<EventId>,
}

impl TryFrom<PublicationTailRecoveryWire> for PublicationTailRecovery {
    type Error = PublicationTailRecoveryError;

    fn try_from(wire: PublicationTailRecoveryWire) -> Result<Self, Self::Error> {
        let record = Self {
            version: wire.version,
            quarantine_file: wire.quarantine_file,
            quarantined_event_ids: wire.quarantined_event_ids,
            quarantined_bytes: wire.quarantined_bytes,
            orphaned_assistant_event_id: wire.orphaned_assistant_event_id,
        };
        record.validate()?;
        Ok(record)
    }
}

/// A malformed publication-tail recovery record.
#[derive(Debug, Error)]
pub enum PublicationTailRecoveryError {
    /// The record does not describe a concrete quarantine.
    #[error("session.publication_tail.recovery record is invalid: {reason}")]
    Invalid {
        /// Exact invariant that failed.
        reason: &'static str,
    },
    /// The exact typed custom-event payload could not be decoded.
    #[error("session.publication_tail.recovery payload is invalid")]
    InvalidPayload {
        /// Underlying strict payload error.
        #[source]
        source: serde_json::Error,
    },
}

/// One classified crash-torn publication tail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TornPublicationTail {
    /// Index of the boundary that opens the incomplete group. Every event
    /// from here to the end of the timeline is quarantined.
    pub(crate) boundary_index: usize,
    /// Every quarantined event id, in file order.
    pub(crate) quarantined_event_ids: Vec<EventId>,
    /// Assistant event the torn marker named, when the marker landed.
    pub(crate) orphaned_assistant_event_id: Option<EventId>,
}

/// Classify `events` as tail-torn, or refuse to.
///
/// Returns `None` whenever the timeline is healthy or whenever its
/// provenance violation is anything other than an incomplete publication
/// group at the very end — in particular for any violation in the interior,
/// which stays a hard failure.
pub(crate) fn torn_publication_tail(events: &[SessionEvent]) -> Option<TornPublicationTail> {
    let boundary_index = events
        .iter()
        .rposition(is_response_state_publication_boundary)?;
    let tail = events.get(boundary_index..)?;
    if tail.len() > MAX_INCOMPLETE_TAIL_LEN {
        return None;
    }
    if matches!(
        response_publication_group_len(events, boundary_index),
        Ok(Some(_))
    ) {
        // The trailing group is complete; whatever is wrong lies elsewhere.
        return None;
    }
    let shape = torn_tail_shape(tail, events)?;
    // The history before the torn boundary must stand on its own. An
    // interior violation is corruption, not a crash artifact, and is never
    // recovered by truncation.
    validate_provider_state_provenance(events.get(..boundary_index)?).ok()?;
    Some(TornPublicationTail {
        boundary_index,
        quarantined_event_ids: tail.iter().map(|event| event.base().id.clone()).collect(),
        orphaned_assistant_event_id: shape.orphaned_assistant_event_id(),
    })
}

/// How far into its group a torn publication tail got before the writer died.
enum TornTailShape {
    /// Only the boundary landed, so no target was named yet.
    BoundaryOnly,
    /// The provenance marker landed, naming an assistant event that did not.
    OrphanedTarget(EventId),
}

impl TornTailShape {
    fn orphaned_assistant_event_id(self) -> Option<EventId> {
        match self {
            Self::BoundaryOnly => None,
            Self::OrphanedTarget(event_id) => Some(event_id),
        }
    }
}

/// Confirm `tail` is a strict prefix of a publication group whose target
/// assistant event is absent from `events`.
fn torn_tail_shape(tail: &[SessionEvent], events: &[SessionEvent]) -> Option<TornTailShape> {
    let boundary = tail.first()?;
    let Some(marker) = tail.get(1) else {
        return Some(TornTailShape::BoundaryOnly);
    };
    if !is_provenance_family(marker)
        || marker.base().parent_id.as_ref() != Some(&boundary.base().id)
    {
        return None;
    }
    let provenance = ProviderStateProvenance::from_event(marker).ok()??;
    let target = provenance.assistant_event_id();
    if events.iter().any(|event| event.base().id == *target) {
        // The named event exists, so the group is misordered or duplicated
        // rather than truncated. That is corruption.
        return None;
    }
    match tail.get(2) {
        None => Some(TornTailShape::OrphanedTarget(target.clone())),
        Some(link_event) => {
            let link = ResponseAudioArtifactLink::from_event(link_event).ok()??;
            (link_event.base().parent_id.as_ref() == Some(&marker.base().id)
                && link.assistant_event_id() == target)
                .then(|| TornTailShape::OrphanedTarget(target.clone()))
        }
    }
}

fn is_provenance_family(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Custom { event_type, .. }
            if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
