use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{
    RESPONSE_AUDIO_SCHEMA, ResponseAudioArtifactRef, ResponseAudioStore,
    ResponseAudioTerminalIntegrity, terminal_integrity_sha256,
};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::resource::{DescriptorGovernor, DescriptorPermit};
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::index::with_registered_generation;

#[derive(Serialize)]
struct HeaderRecord<'owner> {
    record: &'static str,
    schema: u32,
    artifact_id: ResponseAudioArtifactRef,
    owner_session_id: &'owner str,
    owner_generation: uuid::Uuid,
    attempt: u32,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct FrameRecord<'event> {
    record: &'static str,
    event: &'event serde_json::Value,
}

#[derive(Serialize)]
struct TerminalRecord<'response> {
    record: &'static str,
    #[serde(flatten)]
    integrity: ResponseAudioTerminalIntegrity<'response>,
    integrity_sha256: String,
}

/// One in-flight response-audio sidecar.
pub(crate) struct ResponseAudioWriter {
    store: ResponseAudioStore,
    reference: ResponseAudioArtifactRef,
    file: File,
    // Declared after `file` so the descriptor closes before admission returns.
    _descriptor_permit: DescriptorPermit,
    frame_count: u64,
    audio_bytes: u64,
    transcript_bytes: u64,
    audio_hash: Sha256,
    transcript_hash: Sha256,
    records_hash: Sha256,
    last_sequence: Option<u64>,
    audio_complete: bool,
    transcript_complete: bool,
    sealed: bool,
}

impl std::fmt::Debug for ResponseAudioWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponseAudioWriter")
            .field("reference", &self.reference)
            .field("frame_count", &self.frame_count)
            .field("audio_bytes", &self.audio_bytes)
            .field("transcript_bytes", &self.transcript_bytes)
            .field("audio_complete", &self.audio_complete)
            .field("transcript_complete", &self.transcript_complete)
            .field("sealed", &self.sealed)
            .finish_non_exhaustive()
    }
}

impl ResponseAudioStore {
    /// Begin one immutable response-attempt sidecar.
    pub(crate) fn begin(&self, attempt: u32) -> Result<ResponseAudioWriter, SessionPersistError> {
        let reference = ResponseAudioArtifactRef::new();
        if attempt == 0 {
            return Err(invalid(reference, "response attempt must be positive"));
        }
        let directory = self.response_audio_dir();
        let path = self.artifact_path(reference);
        let descriptor_permit = DescriptorGovernor::global()?.try_acquire(1)?;
        let (mut file, records_hash) = with_registered_generation(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root| {
                root.create_dir_all(&directory)?;
                let mut file = root.create_new(&path)?;
                let mut records_hash = Sha256::new();
                write_hashed_record(
                    &mut file,
                    &HeaderRecord {
                        record: "header",
                        schema: RESPONSE_AUDIO_SCHEMA,
                        artifact_id: reference,
                        owner_session_id: &self.registered.id,
                        owner_generation: self.registered.generation,
                        attempt,
                        created_at: chrono::Utc::now(),
                    },
                    &mut records_hash,
                )?;
                if self.fsync {
                    file.sync_all()?;
                    root.sync_dir(&directory)?;
                    root.sync_dir(&self.artifacts_dir())?;
                    root.sync_dir(Path::new(&self.root_session_id))?;
                    root.sync_dir(Path::new(""))?;
                }
                Ok((file, records_hash))
            },
        )?;
        file.flush()?;
        Ok(ResponseAudioWriter {
            store: self.clone(),
            reference,
            file,
            _descriptor_permit: descriptor_permit,
            frame_count: 0,
            audio_bytes: 0,
            transcript_bytes: 0,
            audio_hash: Sha256::new(),
            transcript_hash: Sha256::new(),
            records_hash,
            last_sequence: None,
            audio_complete: false,
            transcript_complete: false,
            sealed: false,
        })
    }

    /// Revalidate and, under an fsyncing session policy, durably checkpoint an
    /// unsealed sidecar before a hard-cut event publishes its reference.
    pub(crate) fn checkpoint_reference(
        &self,
        reference: ResponseAudioArtifactRef,
    ) -> Result<(), SessionPersistError> {
        if !reference.is_uuid_v4() {
            return Err(invalid(reference, "artifact reference was not a UUID v4"));
        }
        let _descriptor_permit = DescriptorGovernor::global()?.try_acquire(1)?;
        with_registered_generation(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root| {
                let file = root.open_read_append(&self.artifact_path(reference))?;
                if self.fsync {
                    file.sync_all()?;
                }
                Ok(())
            },
        )
    }
}

