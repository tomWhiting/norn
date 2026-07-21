use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// A structural or observational validation failure in a strict session store.
#[derive(Debug, Error)]
pub enum StrictStoreError {
    /// Descriptor admission failed before observational I/O began.
    #[error("strict session-store descriptor admission failed: {reason}")]
    DescriptorAdmission {
        /// Self-diagnosing descriptor admission failure.
        reason: String,
    },
    /// Observational file I/O failed.
    #[error("strict session-store I/O failed at {}: {source}", path.display())]
    Io {
        /// File that could not be observed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A required format header was absent.
    #[error("strict session file {} is missing its format header", path.display())]
    MissingHeader {
        /// File without a first row.
        path: PathBuf,
    },
    /// A physical JSONL row was empty.
    #[error("strict session file {} contains an empty row at line {line}", path.display())]
    EmptyRow {
        /// File containing the row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
    },
    /// The final physical row was not newline-terminated.
    #[error("strict session file {} has a torn final row at line {line}", path.display())]
    TornTail {
        /// File containing the partial row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
    },
    /// A JSON row was malformed or could not satisfy its typed schema.
    #[error("strict session file {} has invalid JSON at line {line}: {reason}", path.display())]
    InvalidJson {
        /// File containing the row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
        /// Decoder diagnostic.
        reason: String,
    },
    /// The first row was not exactly the strict one-field header object.
    #[error("strict session file {} has an invalid header: {reason}", path.display())]
    InvalidHeader {
        /// File containing the header.
        path: PathBuf,
        /// Header-shape diagnostic.
        reason: String,
    },
    /// A legacy format was presented to the strict reader.
    #[error(
        "strict session file {} uses legacy format {found}; format {expected} is required",
        path.display()
    )]
    LegacyFormat {
        /// File containing the header.
        path: PathBuf,
        /// Observed legacy version.
        found: u32,
        /// Required strict version.
        expected: u32,
    },
    /// A newer format was presented to this strict reader.
    #[error(
        "strict session file {} uses newer format {found}; this reader supports {expected}",
        path.display()
    )]
    NewerFormat {
        /// File containing the header.
        path: PathBuf,
        /// Observed newer version.
        found: u32,
        /// Version supported by this reader.
        expected: u32,
    },
    /// A row contained a field the typed format does not consume verbatim.
    #[error(
        "strict session file {} contains unknown field '{field}' at line {line}",
        path.display()
    )]
    UnknownField {
        /// File containing the row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
        /// Serde path to the unknown field.
        field: String,
    },
    /// A row used an event discriminator outside the format-2 inventory.
    #[error(
        "strict session file {} contains unknown event type '{event_type}' at line {line}",
        path.display()
    )]
    UnknownEventType {
        /// File containing the row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
        /// Rejected discriminator.
        event_type: String,
    },
    /// Deserializing and serializing a row would change its JSON value.
    #[error(
        "strict session file {} contains a non-canonical row at line {line}: {reason}",
        path.display()
    )]
    NonCanonicalRow {
        /// File containing the row.
        path: PathBuf,
        /// One-based physical line number.
        line: usize,
        /// Exact fidelity failure.
        reason: String,
    },
    /// An index row failed path or format validation.
    #[error("strict session index row {line} is invalid: {reason}")]
    InvalidIndexEntry {
        /// One-based physical line number.
        line: usize,
        /// Validation diagnostic.
        reason: String,
    },
    /// Two index rows claimed the same session identifier.
    #[error("strict session index duplicates id '{id}' at lines {first_line} and {line}")]
    DuplicateSessionId {
        /// Duplicated identifier.
        id: String,
        /// First physical line containing the identifier.
        first_line: usize,
        /// Repeated physical line.
        line: usize,
    },
    /// Two index rows resolved to the same relative timeline file.
    #[error("strict session index maps multiple sessions to '{path}'")]
    DuplicateSessionPath {
        /// Reused relative path.
        path: PathBuf,
    },
    /// A timeline repeated an event identifier.
    #[error(
        "strict session file {} duplicates event id '{event_id}' at lines {first_line} and {line}",
        path.display()
    )]
    DuplicateEventId {
        /// File containing the duplicate.
        path: PathBuf,
        /// Duplicated event identifier.
        event_id: String,
        /// First physical line containing the identifier.
        first_line: usize,
        /// Repeated physical line.
        line: usize,
    },
    /// A timeline's reserved provider-state records are internally inconsistent.
    #[error(
        "strict session file {} has invalid provider-state semantics: {source}",
        path.display()
    )]
    InvalidProviderState {
        /// Timeline containing the invalid reserved records.
        path: PathBuf,
        /// Payload-free semantic classification.
        #[source]
        source: crate::session::ProviderStateValidationError,
    },
    /// Exact format-2 timeline counters cannot be represented by the index schema.
    #[error(
        "strict session file {} overflows index counter '{field}'",
        path.display()
    )]
    IndexCounterOverflow {
        /// Timeline containing the unrepresentable exact total.
        path: PathBuf,
        /// Index counter whose exact value exceeded `u64`.
        field: &'static str,
    },
    /// The staged index count disagreed with the strictly decoded timeline.
    #[error(
        "strict session '{session_id}' indexes {indexed} events but its timeline contains {actual}"
    )]
    EventCountMismatch {
        /// Session whose count disagreed.
        session_id: String,
        /// Count recorded in the index.
        indexed: u64,
        /// Strictly decoded event count.
        actual: u64,
    },
    /// A decoded timeline's event count could not be represented in the index type.
    #[error("strict session '{session_id}' event count is not representable: {reason}")]
    EventCountUnrepresentable {
        /// Session whose count could not be represented.
        session_id: String,
        /// Integer conversion diagnostic.
        reason: String,
    },
    /// The staged index usage totals disagreed with the decoded timeline.
    #[error("strict session '{session_id}' index usage totals do not match its timeline")]
    UsageMismatch {
        /// Session whose usage disagreed.
        session_id: String,
    },
}

impl StrictStoreError {
    pub(super) fn io(path: &std::path::Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn invalid_json(
        path: &std::path::Path,
        line: usize,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::InvalidJson {
            path: path.to_path_buf(),
            line,
            reason: error.to_string(),
        }
    }
}
