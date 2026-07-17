use std::io::{BufRead as _, BufReader, Read};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{
    RESPONSE_AUDIO_SCHEMA, ResponseAudioArtifactRef, ResponseAudioTerminalIntegrity,
    terminal_integrity_sha256,
};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::session::persistence::SessionPersistError;

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum StoredRecord {
    Header {
        schema: u32,
        artifact_id: ResponseAudioArtifactRef,
        owner_session_id: String,
        owner_generation: uuid::Uuid,
        attempt: u32,
        #[serde(rename = "created_at")]
        _created_at: chrono::DateTime<chrono::Utc>,
    },
    Frame {
        event: serde_json::Value,
    },
    Terminal {
        #[serde(default)]
        response_id: Option<String>,
        frame_count: u64,
        audio_bytes: u64,
        audio_sha256: String,
        transcript_bytes: u64,
        transcript_sha256: String,
        integrity_sha256: String,
        audio_complete: bool,
        transcript_complete: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedResponseAudioStream {
    pub(crate) response_id: Option<String>,
    pub(crate) sealed: bool,
}

struct ValidationState {
    reference: ResponseAudioArtifactRef,
    audio_hash: Sha256,
    transcript_hash: Sha256,
    records_hash: Sha256,
    audio_bytes: u64,
    transcript_bytes: u64,
    frame_count: u64,
    last_sequence: Option<u64>,
    audio_complete: bool,
    transcript_complete: bool,
    response_id: Option<String>,
    sealed: bool,
}

struct TerminalExpectation {
    response_id: Option<String>,
    frame_count: u64,
    audio_bytes: u64,
    audio_sha256: String,
    transcript_bytes: u64,
    transcript_sha256: String,
    integrity_sha256: String,
    audio_complete: bool,
    transcript_complete: bool,
}

pub(crate) fn validate_response_audio_stream<Source: Read>(
    source: Source,
    reference: ResponseAudioArtifactRef,
) -> Result<ValidatedResponseAudioStream, SessionPersistError> {
    let mut reader = BufReader::new(source);
    let first = read_header_record(&mut reader, reference)?
        .ok_or_else(|| invalid(reference, "artifact was empty"))?;
    validate_header(parse_record(&first, reference)?, reference)?;
    let mut state = ValidationState {
        reference,
        audio_hash: Sha256::new(),
        transcript_hash: Sha256::new(),
        records_hash: hash_record_line(&first),
        audio_bytes: 0,
        transcript_bytes: 0,
        frame_count: 0,
        last_sequence: None,
        audio_complete: false,
        transcript_complete: false,
        response_id: None,
        sealed: false,
    };
    loop {
        let Some(line) = read_record(&mut reader, reference, state.sealed)? else {
            break;
        };
        match parse_record(&line, reference)? {
            StoredRecord::Header { .. } => {
                return Err(invalid(reference, "header was repeated"));
            }
            StoredRecord::Frame { event } => {
                state.records_hash.update(line.as_bytes());
                state.records_hash.update(b"\n");
                state.apply_frame(event)?;
            }
            StoredRecord::Terminal {
                response_id,
                frame_count,
                audio_bytes,
                audio_sha256,
                transcript_bytes,
                transcript_sha256,
                integrity_sha256,
                audio_complete,
                transcript_complete,
            } => state.apply_terminal(&TerminalExpectation {
                response_id,
                frame_count,
                audio_bytes,
                audio_sha256,
                transcript_bytes,
                transcript_sha256,
                integrity_sha256,
                audio_complete,
                transcript_complete,
            })?,
        }
    }
    Ok(ValidatedResponseAudioStream {
        response_id: state.response_id,
        sealed: state.sealed,
    })
}

fn validate_header(
    record: StoredRecord,
    reference: ResponseAudioArtifactRef,
) -> Result<(), SessionPersistError> {
    match record {
        StoredRecord::Header {
            schema,
            artifact_id,
            owner_session_id,
            owner_generation,
            attempt,
            ..
        } if schema == RESPONSE_AUDIO_SCHEMA
            && artifact_id == reference
            && !owner_session_id.is_empty()
            && owner_generation.get_version_num() == 4
            && attempt > 0 =>
        {
            Ok(())
        }
        StoredRecord::Header { .. } => {
            Err(invalid(reference, "header identity or schema disagreed"))
        }
        StoredRecord::Frame { .. } | StoredRecord::Terminal { .. } => {
            Err(invalid(reference, "first record was not a header"))
        }
    }
}

impl ValidationState {
    fn apply_frame(&mut self, raw: serde_json::Value) -> Result<(), SessionPersistError> {
        if self.sealed {
            return Err(invalid(self.reference, "frame followed terminal record"));
        }
        let envelope = ResponseStreamEvent::from_raw(raw)
            .map_err(|_error| invalid(self.reference, "frame envelope was invalid"))?;
        let event = ResponseAudioEvent::from_stream_event(&envelope)
            .map_err(|_error| invalid(self.reference, "audio frame payload was invalid"))?
            .ok_or_else(|| invalid(self.reference, "sidecar contained a non-audio frame"))?;
        let sequence = event.sequence_number();
        if self.last_sequence.is_some_and(|prior| sequence <= prior) {
            return Err(invalid(
                self.reference,
                "audio frame sequence was not increasing",
            ));
        }
        self.validate_lifecycle(&event)?;
        match event {
            ResponseAudioEvent::AudioDelta { bytes, .. } => {
                self.audio_hash.update(&bytes);
                self.audio_bytes = checked_add(self.audio_bytes, bytes.len(), self.reference)?;
            }
            ResponseAudioEvent::AudioDone { .. } => self.audio_complete = true,
            ResponseAudioEvent::TranscriptDelta { delta, .. } => {
                self.transcript_hash.update(delta.as_bytes());
                self.transcript_bytes =
                    checked_add(self.transcript_bytes, delta.len(), self.reference)?;
            }
            ResponseAudioEvent::TranscriptDone { .. } => self.transcript_complete = true,
        }
        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or_else(|| invalid(self.reference, "frame count overflowed"))?;
        self.last_sequence = Some(sequence);
        Ok(())
    }

    fn validate_lifecycle(&self, event: &ResponseAudioEvent) -> Result<(), SessionPersistError> {
        match event {
            ResponseAudioEvent::AudioDelta { .. } if self.audio_complete => Err(invalid(
                self.reference,
                "audio delta followed channel completion",
            )),
            ResponseAudioEvent::AudioDone { .. } if self.audio_complete => {
                Err(invalid(self.reference, "audio completion was repeated"))
            }
            ResponseAudioEvent::TranscriptDelta { .. } if self.transcript_complete => Err(invalid(
                self.reference,
                "transcript delta followed channel completion",
            )),
            ResponseAudioEvent::TranscriptDone { .. } if self.transcript_complete => Err(invalid(
                self.reference,
                "transcript completion was repeated",
            )),
            _ => Ok(()),
        }
    }

    fn apply_terminal(
        &mut self,
        expected: &TerminalExpectation,
    ) -> Result<(), SessionPersistError> {
        if self.sealed {
            return Err(invalid(self.reference, "terminal record was repeated"));
        }
        let terminal = ResponseAudioTerminalIntegrity {
            response_id: expected.response_id.as_deref(),
            frame_count: expected.frame_count,
            audio_bytes: expected.audio_bytes,
            audio_sha256: &expected.audio_sha256,
            transcript_bytes: expected.transcript_bytes,
            transcript_sha256: &expected.transcript_sha256,
            audio_complete: expected.audio_complete,
            transcript_complete: expected.transcript_complete,
        };
        let integrity_sha256 = terminal_integrity_sha256(self.records_hash.clone(), &terminal)?;
        let valid = expected.frame_count == self.frame_count
            && expected.audio_bytes == self.audio_bytes
            && expected.audio_sha256 == hex_digest(self.audio_hash.clone())
            && expected.transcript_bytes == self.transcript_bytes
            && expected.transcript_sha256 == hex_digest(self.transcript_hash.clone())
            && expected.integrity_sha256 == integrity_sha256
            && expected.audio_complete == self.audio_complete
            && expected.transcript_complete == self.transcript_complete;
        if !valid {
            return Err(invalid(self.reference, "terminal integrity data disagreed"));
        }
        self.response_id.clone_from(&expected.response_id);
        self.sealed = true;
        Ok(())
    }
}

fn read_header_record<Source: Read>(
    reader: &mut BufReader<Source>,
    reference: ResponseAudioArtifactRef,
) -> Result<Option<String>, SessionPersistError> {
    let Some((line, complete)) = read_line(reader, reference)? else {
        return Ok(None);
    };
    if !complete {
        return Err(invalid(reference, "header record was torn"));
    }
    Ok(Some(line))
}

fn read_record<Source: Read>(
    reader: &mut BufReader<Source>,
    reference: ResponseAudioArtifactRef,
    sealed: bool,
) -> Result<Option<String>, SessionPersistError> {
    let Some((line, complete)) = read_line(reader, reference)? else {
        return Ok(None);
    };
    if sealed {
        return Err(invalid(reference, "data followed terminal record"));
    }
    if complete { Ok(Some(line)) } else { Ok(None) }
}

fn read_line<Source: Read>(
    reader: &mut BufReader<Source>,
    reference: ResponseAudioArtifactRef,
) -> Result<Option<(String, bool)>, SessionPersistError> {
    let mut bytes = Vec::new();
    if reader.read_until(b'\n', &mut bytes)? == 0 {
        return Ok(None);
    }
    let complete = bytes.ends_with(b"\n");
    if complete {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(|line| Some((line, complete)))
        .map_err(|_error| invalid(reference, "record JSON was not UTF-8"))
}

fn parse_record(
    line: &str,
    reference: ResponseAudioArtifactRef,
) -> Result<StoredRecord, SessionPersistError> {
    serde_json::from_str(line).map_err(|_error| invalid(reference, "record JSON was invalid"))
}

fn hash_record_line(line: &str) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(line.as_bytes());
    hash.update(b"\n");
    hash
}

fn checked_add(
    current: u64,
    additional: usize,
    reference: ResponseAudioArtifactRef,
) -> Result<u64, SessionPersistError> {
    let additional = u64::try_from(additional)
        .map_err(|_error| invalid(reference, "frame size was not representable"))?;
    current
        .checked_add(additional)
        .ok_or_else(|| invalid(reference, "artifact byte count overflowed"))
}

fn hex_digest(hash: Sha256) -> String {
    format!("{:x}", hash.finalize())
}

fn invalid(reference: ResponseAudioArtifactRef, reason: &'static str) -> SessionPersistError {
    SessionPersistError::InvalidResponseAudioArtifact {
        artifact_id: reference.to_string(),
        reason,
    }
}
