//! Response-scoped audio artifacts for the Responses streaming API.
//!
//! Responses audio frames do not belong to an output item: the public wire
//! contract supplies response-level audio/transcript deltas and separate
//! completion markers, but no item/content identity or terminal media payload.
//! This module therefore persists them as a private,
//! non-replayable sidecar beneath the owning root session. One strict JSONL
//! file retains the exact accepted event envelopes; decoded media is produced
//! only by the reader, avoiding a second on-disk copy of the same audio.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::session::persistence::SessionIndexEntry;
use crate::session::spool::registered_root_session_id;
use crate::session::store::DurabilityPolicy;

#[path = "response_audio_link.rs"]
mod link;
#[path = "response_audio_reader.rs"]
mod reader;
#[path = "response_audio_validator.rs"]
mod validator;
#[path = "response_audio_writer.rs"]
mod writer;

const ARTIFACTS_DIR_NAME: &str = "artifacts";
const RESPONSE_AUDIO_DIR_NAME: &str = "response-audio";
const RESPONSE_AUDIO_EXTENSION: &str = "jsonl";
const RESPONSE_AUDIO_SCHEMA: u32 = 1;
const TERMINAL_INTEGRITY_DOMAIN: &[u8] = b"norn-response-audio-terminal-v1\0";

pub use link::{
    RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE, ResponseAudioArtifactLink, ResponseAudioReferenceError,
    referenced_response_audio_artifacts, response_audio_artifact_links,
};

#[derive(Clone, Copy, Serialize)]
pub(super) struct ResponseAudioTerminalIntegrity<'value> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) response_id: Option<&'value str>,
    pub(super) frame_count: u64,
    pub(super) audio_bytes: u64,
    pub(super) audio_sha256: &'value str,
    pub(super) transcript_bytes: u64,
    pub(super) transcript_sha256: &'value str,
    pub(super) audio_complete: bool,
    pub(super) transcript_complete: bool,
}

pub(super) fn terminal_integrity_sha256(
    mut records_hash: Sha256,
    terminal: &ResponseAudioTerminalIntegrity<'_>,
) -> Result<String, serde_json::Error> {
    records_hash.update(TERMINAL_INTEGRITY_DOMAIN);
    records_hash.update(serde_json::to_vec(terminal)?);
    Ok(format!("{:x}", records_hash.finalize()))
}

/// Opaque UUID-v4 identifier for one response-audio sidecar.
///
/// Norn writers mint these locally; deserialization enforces the same UUID
/// version before a reference can enter a strict transcript.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ResponseAudioArtifactRef(Uuid);

impl<'de> Deserialize<'de> for ResponseAudioArtifactRef {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
        if id.get_version_num() != 4 {
            return Err(serde::de::Error::custom(
                "response-audio artifact reference must be UUID v4",
            ));
        }
        if value != id.hyphenated().to_string() {
            return Err(serde::de::Error::custom(
                "response-audio artifact reference must use canonical UUID text",
            ));
        }
        Ok(Self(id))
    }
}

impl fmt::Display for ResponseAudioArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl ResponseAudioArtifactRef {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn file_name(self) -> String {
        format!("{}.{RESPONSE_AUDIO_EXTENSION}", self.0)
    }

    fn is_uuid_v4(self) -> bool {
        self.0.get_version_num() == 4
    }
}

pub(crate) fn response_audio_artifact_path(
    root_session_id: &str,
    reference: ResponseAudioArtifactRef,
) -> PathBuf {
    PathBuf::from(root_session_id)
        .join(ARTIFACTS_DIR_NAME)
        .join(RESPONSE_AUDIO_DIR_NAME)
        .join(reference.file_name())
}

/// Whether a sidecar reached its local terminal integrity record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseAudioArtifactState {
    /// The provider call reached a terminal response and the sidecar was
    /// sealed after all accepted audio frames had been written.
    Sealed,
    /// No terminal integrity record exists yet. The writer may still be live,
    /// or it may have ended through cancellation, process interruption, or a
    /// retryable stream failure; a reader cannot distinguish those cases.
    Unsealed,
}

