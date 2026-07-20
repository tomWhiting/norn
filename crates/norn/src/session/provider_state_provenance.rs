//! Durable provider-state provenance records.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::session::events::{EventBase, EventId, SessionEvent};

const PROVIDER_STATE_PROVENANCE_VERSION: u32 = 1;

/// Custom-event discriminator for durable provider-state provenance.
pub const PROVIDER_STATE_PROVENANCE_EVENT_TYPE: &str = "provider.state.provenance";

/// Versioned payload recording whether an assistant response has durable provider state.
///
/// The record precedes its target assistant event. Discovery rejects an orphan,
/// duplicate, or conflicting record rather than guessing whether the target is
/// safe to use for provider-side response threading.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderStateProvenance {
    #[serde(deserialize_with = "deserialize_provenance_version")]
    version: u32,
    #[serde(deserialize_with = "deserialize_canonical_event_id")]
    assistant_event_id: EventId,
    stored: bool,
}

impl ProviderStateProvenance {
    /// Build a provenance record for an already-minted assistant event ID.
    #[must_use]
    pub(crate) const fn new(assistant_event_id: EventId, stored: bool) -> Self {
        Self {
            version: PROVIDER_STATE_PROVENANCE_VERSION,
            assistant_event_id,
            stored,
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

fn deserialize_provenance_version<'de, Deserializer>(
    deserializer: Deserializer,
) -> Result<u32, Deserializer::Error>
where
    Deserializer: serde::Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version != PROVIDER_STATE_PROVENANCE_VERSION {
        return Err(serde::de::Error::custom(
            "unsupported provider-state provenance version",
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn round_trips_exact_versioned_custom_family() -> TestResult {
        for stored in [false, true] {
            let assistant_event_id = EventId::new();
            let event = ProviderStateProvenance::new(assistant_event_id.clone(), stored)
                .into_custom_event(EventBase::new(None))?;

            assert!(matches!(
                &event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE
            ));
            let encoded = serde_json::to_value(&event)?;
            assert_eq!(encoded["type"], json!("Custom"));
            assert_eq!(
                encoded["event_type"],
                json!(PROVIDER_STATE_PROVENANCE_EVENT_TYPE)
            );
            let Some(decoded) = ProviderStateProvenance::from_event(&event)? else {
                return Err(std::io::Error::other("provenance family was not recognized").into());
            };
            assert_eq!(decoded.assistant_event_id(), &assistant_event_id);
            assert_eq!(decoded.stored(), stored);
        }
        Ok(())
    }

    #[test]
    fn rejects_future_versions_and_extended_payloads() {
        for data in [
            json!({
                "version": 2,
                "assistant_event_id": EventId::new(),
                "stored": true,
            }),
            json!({
                "version": 1,
                "assistant_event_id": EventId::new(),
                "stored": true,
                "future": true,
            }),
            json!({"version": 1, "assistant_event_id": EventId::new()}),
        ] {
            let event = SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
                data,
            };
            assert!(matches!(
                ProviderStateProvenance::from_event(&event),
                Err(ProviderStateProvenanceError::InvalidPayload { .. })
            ));
        }
    }

    #[test]
    fn ignores_unrelated_custom_events() -> TestResult {
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: "application.note".to_owned(),
            data: json!({"version": 1}),
        };
        assert!(ProviderStateProvenance::from_event(&event)?.is_none());
        Ok(())
    }
}
