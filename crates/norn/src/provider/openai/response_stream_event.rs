//! Lossless envelope for one Responses stream event.
//!
//! The public Responses schema requires every event to carry a nonnegative
//! integer `sequence_number`. The pinned Codex-only event overlays are kept
//! separate: their consumer structs do not require a sequence,
//! `response.metadata` fixtures use both shapes, and `codex.rate_limits`
//! fixtures omit it. Their sequence is therefore optional but must still be a
//! nonnegative integer when present. Future event types retain their complete
//! JSON without an invented missing-sequence rule, while still validating a
//! sequence when one is present. This module validates only that common
//! envelope contract for later identity-keyed reconciliation.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use super::response_contract::{
    self, CodexOverlayEntry, CodexOverlayKind, StreamEventEntry, StreamEventStage,
};

/// Whether the event's contract requires a sequence number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseStreamSequencePolicy {
    /// Public Responses events require `sequence_number`.
    Required,
    /// Pinned Codex event overlays permit it to be absent.
    Optional,
    /// Future events have no locally invented sequence requirement.
    Unspecified,
}

/// Manifest classification assigned to a structurally valid stream event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseStreamEventManifest {
    /// One of the 53 public Responses streaming event variants.
    Public(&'static StreamEventEntry),
    /// A Codex-only event kept outside the public event taxonomy.
    CodexOverlay(&'static CodexOverlayEntry),
    /// A structurally valid future event absent from both pinned manifests.
    Unknown,
}

impl ResponseStreamEventManifest {
    /// Return the exact discriminator recorded by a pinned manifest entry.
    ///
    /// Unknown event discriminators remain available through
    /// [`ResponseStreamEvent::event_type`] without entering diagnostics.
    #[must_use]
    pub const fn known_event_type(self) -> Option<&'static str> {
        match self {
            Self::Public(entry) => Some(entry.name()),
            Self::CodexOverlay(entry) => Some(entry.name()),
            Self::Unknown => None,
        }
    }

    /// Return the public processing stage, if this is a public event.
    ///
    /// Codex overlays intentionally have no invented public lifecycle stage.
    #[must_use]
    pub const fn stage(self) -> Option<StreamEventStage> {
        match self {
            Self::Public(entry) => Some(entry.stage()),
            Self::CodexOverlay(_) | Self::Unknown => None,
        }
    }

    /// Return the sequence-number policy for this manifest family.
    #[must_use]
    pub const fn sequence_policy(self) -> ResponseStreamSequencePolicy {
        match self {
            Self::Public(_) => ResponseStreamSequencePolicy::Required,
            Self::CodexOverlay(_) => ResponseStreamSequencePolicy::Optional,
            Self::Unknown => ResponseStreamSequencePolicy::Unspecified,
        }
    }
}

/// A validated stream event retaining its admitted JSON object exactly.
///
/// Transport adapters may redact reusable credentials before constructing the
/// envelope; all remaining provider fields are retained without normalization.
#[derive(Clone, PartialEq)]
pub struct ResponseStreamEvent {
    raw: Value,
    event_type: String,
    manifest: ResponseStreamEventManifest,
    sequence_number: Option<u64>,
}

impl fmt::Debug for ResponseStreamEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseStreamEvent")
            .field("event_type", &"[RETAINED]")
            .field("manifest", &self.manifest)
            .field("sequence_number", &self.sequence_number)
            .field("raw", &"[RETAINED]")
            .finish()
    }
}

impl ResponseStreamEvent {
    /// Parse one SSE payload and bind it to the frame's exact `event:` name.
    ///
    /// The Responses protocol carries the discriminator twice: in the SSE
    /// frame and in the JSON payload. Accepting a disagreement would let
    /// transport dispatch and payload reconciliation classify the same frame
    /// differently. A raw-first observer can instead call [`Self::from_raw`],
    /// emit the envelope, then call [`Self::validate_sse_event_name`].
    pub fn from_sse(sse_event_name: &str, raw: Value) -> Result<Self, ResponseStreamEventError> {
        let event = Self::from_raw(raw)?;
        event.validate_sse_event_name(sse_event_name)?;
        Ok(event)
    }

