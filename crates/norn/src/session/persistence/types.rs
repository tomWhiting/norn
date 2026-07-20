//! Domain types for session persistence (NC-002).

use crate::session::SessionError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use super::index_entry::{
    ResumeFidelity, SessionIndexEntry, SessionRecordOrigin, SessionStatus,
};

/// Schema version this writer stamps into new session JSONL files (as the
/// header line) and into [`SessionIndexEntry::format_version`].
///
/// Version history:
///
/// * `0` — implicit; files written before the header existed. Such files
///   have no header line and index entries deserialise with
///   `format_version = 0`.
/// * `1` — first explicit version: a [`SessionFileHeader`] JSON object is
///   the first line of every newly created session file.
/// * `2` — strict index and timeline decoding, with explicit resume fidelity
///   and record provenance on every active index row.
pub const SESSION_FORMAT_VERSION: u32 = 2;

/// The mandatory first line of every active index and session JSONL file.
///
/// Serialised as `{"norn_session_format":N}`. The key is deliberately not
/// `type` so a header line can never be confused with a
/// [`SessionEvent`](crate::session::events::SessionEvent) (which is
/// internally tagged on `type`), and vice versa. Active format-2 readers fail
/// closed when this exact header is absent; headerless and format-1 files are
/// inputs to the explicit legacy migrator, never the active runtime reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileHeader {
    /// Schema version of the writer that created the file.
    #[serde(rename = "norn_session_format")]
    pub version: u32,
}

/// Errors produced by the session persistence layer (NC-002).
#[derive(Debug, Error)]
pub enum SessionPersistError {
    /// Filesystem I/O failed.
    #[error("session persistence I/O failed: {0}")]
    Io(std::io::Error),

    /// The process or system descriptor pool was exhausted.
    #[error(transparent)]
    DescriptorExhausted(Box<crate::resource::DescriptorExhaustion>),

    /// The operation could not reserve its bounded descriptor peak.
    #[error(transparent)]
    DescriptorAdmission(Box<crate::resource::DescriptorAdmissionError>),

