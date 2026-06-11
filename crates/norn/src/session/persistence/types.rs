//! Domain types for session persistence (NC-002).

use crate::session::SessionError;
use crate::session::events::SessionEvent;
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
/// [`SessionEvent`] (which is internally tagged on `type`), and vice
/// versa. The header is optional on read: files created before versioning
/// (format `0`) start directly with an event line and still load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFileHeader {
    /// Schema version of the writer that created the file.
    #[serde(rename = "norn_session_format")]
    pub version: u32,
}

/// Result of tolerantly reading a session JSONL file.
///
/// Produced by [`read_session_events`](super::io::read_session_events):
/// structurally corrupt lines (torn writes, invalid JSON),
/// unknown-variant lines (events from a newer writer), and duplicate
/// `EventId` lines (crash-retry artifacts; first occurrence kept) are
/// skipped with a `tracing::warn!` and counted in
/// [`Self::skipped_lines`] instead of failing the whole session.
#[derive(Debug)]
pub struct SessionFileRead {
    /// Every event that parsed successfully, in file order, with later
    /// duplicate-`EventId` occurrences removed.
    pub events: Vec<SessionEvent>,
    /// Number of non-empty lines that were skipped: unparseable as a
    /// [`SessionEvent`] (torn write, invalid JSON, unknown variant) or
    /// carrying an `EventId` already seen earlier in the file. `0` for
    /// a healthy file.
    pub skipped_lines: u64,
    /// Schema version from the file's header line, or `None` for a
    /// pre-versioning (format `0`) file with no header.
    pub format_version: Option<u32>,
}

/// Errors produced by the session persistence layer (NC-002).
#[derive(Debug, Error)]
pub enum SessionPersistError {
    /// Filesystem I/O failed.
    #[error("session persistence I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("session persistence serde error: {0}")]
    Serde(#[from] serde_json::Error),

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

    /// Attempted to fork a session that has no events.
    #[error("cannot fork session '{id}': source has no events")]
    EmptySource {
        /// Source session ID.
        id: String,
    },

    /// `EventStore::append` rejected an event during resume reconstruction.
    #[error("event store rejected resumed event: {0}")]
    EventStore(String),
}

impl From<SessionError> for SessionPersistError {
    fn from(value: SessionError) -> Self {
        Self::EventStore(value.to_string())
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
    /// UUID v7 identifier for the session.
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
}