    /// Parse and validate one provider stream-event object.
    pub fn from_raw(raw: Value) -> Result<Self, ResponseStreamEventError> {
        let object = raw
            .as_object()
            .ok_or(ResponseStreamEventError::ExpectedObject)?;
        let event_type = object
            .get("type")
            .ok_or(ResponseStreamEventError::MissingType)?
            .as_str()
            .ok_or(ResponseStreamEventError::TypeNotString)?
            .to_owned();
        let manifest = classify(&event_type);
        let sequence_number = match object.get("sequence_number") {
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or(ResponseStreamEventError::InvalidSequenceNumber)?,
            ),
            None if manifest.sequence_policy() == ResponseStreamSequencePolicy::Required => {
                return Err(ResponseStreamEventError::MissingSequenceNumber);
            }
            None => None,
        };

        Ok(Self {
            raw,
            event_type,
            manifest,
            sequence_number,
        })
    }

    /// Require an exact match between the SSE `event:` name and JSON `type`.
    ///
    /// An absent SSE event name is represented by an empty string by Norn's
    /// parser and is rejected by the same exact-match rule. The
    /// authority-controlled name is intentionally not retained in the error.
    pub fn validate_sse_event_name(
        &self,
        sse_event_name: &str,
    ) -> Result<(), ResponseStreamEventError> {
        if !sse_event_name.is_empty() && sse_event_name == self.event_type() {
            Ok(())
        } else {
            Err(ResponseStreamEventError::SseEventNameMismatch)
        }
    }

    /// Return the exact validated wire discriminator.
    ///
    /// This value is authority-controlled for [`ResponseStreamEventManifest::Unknown`]
    /// and must not be copied into ordinary diagnostics.
    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// Return the sequence, or `None` for an unsequenced overlay/future event.
    #[must_use]
    pub const fn sequence_number(&self) -> Option<u64> {
        self.sequence_number
    }

    /// Return the manifest classification that admitted this event.
    #[must_use]
    pub const fn manifest(&self) -> ResponseStreamEventManifest {
        self.manifest
    }

    /// Return the public processing stage, if this is a public event.
    #[must_use]
    pub const fn manifest_stage(&self) -> Option<StreamEventStage> {
        self.manifest.stage()
    }

    /// Return the exact admitted JSON, including unknown fields.
    #[must_use]
    pub const fn raw(&self) -> &Value {
        &self.raw
    }

    /// Consume the envelope and return the exact admitted JSON.
    #[must_use]
    pub fn into_raw(self) -> Value {
        self.raw
    }
}

impl TryFrom<Value> for ResponseStreamEvent {
    type Error = ResponseStreamEventError;

    fn try_from(raw: Value) -> Result<Self, Self::Error> {
        Self::from_raw(raw)
    }
}

impl Serialize for ResponseStreamEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResponseStreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        Self::from_raw(raw).map_err(serde::de::Error::custom)
    }
}

/// Failure to validate the common Responses stream-event envelope.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ResponseStreamEventError {
    /// The JSON value was not an object.
    #[error("Responses stream event must be a JSON object")]
    ExpectedObject,
    /// The event object omitted its discriminator.
    #[error("Responses stream event is missing `type`")]
    MissingType,
    /// The event discriminator was not a string.
    #[error("Responses stream event `type` must be a string")]
    TypeNotString,
    /// A public event omitted its required sequence.
    #[error("public Responses stream event is missing `sequence_number`")]
    MissingSequenceNumber,
    /// A sequence was not a nonnegative JSON integer.
    #[error("Responses stream event has an invalid `sequence_number`")]
    InvalidSequenceNumber,
    /// The transport and payload discriminators did not agree exactly.
    #[error("SSE event name does not match Responses payload type")]
    SseEventNameMismatch,
}

fn classify(event_type: &str) -> ResponseStreamEventManifest {
    if let Some(entry) = response_contract::public_stream_event(event_type) {
        return ResponseStreamEventManifest::Public(entry);
    }
    response_contract::codex_overlay(event_type)
        .filter(|entry| entry.kind() == CodexOverlayKind::StreamEvent)
        .map_or(
            ResponseStreamEventManifest::Unknown,
            ResponseStreamEventManifest::CodexOverlay,
        )
}

