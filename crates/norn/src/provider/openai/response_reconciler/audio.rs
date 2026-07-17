//! Response-level lifecycle validation for audio and audio-transcript events.

use crate::provider::response_audio::{ResponseAudioEvent, ResponseAudioEventError};

use super::{ReconcileUpdate, ResponseReconciliationError};
use crate::provider::openai::sse::SseEvent;

#[derive(Debug, Default)]
pub(super) struct ResponseAudioState {
    audio_done: bool,
    transcript_done: bool,
}

impl ResponseAudioState {
    pub(super) fn accept(
        &mut self,
        wire_event: &SseEvent,
        sequence_number: u64,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let event = ResponseAudioEvent::from_parts(
            &wire_event.event_type,
            sequence_number,
            &wire_event.data,
        )
        .map_err(|error| map_decode_error(&error))?
        .ok_or(ResponseReconciliationError::UnclassifiedPublicEvent)?;

        match &event {
            ResponseAudioEvent::AudioDelta { .. } if self.audio_done => {
                return Err(ResponseReconciliationError::AudioDeltaAfterDone);
            }
            ResponseAudioEvent::AudioDone { .. } if self.audio_done => {
                return Err(ResponseReconciliationError::RepeatedAudioDone);
            }
            ResponseAudioEvent::AudioDone { .. } => self.audio_done = true,
            ResponseAudioEvent::TranscriptDelta { .. } if self.transcript_done => {
                return Err(ResponseReconciliationError::AudioTranscriptDeltaAfterDone);
            }
            ResponseAudioEvent::TranscriptDone { .. } if self.transcript_done => {
                return Err(ResponseReconciliationError::RepeatedAudioTranscriptDone);
            }
            ResponseAudioEvent::TranscriptDone { .. } => self.transcript_done = true,
            ResponseAudioEvent::AudioDelta { .. } | ResponseAudioEvent::TranscriptDelta { .. } => {}
        }

        Ok(ReconcileUpdate::ResponseAudio { event })
    }
}

fn map_decode_error(error: &ResponseAudioEventError) -> ResponseReconciliationError {
    match error {
        ResponseAudioEventError::MissingSequenceNumber => {
            ResponseReconciliationError::MissingSequenceNumber
        }
        ResponseAudioEventError::InvalidDelta { event_type } => {
            ResponseReconciliationError::InvalidEnvelopeField {
                event_type,
                field: "delta",
            }
        }
        ResponseAudioEventError::InvalidBase64 { .. } => {
            ResponseReconciliationError::InvalidAudioDeltaBase64
        }
    }
}
