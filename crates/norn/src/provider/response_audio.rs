//! Typed response-scoped audio events.
//!
//! The Responses wire does not attach these events to an output item and does
//! not provide codec or MIME metadata. This typed projection carries the
//! schema-mandated sequence/delta data and independent completion markers; the
//! paired raw envelope retains additional documented example fields such as
//! `response_id` without promoting them to a required typed invariant.

use std::fmt;

use base64::Engine as _;
use serde_json::Value;
use thiserror::Error;

use super::openai::response_stream_event::ResponseStreamEvent;

const AUDIO_DELTA: &str = "response.audio.delta";
const AUDIO_DONE: &str = "response.audio.done";
const TRANSCRIPT_DELTA: &str = "response.audio.transcript.delta";
const TRANSCRIPT_DONE: &str = "response.audio.transcript.done";

/// One typed response-scoped audio event.
#[derive(Clone, Eq, PartialEq)]
pub enum ResponseAudioEvent {
    /// One independently Base64-decoded audio fragment.
    AudioDelta {
        /// Global sequence number from the Responses stream.
        sequence_number: u64,
        /// Decoded bytes carried by this individual delta.
        bytes: Vec<u8>,
    },
    /// The response-level audio channel completed.
    AudioDone {
        /// Global sequence number from the Responses stream.
        sequence_number: u64,
    },
    /// One response-level audio-transcript fragment.
    TranscriptDelta {
        /// Global sequence number from the Responses stream.
        sequence_number: u64,
        /// Transcript fragment carried by this event.
        delta: String,
    },
    /// The response-level audio-transcript channel completed.
    TranscriptDone {
        /// Global sequence number from the Responses stream.
        sequence_number: u64,
    },
}

impl fmt::Debug for ResponseAudioEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AudioDelta {
                sequence_number, ..
            } => formatter
                .debug_struct("AudioDelta")
                .field("sequence_number", sequence_number)
                .field("bytes", &"[REDACTED]")
                .finish(),
            Self::AudioDone { sequence_number } => formatter
                .debug_struct("AudioDone")
                .field("sequence_number", sequence_number)
                .finish(),
            Self::TranscriptDelta {
                sequence_number, ..
            } => formatter
                .debug_struct("TranscriptDelta")
                .field("sequence_number", sequence_number)
                .field("delta", &"[REDACTED]")
                .finish(),
            Self::TranscriptDone { sequence_number } => formatter
                .debug_struct("TranscriptDone")
                .field("sequence_number", sequence_number)
                .finish(),
        }
    }
}

impl ResponseAudioEvent {
    /// Decode one validated Responses stream envelope when it belongs to the
    /// response-scoped audio family. Non-audio envelopes return `Ok(None)`.
    ///
    /// Each `response.audio.delta` value is decoded independently. No codec,
    /// MIME type, output item, or cross-delta Base64 framing is inferred.
    pub fn from_stream_event(
        event: &ResponseStreamEvent,
    ) -> Result<Option<Self>, ResponseAudioEventError> {
        let Some(sequence_number) = event.sequence_number() else {
            return if is_response_audio_event(event.event_type()) {
                Err(ResponseAudioEventError::MissingSequenceNumber)
            } else {
                Ok(None)
            };
        };
        Self::from_parts(event.event_type(), sequence_number, event.raw())
    }

    /// Return the global Responses stream sequence number.
    #[must_use]
    pub const fn sequence_number(&self) -> u64 {
        match self {
            Self::AudioDelta {
                sequence_number, ..
            }
            | Self::AudioDone { sequence_number }
            | Self::TranscriptDelta {
                sequence_number, ..
            }
            | Self::TranscriptDone { sequence_number } => *sequence_number,
        }
    }

    pub(crate) fn from_parts(
        event_type: &str,
        sequence_number: u64,
        raw: &Value,
    ) -> Result<Option<Self>, ResponseAudioEventError> {
        match event_type {
            AUDIO_DELTA => {
                let delta = required_delta(raw, AUDIO_DELTA)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(delta)
                    .map_err(|source| ResponseAudioEventError::InvalidBase64 { source })?;
                Ok(Some(Self::AudioDelta {
                    sequence_number,
                    bytes,
                }))
            }
            AUDIO_DONE => Ok(Some(Self::AudioDone { sequence_number })),
            TRANSCRIPT_DELTA => Ok(Some(Self::TranscriptDelta {
                sequence_number,
                delta: required_delta(raw, TRANSCRIPT_DELTA)?.to_owned(),
            })),
            TRANSCRIPT_DONE => Ok(Some(Self::TranscriptDone { sequence_number })),
            _ => Ok(None),
        }
    }
}

/// Structural failure while decoding a typed response-scoped audio event.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ResponseAudioEventError {
    /// An audio-family event unexpectedly lacked its public sequence number.
    #[error("Responses audio event had no sequence_number")]
    MissingSequenceNumber,
    /// A delta event omitted its required string payload.
    #[error("{event_type} missing or invalid delta")]
    InvalidDelta {
        /// Pinned audio event discriminator.
        event_type: &'static str,
    },
    /// One audio delta was not independently valid standard Base64.
    #[error("response.audio.delta carried invalid Base64 audio data")]
    InvalidBase64 {
        /// Decoder failure without any media content.
        #[source]
        source: base64::DecodeError,
    },
}