#[cfg(test)]
mod official_shape_tests;

#[cfg(test)]
mod tests {
    use std::error::Error;

    use serde_json::json;

    use super::*;
    use crate::provider::openai::response_contract::{CODEX_OVERLAY, PUBLIC_STREAM_EVENTS};

    #[test]
    fn all_public_events_require_and_retain_a_sequence() -> Result<(), Box<dyn Error>> {
        for entry in &PUBLIC_STREAM_EVENTS {
            let raw = json!({
                "type": entry.name(),
                "sequence_number": 0,
                "future_field": {"kept": true},
            });
            let event = ResponseStreamEvent::from_sse(entry.name(), raw.clone())?;

            assert_eq!(event.event_type(), entry.name());
            assert_eq!(event.manifest().known_event_type(), Some(entry.name()));
            assert_eq!(event.sequence_number(), Some(0));
            assert_eq!(event.manifest_stage(), Some(entry.stage()));
            assert_eq!(event.raw(), &raw);
            assert_eq!(
                event.manifest().sequence_policy(),
                ResponseStreamSequencePolicy::Required
            );
            assert_eq!(
                ResponseStreamEvent::from_raw(json!({"type": entry.name()})),
                Err(ResponseStreamEventError::MissingSequenceNumber)
            );
        }
        Ok(())
    }

    #[test]
    fn codex_event_overlays_accept_optional_sequences() -> Result<(), Box<dyn Error>> {
        let overlays: Vec<_> = CODEX_OVERLAY
            .iter()
            .filter(|entry| entry.kind() == CodexOverlayKind::StreamEvent)
            .collect();
        assert_eq!(overlays.len(), 2);

        for entry in overlays {
            let unsequenced =
                ResponseStreamEvent::from_sse(entry.name(), json!({"type": entry.name()}))?;
            assert_eq!(unsequenced.event_type(), entry.name());
            assert_eq!(
                unsequenced.manifest().known_event_type(),
                Some(entry.name())
            );
            assert_eq!(unsequenced.sequence_number(), None);
            assert_eq!(unsequenced.manifest_stage(), None);
            assert_eq!(
                unsequenced.manifest().sequence_policy(),
                ResponseStreamSequencePolicy::Optional
            );

            let sequenced = ResponseStreamEvent::from_raw(json!({
                "type": entry.name(),
                "sequence_number": 9,
            }))?;
            assert_eq!(sequenced.sequence_number(), Some(9));
        }
        Ok(())
    }

    #[test]
    fn rejects_malformed_sequences_for_both_manifest_families() {
        for (event_type, value) in [
            ("response.created", json!(-1)),
            ("response.created", json!(1.5)),
            ("response.metadata", json!(null)),
            ("codex.rate_limits", json!("1")),
        ] {
            assert_eq!(
                ResponseStreamEvent::from_raw(json!({
                    "type": event_type,
                    "sequence_number": value,
                })),
                Err(ResponseStreamEventError::InvalidSequenceNumber)
            );
        }
    }

