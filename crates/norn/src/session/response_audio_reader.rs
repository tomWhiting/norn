use std::fs::File;
use std::io::{BufRead as _, BufReader};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{
    RESPONSE_AUDIO_EXTENSION, RESPONSE_AUDIO_SCHEMA, ResponseAudioArtifact,
    ResponseAudioArtifactRef, ResponseAudioArtifactState, ResponseAudioStore,
    ResponseAudioTerminalIntegrity, terminal_integrity_sha256,
};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::resource::DescriptorGovernor;
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::index::with_registered_generation;
use crate::util::PrivateEntryKind;

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum StoredRecord {
    Header {
        schema: u32,
        artifact_id: ResponseAudioArtifactRef,
        owner_session_id: String,
        owner_generation: uuid::Uuid,
        attempt: u32,
        created_at: chrono::DateTime<chrono::Utc>,
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

struct DecodedState {
    reference: ResponseAudioArtifactRef,
    audio: Vec<u8>,
    transcript: String,
    audio_hash: Sha256,
    transcript_hash: Sha256,
    records_hash: Sha256,
    audio_complete: bool,
    transcript_complete: bool,
    frame_count: u64,
    last_sequence: Option<u64>,
    response_id: Option<String>,
    sealed: bool,
}

impl ResponseAudioStore {
    /// List every response-audio sidecar owned by this root session.
    pub fn list(&self) -> Result<Vec<ResponseAudioArtifactRef>, SessionPersistError> {
        with_registered_generation(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root| {
                let entries = match root.read_dir(&self.response_audio_dir()) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Vec::new());
                    }
                    Err(error) => return Err(error.into()),
                };
                entries
                    .into_iter()
                    .map(|entry| {
                        if entry.kind != PrivateEntryKind::File {
                            return Err(invalid_name("non-regular sidecar entry"));
                        }
                        let name = entry
                            .name
                            .into_string()
                            .map_err(|_error| invalid_name("sidecar filename was not UTF-8"))?;
                        parse_file_name(&name)
                    })
                    .collect()
            },
        )
    }

    /// Strictly read and verify one response-audio sidecar.
    pub fn read(
        &self,
        reference: ResponseAudioArtifactRef,
    ) -> Result<ResponseAudioArtifact, SessionPersistError> {
        if !reference.is_uuid_v4() {
            return Err(invalid(reference, "artifact reference was not a UUID v4"));
        }
        let _descriptor_permit = DescriptorGovernor::global()?.try_acquire(1)?;
        let file = with_registered_generation(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root| Ok(root.open_read(&self.artifact_path(reference))?),
        )?;
        decode(file, reference)
    }
}