fn required_delta<'value>(
    raw: &'value Value,
    event_type: &'static str,
) -> Result<&'value str, ResponseAudioEventError> {
    raw.get("delta")
        .and_then(Value::as_str)
        .ok_or(ResponseAudioEventError::InvalidDelta { event_type })
}

pub(crate) fn is_response_audio_event(event_type: &str) -> bool {
    matches!(
        event_type,
        AUDIO_DELTA | AUDIO_DONE | TRANSCRIPT_DELTA | TRANSCRIPT_DONE
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use serde_json::json;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn stream_event(
        event_type: &str,
        sequence_number: u64,
        delta: Option<&str>,
    ) -> Result<ResponseStreamEvent, Box<dyn Error>> {
        let mut raw = json!({
            "type": event_type,
            "sequence_number": sequence_number,
        });
        if let (Some(object), Some(delta)) = (raw.as_object_mut(), delta) {
            object.insert("delta".to_owned(), json!(delta));
        }
        Ok(ResponseStreamEvent::from_sse(event_type, raw)?)
    }

    #[test]
    fn all_four_audio_events_decode_with_their_sequence() -> TestResult {
        for (wire, expected) in [
            (
                stream_event(AUDIO_DELTA, 1, Some("YXVkaW8="))?,
                ResponseAudioEvent::AudioDelta {
                    sequence_number: 1,
                    bytes: b"audio".to_vec(),
                },
            ),
            (
                stream_event(AUDIO_DONE, 2, None)?,
                ResponseAudioEvent::AudioDone { sequence_number: 2 },
            ),
            (
                stream_event(TRANSCRIPT_DELTA, 3, Some("spoken text"))?,
                ResponseAudioEvent::TranscriptDelta {
                    sequence_number: 3,
                    delta: "spoken text".to_owned(),
                },
            ),
            (
                stream_event(TRANSCRIPT_DONE, 4, None)?,
                ResponseAudioEvent::TranscriptDone { sequence_number: 4 },
            ),
        ] {
            assert_eq!(
                ResponseAudioEvent::from_stream_event(&wire)?,
                Some(expected)
            );
        }
        Ok(())
    }

    #[test]
    fn audio_delta_is_decoded_independently() -> TestResult {
        let event = ResponseStreamEvent::from_sse(
            AUDIO_DELTA,
            json!({
                "type": AUDIO_DELTA,
                "sequence_number": 8,
                "delta": "YXVkaW8=",
            }),
        )?;
        assert_eq!(
            ResponseAudioEvent::from_stream_event(&event)?,
            Some(ResponseAudioEvent::AudioDelta {
                sequence_number: 8,
                bytes: b"audio".to_vec(),
            })
        );
        Ok(())
    }

    #[test]
    fn non_audio_event_returns_none() -> TestResult {
        let event = ResponseStreamEvent::from_sse(
            "response.created",
            json!({"type": "response.created", "sequence_number": 1}),
        )?;
        assert_eq!(ResponseAudioEvent::from_stream_event(&event)?, None);
        Ok(())
    }

    #[test]
    fn invalid_delta_shapes_and_audio_base64_are_typed() -> TestResult {
        for event_type in [AUDIO_DELTA, TRANSCRIPT_DELTA] {
            let missing = ResponseStreamEvent::from_sse(
                event_type,
                json!({"type": event_type, "sequence_number": 1}),
            )?;
            assert_eq!(
                ResponseAudioEvent::from_stream_event(&missing),
                Err(ResponseAudioEventError::InvalidDelta { event_type })
            );

            let non_string = ResponseStreamEvent::from_sse(
                event_type,
                json!({"type": event_type, "sequence_number": 2, "delta": 42}),
            )?;
            assert_eq!(
                ResponseAudioEvent::from_stream_event(&non_string),
                Err(ResponseAudioEventError::InvalidDelta { event_type })
            );
        }

        let invalid = ResponseStreamEvent::from_sse(
            AUDIO_DELTA,
            json!({"type": AUDIO_DELTA, "sequence_number": 3, "delta": "***"}),
        )?;
        assert!(matches!(
            ResponseAudioEvent::from_stream_event(&invalid),
            Err(ResponseAudioEventError::InvalidBase64 { .. })
        ));
        Ok(())
    }

    #[test]
    fn debug_never_discloses_media_or_transcript_contents() {
        let media = format!(
            "{:?}",
            ResponseAudioEvent::AudioDelta {
                sequence_number: 1,
                bytes: b"MEDIA_SENTINEL".to_vec(),
            }
        );
        let transcript = format!(
            "{:?}",
            ResponseAudioEvent::TranscriptDelta {
                sequence_number: 2,
                delta: "TRANSCRIPT_SENTINEL".to_owned(),
            }
        );
        assert!(!media.contains("MEDIA_SENTINEL"));
        assert!(!transcript.contains("TRANSCRIPT_SENTINEL"));
        assert!(media.contains("[REDACTED]"));
        assert!(transcript.contains("[REDACTED]"));
    }
}
