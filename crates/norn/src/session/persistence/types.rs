//! Domain types for session persistence (NC-002).

use crate::session::SessionError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
pub const SESSION_FORMAT_VERSION: u32 = 1;

/// The header line written as the first line of a session JSONL file at
/// creation time.
///
/// Serialised as `{"norn_session_format":N}`. The key is deliberately not
/// `type` so a header line can never be confused with a
/// [`SessionEvent`](crate::session::events::SessionEvent) (which is
/// internally tagged on `type`), and vice
/// versa. The header is optional on read: files created before versioning
/// (format `0`) start directly with an event line and still load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    /// A caller-supplied session ID is already in use — indexed, or
    /// present on disk as an orphan `{id}.jsonl` session file. The
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
}

impl From<std::io::Error> for SessionPersistError {
    fn from(error: std::io::Error) -> Self {
        Self::from_io(error, "performing session persistence I/O", None)
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
            other => Self::EventStore(other.to_string()),
        }
    }
}

/// Lifecycle status recorded in the session index. Serialised as
/// lowercase strings (`"active"` / `"completed"`) per NC-002 R3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Session is live and may still accept new events.
    Active,
    /// Session has been finalised and will receive no further events.
    Completed,
}

/// One row in `index.jsonl`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionIndexEntry {
    /// Session identifier — a UUID v4 by default (R8: v7's shared
    /// timestamp prefix defeats git-style short-prefix eyeballing), or a
    /// validated caller-supplied opaque string.
    pub id: String,
    /// Optional human-readable name (set via `/name` or `--session-name`).
    pub name: Option<String>,
    /// Model identifier active when the session was created.
    pub model: String,
    /// Working directory the session was started in.
    pub working_dir: String,
    /// Creation timestamp (ISO 8601 / RFC 3339 with `chrono` `serde` feature).
    pub created_at: DateTime<Utc>,
    /// Most recent append timestamp.
    pub updated_at: DateTime<Utc>,
    /// Total number of events written to the session JSONL file.
    pub event_count: u64,
    /// Lifecycle status.
    pub status: SessionStatus,
    /// Session JSONL schema version of the writer that created the
    /// session ([`SESSION_FORMAT_VERSION`] for new sessions). `0` means
    /// the session predates versioning and its file has no header line.
    #[serde(default)]
    pub format_version: u32,
    /// Cumulative input tokens across all turns.
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all turns.
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Cumulative cache-read tokens across all turns.
    #[serde(default)]
    pub total_cache_read_tokens: u64,
    /// The session file's path **relative to the data directory**, for
    /// sessions whose file does not live at the flat `{id}.jsonl`
    /// derivation — child sessions live at
    /// `{root-id}/children/{path-slug}.jsonl`. Absent (`None`) for
    /// legacy/root sessions, which keep resolving through the flat
    /// derivation unchanged — zero migration of old rows. Discovery stays
    /// manifest-driven: nothing ever crawls the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel_path: Option<String>,
    /// Session id of the parent this session was branched from (the
    /// index-side half of the durable parent↔child linkage; the
    /// event-side half is the parent's `ChildBranch` event). `None` for
    /// root sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}