    /// JSON (de)serialization failed.
    #[error("session persistence serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Waiting for the inter-process session-index lock exceeded the
    /// caller-configured acquisition deadline (see
    /// [`SessionManager::with_index_lock_deadline`](crate::session::SessionManager::with_index_lock_deadline)).
    /// The index was not read or modified. Without a configured
    /// deadline the wait is indefinite and this variant is never
    /// produced.
    #[error(
        "timed out after {waited:?} waiting for the session index lock at {}; another norn \
         process may be holding it (a wedged holder blocks every session open on this machine)",
        path.display()
    )]
    IndexLockTimeout {
        /// Path of the lock file that could not be acquired in time.
        path: std::path::PathBuf,
        /// The configured deadline that elapsed.
        waited: std::time::Duration,
    },

    /// Session ID could not be resolved against the index.
    #[error("no session matches identifier '{input}'")]
    NotFound {
        /// User-supplied identifier (ID, name, or prefix).
        input: String,
    },

    /// An ID prefix matched more than one entry in the index.
    #[error("identifier '{prefix}' is ambiguous; matches: {}", matches.join(", "))]
    AmbiguousPrefix {
        /// User-supplied prefix.
        prefix: String,
        /// IDs that matched the prefix.
        matches: Vec<String>,
    },

    /// A persisted spool reference failed validation. References become
    /// paths under the session data directory
    /// (`<session-id>/spool/<event-id>.bin`), so the read side
    /// ([`read_spooled_output`](crate::session::spool::read_spooled_output))
    /// rejects anything that could traverse outside it or that does not
    /// match the shape [`SpoolWriter::write`](crate::session::spool::SpoolWriter::write)
    /// produces.
    #[error("invalid spool reference '{spool_ref}': {reason}")]
    InvalidSpoolRef {
        /// The rejected reference.
        spool_ref: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A response-audio sidecar reference or its strict JSONL contents did
    /// not match the private artifact contract.
    #[error("invalid response-audio artifact '{artifact_id}': {reason}")]
    InvalidResponseAudioArtifact {
        /// Locally minted artifact identifier, or the rejected reference.
        artifact_id: String,
        /// Static structural reason. Provider-controlled content is never
        /// copied into this diagnostic.
        reason: &'static str,
    },

    /// A transcript-side response-audio association was structurally invalid.
    #[error("invalid response-audio transcript association: {0}")]
    InvalidResponseAudioReference(#[from] crate::session::ResponseAudioReferenceError),

    /// A caller-supplied session ID failed validation. Session IDs become
    /// file names (`{id}.jsonl`), so the explicit-ID path
    /// ([`SessionManager::open_or_resume`](crate::session::SessionManager::open_or_resume))
    /// rejects anything that could escape the data directory or collide
    /// with the persistence layer's own files.
    #[error("invalid session id '{id}': {reason}")]
    InvalidSessionId {
        /// The rejected identifier.
        id: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A caller-supplied session ID is already indexed, or its target path is
    /// occupied inside an otherwise valid strict store. A store containing
    /// artifacts but no index instead fails as [`Self::MissingIndex`]. The
    /// create-exactly-this path
    /// ([`SessionManager::create_with_id`](crate::session::SessionManager::create_with_id))
    /// never attaches to prior history in either form — choose a new ID
    /// (or, for an indexed session, resume it).
    #[error(
        "session id '{id}' is already in use; choose a new id (or resume the existing session)"
    )]
    IdExists {
        /// The colliding identifier.
        id: String,
    },

    /// Attempted to fork a session that has no events.
    #[error("cannot fork session '{id}': source has no events")]
    EmptySource {
        /// Source session ID.
        id: String,
    },

    /// A child mint found the on-disk location for its freshly reserved
    /// name already occupied — an orphan slug file or an index row with
    /// the same relative path. Under parent-first write ordering the new
    /// code can never produce this state itself; it means external
    /// tampering or residue from a pre-parent-first writer. The mint
    /// refuses hard: truncating the orphan would destroy another agent's
    /// history and appending would interleave two agents in one file.
    #[error(
        "child session path '{rel_path}' is already occupied by a file or index row \
         that the current mint did not create; refusing to touch it"
    )]
    ChildPathOccupied {
        /// The contested path, relative to the data directory.
        rel_path: String,
    },

    /// A crash-recovery publication record disagreed with an existing index
    /// row, timeline, or private staging artifact. Recovery never replaces or
    /// removes the disagreeing bytes.
    #[error("cannot recover publication for session '{id}': {reason}")]
    PublicationConflict {
        /// Session identifier recorded by the publication journal.
        id: String,
        /// Exact invariant that failed during recovery.
        reason: &'static str,
    },

    /// An index-rewrite artifact used the writer-owned name prefix without
    /// matching the exact canonical UUID shape, or occupied an owned name
    /// with a non-file entry. Recovery never guesses ownership from a prefix.
    #[error("cannot recover session index artifact '{name}': {reason}")]
    IndexArtifactConflict {
        /// Conflicting entry name inside the session-store root.
        name: String,
        /// Exact ownership invariant that failed.
        reason: &'static str,
    },

    /// The complete replacement index was atomically renamed into place, but
    /// synchronizing the session-store directory failed. The replacement is
    /// visible to this process, while its survival across a crash is unknown.
    #[error(
        "session index replacement at {} is visible, but its crash durability is \
         indeterminate because the parent directory could not be synchronized: {source}",
        path.display()
    )]
    IndexCommitIndeterminate {
        /// Active index whose replacement reached the rename boundary.
        path: std::path::PathBuf,
        /// Directory synchronization failure after the successful rename.
        #[source]
        source: std::io::Error,
    },

    /// A durable deletion record disagreed with the active index or its owned
    /// artifact paths. Recovery refuses to guess which state is authoritative.
    #[error("cannot recover session deletion '{transaction_id}': {reason}")]
    DeletionConflict {
        /// Identifier encoded in the owned deletion-journal file name.
        transaction_id: String,
        /// Exact invariant that failed during recovery.
        reason: &'static str,
    },

    /// The index no longer contains a deleted session subtree, but physical
    /// cleanup did not finish. The durable deletion journal remains and the
    /// next session-store operation will retry cleanup before proceeding.
    #[error(
        "session '{id}' was deleted from the index, but cleanup transaction '{transaction_id}' \
         remains pending and will be retried: {source}"
    )]
    DeletionCleanupPending {
        /// Session explicitly selected for deletion.
        id: String,
        /// Durable deletion transaction retained for recovery.
        transaction_id: String,
        /// Cleanup or journal-removal failure after index publication.
        #[source]
        source: Box<SessionPersistError>,
    },

    /// A persistent child was requested under an ephemeral parent. An
    /// ephemeral parent has no session directory to hang a child file off,
    /// so the request is refused typed at the mint boundary — never
    /// discovered later as a missing-directory I/O failure. Pass
    /// `ChildDurability::Ephemeral` (the honest propagation of the
    /// parent's own choice) or run the parent with a persisted session.
    #[error(
        "cannot mint a persistent child under ephemeral parent '{parent_path}': \
         the parent has no persisted session; mint the child ephemeral or persist the parent"
    )]
    EphemeralParent {
        /// The ephemeral parent's coordination path address.
        parent_path: String,
    },

    /// `EventStore::append` rejected an event during resume reconstruction.
    #[error("event store rejected resumed event: {0}")]
    EventStore(String),

    /// The active format-2 index failed strict structural validation.
    #[error("invalid active session index: {0}")]
    InvalidIndex(#[source] Box<super::strict::StrictStoreError>),

    /// An active format-2 timeline failed strict structural validation.
    #[error("invalid active session timeline: {0}")]
    InvalidTimeline(#[source] Box<super::strict::StrictStoreError>),

    /// An append would duplicate or contradict an event already durable in
    /// the strict timeline, or retry a different event after an ambiguous
    /// write outcome.
    #[error("session event '{event_id}' cannot be appended: {reason}")]
    EventAppendConflict {
        /// Provider-independent session event identifier.
        event_id: String,
        /// Exact fail-closed conflict reason.
        reason: &'static str,
    },

    /// A retained session handle or index mutation no longer names the active
    /// generation for its session identifier.
    #[error("session generation changed for '{id}'")]
    GenerationChanged {
        /// Session whose generation comparison failed.
        id: String,
    },

    /// A session-store directory contained artifacts but no active index.
    #[error(
        "session store at {} contains data but is missing its required format-2 index",
        path.display()
    )]
    MissingIndex {
        /// Session-store directory whose authority is ambiguous without an index.
        path: std::path::PathBuf,
    },

    /// A degraded projection needs explicit trusted approval before execution.
    #[error("session '{id}' is a fresh-epoch projection; explicit resume approval is required")]
    ResumeApprovalRequired {
        /// Session whose visible history cannot silently enter a new epoch.
        id: String,
    },

    /// The retained record is available for inspection/export but not execution.
    #[error("session '{id}' is inspect-only and cannot be resumed")]
    SessionNotResumable {
        /// Inspect-only session identifier.
        id: String,
    },

    /// An advisory index counter could not represent the next exact value.
    #[error("session '{id}' index counter '{field}' overflowed; the index was not changed")]
    IndexCounterOverflow {
        /// Session whose counter update was rejected.
        id: String,
        /// Counter that could not represent the update.
        field: &'static str,
    },

    /// Provider-scoped state is already owned by another opaque identity.
    ///
    /// Neither the stored nor requested identity is included in the error.
    #[error("session belongs to a different provider-state identity")]
    ProviderStateIdentityMismatch,

    /// A bound session was opened by a provider that could not prove identity.
    #[error("session requires a provider-state identity")]
    ProviderStateIdentityRequired,
}