    #[test]
    fn rejects_structurally_invalid_discriminators() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            ResponseStreamEvent::from_raw(json!([])),
            Err(ResponseStreamEventError::ExpectedObject)
        );
        assert_eq!(
            ResponseStreamEvent::from_raw(json!({"sequence_number": 1})),
            Err(ResponseStreamEventError::MissingType)
        );
        assert_eq!(
            ResponseStreamEvent::from_raw(json!({"type": 1, "sequence_number": 1})),
            Err(ResponseStreamEventError::TypeNotString)
        );
        let unknown = ResponseStreamEvent::from_raw(json!({
            "type": "response.processed",
            "sequence_number": 1,
            "future": {"retained": true},
        }))?;
        assert_eq!(unknown.manifest(), ResponseStreamEventManifest::Unknown);
        Ok(())
    }

    #[test]
    fn unknown_events_are_retained_without_an_invented_sequence_policy()
    -> Result<(), Box<dyn Error>> {
        for raw in [
            json!({"type": "response.future"}),
            json!({"type": "response.future", "sequence_number": 12}),
        ] {
            let event = ResponseStreamEvent::from_sse("response.future", raw.clone())?;
            assert_eq!(event.event_type(), "response.future");
            assert_eq!(event.manifest(), ResponseStreamEventManifest::Unknown);
            assert_eq!(event.manifest().known_event_type(), None);
            assert_eq!(
                event.manifest().sequence_policy(),
                ResponseStreamSequencePolicy::Unspecified
            );
            assert_eq!(event.raw(), &raw);
        }

        assert_eq!(
            ResponseStreamEvent::from_raw(json!({
                "type": "response.future",
                "sequence_number": -1,
            })),
            Err(ResponseStreamEventError::InvalidSequenceNumber)
        );
        Ok(())
    }

    #[test]
    fn non_event_codex_overlay_names_remain_unknown_event_types() -> Result<(), Box<dyn Error>> {
        for entry in &CODEX_OVERLAY {
            if entry.kind() != CodexOverlayKind::StreamEvent {
                let event = ResponseStreamEvent::from_raw(json!({"type": entry.name()}))?;
                assert_eq!(event.event_type(), entry.name());
                assert_eq!(event.manifest(), ResponseStreamEventManifest::Unknown);
            }
        }
        Ok(())
    }

    #[test]
    fn sse_event_name_must_match_payload_discriminator_exactly() -> Result<(), Box<dyn Error>> {
        let raw = json!({
            "type": "response.output_text.delta",
            "sequence_number": 4,
            "delta": "hello",
        });
        let event = ResponseStreamEvent::from_sse("response.output_text.delta", raw.clone())?;
        assert_eq!(event.raw(), &raw);
        assert_eq!(
            event.validate_sse_event_name("response.output_text.delta"),
            Ok(())
        );

        for sse_event_name in [
            "",
            "response.output_text.done",
            " response.output_text.delta",
        ] {
            assert_eq!(
                ResponseStreamEvent::from_sse(sse_event_name, raw.clone()),
                Err(ResponseStreamEventError::SseEventNameMismatch)
            );
        }
        Ok(())
    }

    #[test]
    fn debug_and_validation_errors_do_not_disclose_authority_data() -> Result<(), Box<dyn Error>> {
        let sentinel_type = "response.future.secret-sentinel";
        let sentinel_data = "payload-secret-sentinel";
        let event = ResponseStreamEvent::from_raw(json!({
            "type": sentinel_type,
            "sequence_number": 1,
            "data": sentinel_data,
        }))?;
        let debug = format!("{event:?}");
        assert!(!debug.contains(sentinel_type));
        assert!(!debug.contains(sentinel_data));

        let mismatch = event.validate_sse_event_name("transport-secret-sentinel");
        assert_eq!(
            mismatch,
            Err(ResponseStreamEventError::SseEventNameMismatch)
        );
        let rendered = mismatch.map_or_else(|error| error.to_string(), |()| String::new());
        assert!(!rendered.contains(sentinel_type));
        assert!(!rendered.contains("transport-secret-sentinel"));
        Ok(())
    }

    #[test]
    fn serde_round_trip_preserves_exact_provider_value() -> Result<(), Box<dyn Error>> {
        for raw in [
            json!({
                "type": "response.output_text.delta",
                "sequence_number": 42,
                "item_id": "msg_1",
                "output_index": 3,
                "content_index": 2,
                "delta": "hello",
                "obfuscation": "opaque padding",
                "future": [null, 1, {"nested": true}],
            }),
            json!({
                "type": "response.future",
                "future": [null, 1, {"nested": true}],
            }),
        ] {
            let event: ResponseStreamEvent = serde_json::from_value(raw.clone())?;
            assert_eq!(event.raw(), &raw);
            assert_eq!(serde_json::to_value(&event)?, raw);
            assert_eq!(event.into_raw(), raw);
        }
        Ok(())
    }
}