impl ResponseAudioWriter {
    pub(crate) const fn reference(&self) -> ResponseAudioArtifactRef {
        self.reference
    }

    /// Append one reconciler-accepted audio frame and its exact raw envelope.
    pub(crate) fn append(
        &mut self,
        raw: &ResponseStreamEvent,
        event: &ResponseAudioEvent,
    ) -> Result<(), SessionPersistError> {
        if self.sealed {
            return Err(invalid(self.reference, "frame followed terminal record"));
        }
        let projected = ResponseAudioEvent::from_stream_event(raw)
            .map_err(|_error| invalid(self.reference, "retained frame payload was invalid"))?
            .ok_or_else(|| invalid(self.reference, "retained frame was not response audio"))?;
        if &projected != event {
            return Err(invalid(
                self.reference,
                "typed frame disagreed with retained envelope",
            ));
        }
        if self
            .last_sequence
            .is_some_and(|prior| event.sequence_number() <= prior)
        {
            return Err(invalid(
                self.reference,
                "audio frame sequence was not increasing",
            ));
        }
        self.validate_lifecycle(event)?;
        write_hashed_record(
            &mut self.file,
            &FrameRecord {
                record: "frame",
                event: raw.raw(),
            },
            &mut self.records_hash,
        )?;
        self.apply(event)?;
        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or_else(|| invalid(self.reference, "frame count overflowed"))?;
        self.last_sequence = Some(event.sequence_number());
        Ok(())
    }

    /// Seal the sidecar after the provider's terminal response frame.
    pub(crate) fn seal(
        mut self,
        response_id: Option<&str>,
    ) -> Result<ResponseAudioArtifactRef, SessionPersistError> {
        let audio_sha256 = hex_digest(self.audio_hash.clone());
        let transcript_sha256 = hex_digest(self.transcript_hash.clone());
        let integrity = ResponseAudioTerminalIntegrity {
            response_id,
            frame_count: self.frame_count,
            audio_bytes: self.audio_bytes,
            audio_sha256: &audio_sha256,
            transcript_bytes: self.transcript_bytes,
            transcript_sha256: &transcript_sha256,
            audio_complete: self.audio_complete,
            transcript_complete: self.transcript_complete,
        };
        let integrity_sha256 = terminal_integrity_sha256(self.records_hash.clone(), &integrity)?;
        let record = TerminalRecord {
            record: "terminal",
            integrity,
            integrity_sha256,
        };
        write_record(&mut self.file, &record)?;
        self.file.flush()?;
        if self.store.fsync {
            self.file.sync_all()?;
        }
        with_registered_generation(
            &self.store.data_dir,
            &self.store.registered,
            self.store.index_lock_deadline,
            |root| {
                if self.store.fsync {
                    root.sync_dir(&self.store.response_audio_dir())?;
                }
                Ok(())
            },
        )?;
        self.sealed = true;
        Ok(self.reference)
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

    fn apply(&mut self, event: &ResponseAudioEvent) -> Result<(), SessionPersistError> {
        match event {
            ResponseAudioEvent::AudioDelta { bytes, .. } => {
                self.audio_hash.update(bytes);
                self.audio_bytes = checked_len_add(self.audio_bytes, bytes.len(), self.reference)?;
            }
            ResponseAudioEvent::AudioDone { .. } => {
                self.audio_complete = true;
            }
            ResponseAudioEvent::TranscriptDelta { delta, .. } => {
                self.transcript_hash.update(delta.as_bytes());
                self.transcript_bytes =
                    checked_len_add(self.transcript_bytes, delta.len(), self.reference)?;
            }
            ResponseAudioEvent::TranscriptDone { .. } => {
                self.transcript_complete = true;
            }
        }
        Ok(())
    }
}

fn checked_len_add(
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

fn write_record(file: &mut File, record: &impl Serialize) -> Result<(), SessionPersistError> {
    serde_json::to_writer(&mut *file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn write_hashed_record(
    file: &mut File,
    record: &impl Serialize,
    hash: &mut Sha256,
) -> Result<(), SessionPersistError> {
    let encoded = serde_json::to_vec(record)?;
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    hash.update(&encoded);
    hash.update(b"\n");
    Ok(())
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