impl From<std::io::Error> for SessionPersistError {
    fn from(error: std::io::Error) -> Self {
        Self::from_io(error, "performing session persistence I/O", None)
    }
}

impl From<super::strict::StrictStoreError> for SessionPersistError {
    fn from(error: super::strict::StrictStoreError) -> Self {
        Self::InvalidIndex(Box::new(error))
    }
}

impl From<crate::resource::DescriptorAdmissionError> for SessionPersistError {
    fn from(error: crate::resource::DescriptorAdmissionError) -> Self {
        Self::DescriptorAdmission(Box::new(error))
    }
}

impl SessionPersistError {
    pub(crate) fn from_io(
        error: std::io::Error,
        operation: &str,
        path: Option<&std::path::Path>,
    ) -> Self {
        match crate::resource::classify_descriptor_error(&error, operation, path) {
            Some(exhaustion) => Self::DescriptorExhausted(Box::new(exhaustion)),
            None => Self::Io(error),
        }
    }
}

impl From<SessionPersistError> for SessionError {
    fn from(error: SessionPersistError) -> Self {
        match error {
            SessionPersistError::DescriptorExhausted(source) => Self::DescriptorExhausted(source),
            SessionPersistError::ProviderStateIdentityRequired => {
                Self::ProviderStateIdentityRequired
            }
            SessionPersistError::ProviderStateIdentityMismatch => {
                Self::ProviderStateIdentityMismatch
            }
            other => Self::StorageError {
                reason: other.to_string(),
            },
        }
    }
}

impl From<SessionError> for SessionPersistError {
    fn from(value: SessionError) -> Self {
        match value {
            SessionError::DescriptorExhausted(source) => Self::DescriptorExhausted(source),
            SessionError::ProviderStateIdentityRequired => Self::ProviderStateIdentityRequired,
            SessionError::ProviderStateIdentityMismatch => Self::ProviderStateIdentityMismatch,
            other => Self::EventStore(other.to_string()),
        }
    }
}
