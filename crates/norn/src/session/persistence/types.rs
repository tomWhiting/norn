//! Domain types for session persistence (NC-002).

use crate::session::SessionError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced by the session persistence layer (NC-002).
#[derive(Debug, Error)]
pub enum SessionPersistError {
    /// Filesystem I/O failed.
    #[error("session persistence I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A JSONL line could not be parsed.
    #[error("session persistence parse error on line {line}: {source}")]
    Parse {
        /// 1-based line number of the failing line.
        line: usize,
        /// Underlying JSON parse error.
        #[source]
        source: serde_json::Error,
    },

    /// JSON (de)serialization failed outside the line-tracked reader path.
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
