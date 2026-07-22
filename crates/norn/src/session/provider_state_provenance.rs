//! Durable provider-state provenance records.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::system_prompt::PromptSeedFingerprint;

const LEGACY_PROVIDER_STATE_PROVENANCE_VERSION: u32 = 1;
const PROMPT_SEED_PROVIDER_STATE_PROVENANCE_VERSION: u32 = 2;

/// Custom-event discriminator for durable provider-state provenance.
pub const PROVIDER_STATE_PROVENANCE_EVENT_TYPE: &str = "provider.state.provenance";

/// Versioned payload recording whether an assistant response has durable provider state.
///
/// The record precedes its target assistant event. Discovery rejects an orphan,
/// duplicate, or conflicting record rather than guessing whether the target is
/// safe to use for provider-side response threading.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProviderStateProvenanceWire")]
pub struct ProviderStateProvenance {
    version: u32,
    assistant_event_id: EventId,
    stored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_seed_sha256: Option<PromptSeedFingerprint>,
}

impl ProviderStateProvenance {
    /// Build a provenance record for an already-minted assistant event ID.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn new(assistant_event_id: EventId, stored: bool) -> Self {
        Self {
            version: LEGACY_PROVIDER_STATE_PROVENANCE_VERSION,
            assistant_event_id,
            stored,
            prompt_seed_sha256: None,
        }
    }

    /// Build current provenance bound to the exact non-System prompt seed.
    #[must_use]
    pub(crate) const fn with_prompt_seed(
        assistant_event_id: EventId,
        stored: bool,
        prompt_seed_sha256: PromptSeedFingerprint,
    ) -> Self {
        Self {
            version: PROMPT_SEED_PROVIDER_STATE_PROVENANCE_VERSION,
            assistant_event_id,
            stored,
            prompt_seed_sha256: Some(prompt_seed_sha256),
        }
    }

    /// Return the assistant event this record describes.
    #[must_use]
    pub fn assistant_event_id(&self) -> &EventId {
        &self.assistant_event_id
    }

    /// Whether the provider durably stored the target response.
    #[must_use]
    pub const fn stored(&self) -> bool {
        self.stored
    }

    /// Prompt seed that was present on the request producing this response.
    ///
    /// `None` identifies a readable pre-D8 V1 record whose seed is unbound.
    #[must_use]
    pub const fn prompt_seed_fingerprint(&self) -> Option<PromptSeedFingerprint> {
        self.prompt_seed_sha256
    }

    /// Wrap the typed payload in the format-2-compatible custom-event shape.
    pub(crate) fn into_custom_event(
        self,
        base: EventBase,
    ) -> Result<SessionEvent, serde_json::Error> {
        Ok(SessionEvent::Custom {
            base,
            event_type: PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(self)?,
        })
    }

    /// Parse this exact custom-event family and ignore every other event.
    pub(crate) fn from_event(
        event: &SessionEvent,
    ) -> Result<Option<Self>, ProviderStateProvenanceError> {
        let SessionEvent::Custom {
            event_type, data, ..
        } = event
        else {
            return Ok(None);
        };
        if event_type != PROVIDER_STATE_PROVENANCE_EVENT_TYPE {
            return Ok(None);
        }
        serde_json::from_value(data.clone())
            .map(Some)
            .map_err(|source| ProviderStateProvenanceError::InvalidPayload { source })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderStateProvenanceWire {
    version: u32,
    #[serde(deserialize_with = "deserialize_canonical_event_id")]
    assistant_event_id: EventId,
    stored: bool,
    prompt_seed_sha256: Option<PromptSeedFingerprint>,
}

impl TryFrom<ProviderStateProvenanceWire> for ProviderStateProvenance {
    type Error = &'static str;

    fn try_from(wire: ProviderStateProvenanceWire) -> Result<Self, Self::Error> {
        match (wire.version, wire.prompt_seed_sha256) {
            (LEGACY_PROVIDER_STATE_PROVENANCE_VERSION, None) => Ok(Self {
                version: LEGACY_PROVIDER_STATE_PROVENANCE_VERSION,
                assistant_event_id: wire.assistant_event_id,
                stored: wire.stored,
                prompt_seed_sha256: None,
            }),
            (PROMPT_SEED_PROVIDER_STATE_PROVENANCE_VERSION, Some(prompt_seed)) => Ok(
                Self::with_prompt_seed(wire.assistant_event_id, wire.stored, prompt_seed),
            ),
            (LEGACY_PROVIDER_STATE_PROVENANCE_VERSION, Some(_)) => {
                Err("provider-state provenance V1 cannot carry a prompt seed")
            }
            (PROMPT_SEED_PROVIDER_STATE_PROVENANCE_VERSION, None) => {
                Err("provider-state provenance V2 requires a prompt seed")
            }
            _ => Err("unsupported provider-state provenance version"),
        }
    }
}

/// A malformed provider-state provenance payload.
#[derive(Debug, Error)]
pub enum ProviderStateProvenanceError {
    /// The exact typed custom-event payload could not be decoded.
    #[error("provider.state.provenance payload is invalid")]
    InvalidPayload {
        /// Underlying strict payload error.
        #[source]
        source: serde_json::Error,
    },
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

#[cfg(test)]
mod tests;
