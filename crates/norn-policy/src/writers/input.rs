//! Owned writer-analysis inputs and structural failures.

use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use super::model::WriterTokenError;
use super::registry::RegistryError;
use crate::finding::ByteSpanError;
use crate::path::RepositoryPath;
use crate::rust::RustSourceError;

/// One owned production Rust source supplied to writer analysis.
#[derive(Clone, Eq, PartialEq)]
pub struct WriterSource {
    path: RepositoryPath,
    bytes: Arc<[u8]>,
}

impl WriterSource {
    /// Construct a source from normalized path and owned immutable bytes.
    #[must_use]
    pub fn new(path: RepositoryPath, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            path,
            bytes: bytes.into(),
        }
    }

    pub(crate) const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for WriterSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterSource")
            .field("path", &self.path)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

/// Structural writer scan failures.
#[derive(Debug, Error)]
pub enum WriterScanError {
    /// The source path occurs more than once.
    #[error("writer source path occurs more than once")]
    DuplicateSource,
    /// The supplied sink registry is invalid.
    #[error("writer sink registry is invalid")]
    Registry(#[from] RegistryError),
    /// Rust parsing or production-range analysis failed.
    #[error("writer source could not be analyzed")]
    Rust(#[from] RustSourceError),
    /// A public Rust re-export could not be represented exactly.
    #[error("writer public re-export could not be analyzed")]
    ReexportSyntax,
    /// A source byte range could not be represented.
    #[error("writer source span is invalid")]
    Span(#[from] ByteSpanError),
    /// A source byte offset exceeds the stable 64-bit representation.
    #[error("writer source offset exceeds u64")]
    Offset,
    /// An identical-occurrence multiset exceeds the stable 32-bit ordinal.
    #[error("writer operation multiset exceeds u32")]
    Ordinal,
    /// A candidate name cannot be represented by the closed token grammar.
    #[error("writer candidate name is invalid")]
    Candidate(#[from] WriterTokenError),
}
