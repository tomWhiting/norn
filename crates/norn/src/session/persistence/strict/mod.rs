//! Fail-closed codec and observational validation for the strict session store.
//!
//! The active runtime and staged migration validator share the same canonical
//! format-2 index row type.

mod error;
mod index_validation;
mod json;
mod line_reader;
mod reader;
mod types;
mod validation;

pub use super::types::{ResumeFidelity, SessionIndexEntry, SessionRecordOrigin};
pub use error::StrictStoreError;
pub(crate) use index_validation::validate_index_entries;
pub(crate) use reader::visit_strict_event_file;
pub use reader::{read_strict_event_file, read_strict_index_file, validate_strict_event_file};
pub use types::{
    STRICT_SESSION_FORMAT_VERSION, StrictEventFile, StrictFormatHeader, StrictIndexFile,
};
pub use validation::{ValidatedStrictSession, ValidatedStrictStore, validate_staged_store};

#[cfg(test)]
#[path = "index_relationship_tests.rs"]
mod index_relationship_tests;
#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod validation_tests;