fn decode(
    file: File,
    reference: ResponseAudioArtifactRef,
) -> Result<ResponseAudioArtifact, SessionPersistError> {
    let mut reader = BufReader::new(file);
    let first = read_complete_record(&mut reader, reference)?
        .ok_or_else(|| invalid(reference, "artifact was empty"))?;
    let (owner_session_id, owner_generation, attempt, created_at) =
        match parse_record(&first, reference)? {
            StoredRecord::Header {
                schema,
                artifact_id,
                owner_session_id,
                owner_generation,
                attempt,
                created_at,
            } if schema == RESPONSE_AUDIO_SCHEMA
                && artifact_id == reference
                && !owner_session_id.is_empty()
                && owner_generation.get_version_num() == 4
                && attempt > 0 =>
            {
                (owner_session_id, owner_generation, attempt, created_at)
            }
            StoredRecord::Header { .. } => {
                return Err(invalid(reference, "header identity or schema disagreed"));
            }
            StoredRecord::Frame { .. } | StoredRecord::Terminal { .. } => {
                return Err(invalid(reference, "first record was not a header"));
            }
        };
    let mut state = DecodedState {
        reference,
        audio: Vec::new(),
        transcript: String::new(),
        audio_hash: Sha256::new(),
        transcript_hash: Sha256::new(),
        records_hash: hash_record_line(&first),
        audio_complete: false,
        transcript_complete: false,
        frame_count: 0,
        last_sequence: None,
        response_id: None,
        sealed: false,
    };
    loop {
        let Some(line) = read_next_record(&mut reader, reference, state.sealed)? else {
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
            } => state.apply_terminal(TerminalExpectation {
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
    Ok(ResponseAudioArtifact {
        reference,
        owner_session_id,
        owner_generation,
        attempt,
        created_at,
        audio: state.audio,
        transcript: state.transcript,
        audio_complete: state.audio_complete,
        transcript_complete: state.transcript_complete,
        response_id: state.response_id,
        state: if state.sealed {
            ResponseAudioArtifactState::Sealed
        } else {
            ResponseAudioArtifactState::Unsealed
        },
    })
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

impl DecodedState {
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
                self.audio.extend(bytes);
            }
            ResponseAudioEvent::AudioDone { .. } => self.audio_complete = true,
            ResponseAudioEvent::TranscriptDelta { delta, .. } => {
                self.transcript_hash.update(delta.as_bytes());
                self.transcript.push_str(&delta);
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

    fn apply_terminal(&mut self, expected: TerminalExpectation) -> Result<(), SessionPersistError> {
        if self.sealed {
            return Err(invalid(self.reference, "terminal record was repeated"));
        }
        let audio_bytes = u64::try_from(self.audio.len())
            .map_err(|_error| invalid(self.reference, "audio size was not representable"))?;
        let transcript_bytes = u64::try_from(self.transcript.len())
            .map_err(|_error| invalid(self.reference, "transcript size was not representable"))?;
        let terminal_integrity = ResponseAudioTerminalIntegrity {
            response_id: expected.response_id.as_deref(),
            frame_count: expected.frame_count,
            audio_bytes: expected.audio_bytes,
            audio_sha256: &expected.audio_sha256,
            transcript_bytes: expected.transcript_bytes,
            transcript_sha256: &expected.transcript_sha256,
            audio_complete: expected.audio_complete,
            transcript_complete: expected.transcript_complete,
        };
        let integrity_sha256 =
            terminal_integrity_sha256(self.records_hash.clone(), &terminal_integrity)?;
        let valid = expected.frame_count == self.frame_count
            && expected.audio_bytes == audio_bytes
            && expected.audio_sha256 == hex_digest(self.audio_hash.clone())
            && expected.transcript_bytes == transcript_bytes
            && expected.transcript_sha256 == hex_digest(self.transcript_hash.clone())
            && expected.integrity_sha256 == integrity_sha256
            && expected.audio_complete == self.audio_complete
            && expected.transcript_complete == self.transcript_complete;
        if !valid {
            return Err(invalid(self.reference, "terminal integrity data disagreed"));
        }
        self.response_id = expected.response_id;
        self.sealed = true;
        Ok(())
    }
}

fn read_complete_record(
    reader: &mut BufReader<File>,
    reference: ResponseAudioArtifactRef,
) -> Result<Option<String>, SessionPersistError> {
    let mut bytes = Vec::new();
    if reader.read_until(b'\n', &mut bytes)? == 0 {
        return Ok(None);
    }
    if !bytes.ends_with(b"\n") {
        return Err(invalid(reference, "header record was torn"));
    }
    bytes.pop();
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_error| invalid(reference, "record JSON was not UTF-8"))
}

fn read_next_record(
    reader: &mut BufReader<File>,
    reference: ResponseAudioArtifactRef,
    sealed: bool,
) -> Result<Option<String>, SessionPersistError> {
    let mut bytes = Vec::new();
    if reader.read_until(b'\n', &mut bytes)? == 0 {
        return Ok(None);
    }
    if sealed {
        return Err(invalid(reference, "data followed terminal record"));
    }
    if !bytes.ends_with(b"\n") {
        return Ok(None);
    }
    bytes.pop();
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_error| invalid(reference, "record JSON was not UTF-8"))
}

fn hash_record_line(line: &str) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(line.as_bytes());
    hash.update(b"\n");
    hash
}

fn parse_record(
    line: &str,
    reference: ResponseAudioArtifactRef,
) -> Result<StoredRecord, SessionPersistError> {
    serde_json::from_str(line).map_err(|_error| invalid(reference, "record JSON was invalid"))
}

fn parse_file_name(name: &str) -> Result<ResponseAudioArtifactRef, SessionPersistError> {
    let suffix = format!(".{RESPONSE_AUDIO_EXTENSION}");
    let stem = name
        .strip_suffix(&suffix)
        .ok_or_else(|| invalid_name("sidecar filename had the wrong extension"))?;
    let id = uuid::Uuid::parse_str(stem)
        .map_err(|_error| invalid_name("sidecar filename was not a UUID"))?;
    if id.get_version_num() != 4 {
        return Err(invalid_name("sidecar filename was not a UUID v4"));
    }
    if stem != id.hyphenated().to_string() {
        return Err(invalid_name("sidecar filename was not canonical UUID text"));
    }
    Ok(ResponseAudioArtifactRef(id))
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

fn invalid_name(reason: &'static str) -> SessionPersistError {
    SessionPersistError::InvalidResponseAudioArtifact {
        artifact_id: "<inventory>".to_owned(),
        reason,
    }
}
