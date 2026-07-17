use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::ResponseAudioArtifactRef;
use crate::session::events::{EventBase, EventId, SessionEvent};

const RESPONSE_AUDIO_ARTIFACT_LINK_VERSION: u32 = 1;
const PARTIAL_OUTPUT_EVENT_TYPE: &str = "loop.partial_output";

/// Custom-event discriminator for a sealed response-audio association.
pub const RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE: &str = "response.audio.artifact";

/// Versioned payload of a [`RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE`] custom event.
///
/// The event is a non-replayable transcript association. Its assistant ID is
/// minted before either event is appended so the link can be made durable
/// first; the assistant event then names the link event as its parent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseAudioArtifactLink {
    #[serde(deserialize_with = "deserialize_artifact_link_version")]
    version: u32,
    #[serde(deserialize_with = "deserialize_canonical_event_id")]
    assistant_event_id: EventId,
    reference: ResponseAudioArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_id: Option<String>,
}

impl ResponseAudioArtifactLink {
    /// Build a link to a sealed artifact and its already-minted assistant ID.
    #[must_use]
    pub(crate) fn new(
        assistant_event_id: EventId,
        reference: ResponseAudioArtifactRef,
        response_id: Option<String>,
    ) -> Self {
        Self {
            version: RESPONSE_AUDIO_ARTIFACT_LINK_VERSION,
            assistant_event_id,
            reference,
            response_id,
        }
    }

    /// Return the assistant event this link precedes.
    #[must_use]
    pub fn assistant_event_id(&self) -> &EventId {
        &self.assistant_event_id
    }

    /// Return the linked sidecar reference.
    #[must_use]
    pub const fn reference(&self) -> ResponseAudioArtifactRef {
        self.reference
    }

    /// Return the terminal provider response ID, when one was supplied.
    #[must_use]
    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// Wrap the typed payload in its exact existing [`SessionEvent::Custom`]
    /// representation.
    pub(crate) fn into_custom_event(
        self,
        base: EventBase,
    ) -> Result<SessionEvent, serde_json::Error> {
        Ok(SessionEvent::Custom {
            base,
            event_type: RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(self)?,
        })
    }

    /// Parse this exact custom-event family, rejecting malformed or extended
    /// payloads while ignoring every other event family.
    pub fn from_event(event: &SessionEvent) -> Result<Option<Self>, ResponseAudioReferenceError> {
        let SessionEvent::Custom {
            event_type, data, ..
        } = event
        else {
            return Ok(None);
        };
        if event_type != RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE {
            return Ok(None);
        }
        serde_json::from_value(data.clone())
            .map(Some)
            .map_err(|source| ResponseAudioReferenceError::InvalidArtifactLink { source })
    }
}

/// A malformed or internally inconsistent response-audio transcript link.
#[derive(Debug, Error)]
pub enum ResponseAudioReferenceError {
    /// The exact typed custom-event payload could not be decoded.
    #[error("response.audio.artifact payload is invalid")]
    InvalidArtifactLink {
        /// Underlying strict payload error.
        #[source]
        source: serde_json::Error,
    },
    /// A hard-cut partial-output record carried an invalid reference.
    #[error("loop.partial_output response-audio reference is invalid")]
    InvalidPartialOutputReference {
        /// Underlying canonical UUID error.
        #[source]
        source: serde_json::Error,
    },
    /// The association was appended after its assistant rather than before it.
    #[error("response.audio.artifact does not precede assistant event {assistant_event_id}")]
    LinkDoesNotPrecedeAssistant {
        /// Misordered event ID.
        assistant_event_id: String,
    },
    /// The assistant did not name the precursor link as its parent.
    #[error("response.audio.artifact is not the parent of assistant event {assistant_event_id}")]
    LinkIsNotAssistantParent {
        /// Disconnected event ID.
        assistant_event_id: String,
    },
    /// More than one association claimed the same assistant event.
    #[error(
        "multiple response.audio.artifact events reference assistant event {assistant_event_id}"
    )]
    DuplicateAssistantLink {
        /// Multiply-linked event ID.
        assistant_event_id: String,
    },
    /// More than one association claimed the same response attempt sidecar.
    #[error("multiple response.audio.artifact events claim sidecar {artifact_id}")]
    DuplicateArtifactLink {
        /// Multiply-linked artifact ID.
        artifact_id: String,
    },
    /// Link and assistant provider response identities disagreed.
    #[error(
        "response.audio.artifact response ID disagrees with assistant event {assistant_event_id}"
    )]
    ResponseIdMismatch {
        /// Inconsistent event ID.
        assistant_event_id: String,
    },
}