/// Strictly decoded contents of one response-audio sidecar.
#[derive(Clone, Eq, PartialEq)]
pub struct ResponseAudioArtifact {
    /// Opaque local reference naming the sidecar.
    pub reference: ResponseAudioArtifactRef,
    /// Timeline that originally created the artifact.
    pub owner_session_id: String,
    /// Exact timeline generation that originally created the artifact.
    pub owner_generation: Uuid,
    /// One-based provider attempt within the originating call.
    pub attempt: u32,
    /// UTC timestamp recorded before the first frame was accepted.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Audio bytes decoded independently from each accepted Base64 delta.
    pub audio: Vec<u8>,
    /// Transcript deltas concatenated in accepted stream order.
    pub transcript: String,
    /// Whether `response.audio.done` was observed.
    pub audio_complete: bool,
    /// Whether `response.audio.transcript.done` was observed.
    pub transcript_complete: bool,
    /// Authoritative terminal response identifier, when the terminal frame
    /// supplied one. This is metadata, never the artifact's identity.
    pub response_id: Option<String>,
    /// Whether the local terminal integrity record was present and valid.
    pub state: ResponseAudioArtifactState,
}

impl fmt::Debug for ResponseAudioArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseAudioArtifact")
            .field("reference", &self.reference)
            .field("attempt", &self.attempt)
            .field("audio_bytes", &self.audio.len())
            .field("transcript_bytes", &self.transcript.len())
            .field("audio_complete", &self.audio_complete)
            .field("transcript_complete", &self.transcript_complete)
            .field("has_response_id", &self.response_id.is_some())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Generation-bound authority for response-audio artifacts of one timeline.
///
/// Files live beneath the owning root session, while every create, seal, list,
/// and open first verifies this exact timeline generation. Streaming writes use
/// only the already-open sidecar descriptor and never hold the global session
/// index lock.
#[derive(Clone, Debug)]
pub struct ResponseAudioStore {
    data_dir: PathBuf,
    registered: SessionIndexEntry,
    root_session_id: String,
    index_lock_deadline: Option<std::time::Duration>,
    fsync: bool,
}

impl ResponseAudioStore {
    /// Bind a response-audio store to one registered session generation.
    #[must_use]
    pub fn for_session(
        data_dir: &Path,
        registered: &SessionIndexEntry,
        durability: DurabilityPolicy,
        index_lock_deadline: Option<std::time::Duration>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            registered: registered.clone(),
            root_session_id: registered_root_session_id(registered).to_owned(),
            index_lock_deadline,
            fsync: durability != DurabilityPolicy::Flush,
        }
    }

    /// Resolve and strictly verify a sealed transcript association.
    ///
    /// This performs the full streaming sidecar integrity pass on demand and
    /// binds its terminal provider response ID to the typed transcript link.
    /// Ordinary session resume validates the link structure only and does not
    /// decode or hash potentially large media files.
    pub fn read_linked(
        &self,
        link: &ResponseAudioArtifactLink,
    ) -> Result<ResponseAudioArtifact, crate::session::SessionPersistError> {
        let artifact = self.read(link.reference())?;
        if artifact.state != ResponseAudioArtifactState::Sealed {
            return Err(
                crate::session::SessionPersistError::InvalidResponseAudioArtifact {
                    artifact_id: link.reference().to_string(),
                    reason: "a transcript artifact link resolved to an unsealed sidecar",
                },
            );
        }
        if artifact.response_id.as_deref() != link.response_id() {
            return Err(
                crate::session::SessionPersistError::InvalidResponseAudioArtifact {
                    artifact_id: link.reference().to_string(),
                    reason: "the sidecar terminal response ID disagreed with its transcript link",
                },
            );
        }
        Ok(artifact)
    }

    fn artifacts_dir(&self) -> PathBuf {
        PathBuf::from(&self.root_session_id).join(ARTIFACTS_DIR_NAME)
    }

    fn response_audio_dir(&self) -> PathBuf {
        self.artifacts_dir().join(RESPONSE_AUDIO_DIR_NAME)
    }

    fn artifact_path(&self, reference: ResponseAudioArtifactRef) -> PathBuf {
        response_audio_artifact_path(&self.root_session_id, reference)
    }
}

pub(crate) use validator::validate_response_audio_stream;
pub(crate) use writer::ResponseAudioWriter;

#[cfg(test)]
#[path = "response_audio_link_tests.rs"]
mod link_tests;

#[cfg(test)]
#[path = "response_audio_tests.rs"]
mod tests;
