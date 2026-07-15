use thiserror::Error;

use crate::finding::EvidenceTraceabilityIssue;
use crate::{EntryKind, RepositoryPathError};

/// Non-disclosing failure categories establishing a Responses contract authority.
///
/// Caller-controlled paths, fixture identifiers, JSON members, and bytes are
/// deliberately absent so both `Display` and `Debug` remain safe diagnostics.
#[derive(Debug, Error)]
pub enum ResponsesContractError {
    /// A compiled fixed repository path was invalid.
    #[error("a fixed Responses authority path is invalid")]
    FixedPath(#[source] RepositoryPathError),
    /// A required snapshot entry was absent.
    #[error("a required Responses authority entry is missing")]
    Missing,
    /// A required entry was not an ordinary file.
    #[error("a required Responses authority entry is not regular")]
    NotRegular {
        /// Observed entry class, which contains no caller-controlled text.
        kind: EntryKind,
    },
    /// Strict duplicate-safe JSON decoding failed.
    #[error("a Responses authority JSON document is invalid")]
    Json,
    /// A fixed control document did not match its closed schema.
    #[error("a Responses authority schema is invalid")]
    Schema,
    /// A fixture document did not match its manifest identity and dialect.
    #[error("a Responses fixture schema is invalid")]
    FixtureSchema,
    /// Two authority rows declared the same path.
    #[error("a Responses authority path is declared more than once")]
    DuplicateAuthorityPath,
    /// Two fixture rows used the same stable identifier.
    #[error("a Responses fixture identifier is declared more than once")]
    DuplicateFixtureId,
    /// Two fixture rows declared the same path.
    #[error("a Responses fixture path is declared more than once")]
    DuplicateFixturePath,
    /// A governed byte count could not be represented as `u64`.
    #[error("a Responses authority byte length is not representable")]
    LengthOverflow {
        /// Integer conversion failure contains no caller-controlled text.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// Declared and observed byte lengths differed.
    #[error("a Responses authority byte length does not match")]
    LengthMismatch,
    /// Declared and observed SHA-256 digests differed.
    #[error("a Responses authority digest does not match")]
    DigestMismatch,
    /// A file beneath the fixture root was not declared by either manifest.
    #[error("an undeclared Responses fixture is present")]
    UndeclaredFixture,
    /// A file beneath the public-contract root was not declared as an output.
    #[error("an undeclared public Responses contract file is present")]
    UndeclaredPublicContract,
    /// The pinned 62-row traceability registry was invalid or disagreed with a fixture.
    #[error("Responses fixture traceability does not match")]
    TraceabilitySchema,
    /// A valid registry and the observed fixture evidence disagree.
    #[error("Responses fixture evidence traceability is incomplete or inconsistent")]
    EvidenceTraceability {
        /// Closed semantic disagreement class.
        issue: EvidenceTraceabilityIssue,
        /// Number of disagreements in the reported class; always nonzero.
        count: u64,
    },
    /// The pinned traceability registry was not UTF-8.
    #[error("Responses fixture traceability is not UTF-8")]
    TraceabilityUtf8 {
        /// UTF-8 failure reports offsets only, never input bytes.
        #[source]
        source: std::str::Utf8Error,
    },
    /// One traceability row was not strict closed-schema JSON.
    #[error("Responses fixture traceability JSON is invalid")]
    TraceabilityJson,
}