/// Parse and validate every typed response-audio artifact link.
///
/// A missing future assistant is an honest crash precursor and remains valid.
/// When the assistant exists, the link must precede it, parent it, and carry
/// the same provider response identity.
pub fn response_audio_artifact_links(
    events: &[SessionEvent],
) -> Result<Vec<ResponseAudioArtifactLink>, ResponseAudioReferenceError> {
    let assistants: HashMap<&str, (usize, &EventBase, &Option<String>)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            SessionEvent::AssistantMessage {
                base, response_id, ..
            } => Some((base.id.as_str(), (index, base, response_id))),
            _ => None,
        })
        .collect();
    let mut linked_assistants = HashSet::new();
    let mut linked_references = HashSet::new();
    let mut links = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let Some(link) = ResponseAudioArtifactLink::from_event(event)? else {
            continue;
        };
        validate_artifact_link(
            event,
            index,
            &link,
            &assistants,
            &mut linked_assistants,
            &mut linked_references,
        )?;
        links.push(link);
    }
    Ok(links)
}

/// Enumerate all canonical response-audio references in a transcript.
///
/// Typed artifact links are checked against any matching assistant event.
/// Hard-cut `loop.partial_output` references are also included, without
/// interpreting the rest of that event's payload.
pub fn referenced_response_audio_artifacts(
    events: &[SessionEvent],
) -> Result<Vec<ResponseAudioArtifactRef>, ResponseAudioReferenceError> {
    let mut references = BTreeMap::new();
    for link in response_audio_artifact_links(events)? {
        references.insert(link.reference().file_name(), link.reference());
    }
    for event in events {
        if let Some(reference) = partial_output_reference(event)? {
            references.insert(reference.file_name(), reference);
        }
    }
    Ok(references.into_values().collect())
}

fn validate_artifact_link(
    event: &SessionEvent,
    link_index: usize,
    link: &ResponseAudioArtifactLink,
    assistants: &HashMap<&str, (usize, &EventBase, &Option<String>)>,
    linked_assistants: &mut HashSet<String>,
    linked_references: &mut HashSet<ResponseAudioArtifactRef>,
) -> Result<(), ResponseAudioReferenceError> {
    let assistant_id = link.assistant_event_id().as_str();
    if !linked_assistants.insert(assistant_id.to_owned()) {
        return Err(ResponseAudioReferenceError::DuplicateAssistantLink {
            assistant_event_id: assistant_id.to_owned(),
        });
    }
    if !linked_references.insert(link.reference()) {
        return Err(ResponseAudioReferenceError::DuplicateArtifactLink {
            artifact_id: link.reference().to_string(),
        });
    }
    let Some((assistant_index, assistant_base, assistant_response_id)) =
        assistants.get(assistant_id)
    else {
        return Ok(());
    };
    if *assistant_index <= link_index {
        return Err(ResponseAudioReferenceError::LinkDoesNotPrecedeAssistant {
            assistant_event_id: assistant_id.to_owned(),
        });
    }
    if assistant_base.parent_id.as_ref() != Some(&event.base().id) {
        return Err(ResponseAudioReferenceError::LinkIsNotAssistantParent {
            assistant_event_id: assistant_id.to_owned(),
        });
    }
    if assistant_response_id.as_deref() != link.response_id() {
        return Err(ResponseAudioReferenceError::ResponseIdMismatch {
            assistant_event_id: assistant_id.to_owned(),
        });
    }
    Ok(())
}

fn partial_output_reference(
    event: &SessionEvent,
) -> Result<Option<ResponseAudioArtifactRef>, ResponseAudioReferenceError> {
    let SessionEvent::Custom {
        event_type, data, ..
    } = event
    else {
        return Ok(None);
    };
    if event_type != PARTIAL_OUTPUT_EVENT_TYPE {
        return Ok(None);
    }
    let Some(value) = data.get("response_audio") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|source| ResponseAudioReferenceError::InvalidPartialOutputReference { source })
}

fn deserialize_artifact_link_version<'de, Deserializer>(
    deserializer: Deserializer,
) -> Result<u32, Deserializer::Error>
where
    Deserializer: serde::Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version != RESPONSE_AUDIO_ARTIFACT_LINK_VERSION {
        return Err(serde::de::Error::custom(
            "unsupported response-audio artifact link version",
        ));
    }
    Ok(version)
}

fn deserialize_canonical_event_id<'de, Deserializer>(
    deserializer: Deserializer,
) -> Result<EventId, Deserializer::Error>
where
    Deserializer: serde::Deserializer<'de>,
{
    let event_id = EventId::deserialize(deserializer)?;
    let parsed = Uuid::parse_str(event_id.as_str()).map_err(serde::de::Error::custom)?;
    if event_id.as_str() != parsed.hyphenated().to_string() {
        return Err(serde::de::Error::custom(
            "assistant event ID must use canonical UUID text",
        ));
    }
    Ok(event_id)
}
