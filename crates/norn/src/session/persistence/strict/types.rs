use serde::{Deserialize, Serialize};

use crate::session::events::SessionEvent;

use super::super::{SESSION_FORMAT_VERSION, SessionIndexEntry};

/// The active strict session-store format decoded by this codec.
pub const STRICT_SESSION_FORMAT_VERSION: u32 = SESSION_FORMAT_VERSION;

/// The exact first row of every strict index and timeline JSONL file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrictFormatHeader {
    /// Strict file-format version.
    #[serde(rename = "norn_session_format")]
    pub version: u32,
}

impl StrictFormatHeader {
    /// Construct the only header accepted by this reader.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            version: STRICT_SESSION_FORMAT_VERSION,
        }
    }
}

/// A strictly decoded index file.
#[derive(Clone, Debug, PartialEq)]
pub struct StrictIndexFile {
    /// Exact format header.
    pub header: StrictFormatHeader,
    /// Manifest rows in physical file order.
    pub entries: Vec<SessionIndexEntry>,
}

/// A strictly decoded session timeline.
#[derive(Clone, Debug)]
pub struct StrictEventFile {
    /// Exact format header.
    pub header: StrictFormatHeader,
    /// Events in physical append order.
    pub events: Vec<SessionEvent>,
}
