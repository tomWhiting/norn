//! Strict reviewed writer-family authority.

mod vocabulary;

use std::str::Utf8Error;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::classify::{
    classification_has_valid_structure, validate_required_classifications_for_operations,
};
use super::model::{WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION};
use super::{
    ClassificationIssue, RegistryError, WriterClassification, WriterOperationId,
    builtin_sink_registry,
};
use crate::baseline::OriginLedger;
use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::version::DIGEST_VERSION;
use vocabulary::{VocabularyIssue, VocabularyTable, WriterFamilyVocabulary};

/// Complete reviewed classification authority for immutable P1 writer operations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriterFamilyRegistry {
    schema_version: u32,
    algorithms: WriterFamilyAlgorithms,
    sink_registry: Digest,
    writer_resolutions: Digest,
    vocabulary: WriterFamilyVocabulary,
    classifications: Vec<WriterClassification>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WriterFamilyAlgorithms {
    writer: String,
    digest: String,
}

impl WriterFamilyRegistry {
    /// Construct the deterministic P1 registry from reviewed classifications.
    ///
    /// Rows are normalized into operation-identity order before the same
    /// structural validation used by the strict decoder. Vocabularies remain
    /// exact reviewed input and must already be sorted and unique. This
    /// function never assigns a family, primitive, or review token.
    ///
    /// # Errors
    ///
    /// Rejects duplicate operation identities, invalid shared-family edges,
    /// open or inconsistent vocabularies, or an invalid compiled sink registry.
    pub fn author_p1(
        writer_resolutions: Digest,
        families: Vec<super::WriterToken>,
        shared_primitives: Vec<super::WriterToken>,
        cleanup_reviews: Vec<super::WriterToken>,
        false_positive_reviews: Vec<super::WriterToken>,
        mut classifications: Vec<WriterClassification>,
    ) -> Result<Self, WriterFamilyRegistryError> {
        classifications.sort_by_key(|row| row.operation);
        WriterFamilyDocument {
            schema_version: WRITER_SCHEMA_VERSION,
            algorithms: WriterFamilyAlgorithms {
                writer: WRITER_ANALYZER_VERSION.to_owned(),
                digest: DIGEST_VERSION.to_owned(),
            },
            sink_registry: builtin_sink_registry()
                .map_err(WriterFamilyRegistryError::ReviewedSinkRegistry)?
                .digest(),
            writer_resolutions,
            vocabulary: WriterFamilyVocabulary::new(
                families,
                shared_primitives,
                cleanup_reviews,
                false_positive_reviews,
            ),
            classifications,
        }
        .validate()
    }

    /// Decode strict TOML and bind it to this evaluator's built-in sink registry.
    ///
    /// # Errors
    ///
    /// Rejects non-UTF-8, malformed or open-schema TOML, incompatible algorithm
    /// identities, sink-registry drift, unsorted or duplicate operation rows,
    /// structurally invalid shared-family edges, and any open, overlapping, or
    /// unused vocabulary token.
    pub fn decode_p1(bytes: &[u8]) -> Result<Self, WriterFamilyRegistryError> {
        let text = std::str::from_utf8(bytes).map_err(WriterFamilyRegistryError::Utf8)?;
        let document: WriterFamilyDocument =
            toml::from_str(text).map_err(WriterFamilyRegistryError::Toml)?;
        document.validate()
    }

    /// Return the closed writer-family schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the reviewed built-in sink-registry identity.
    #[must_use]
    pub const fn sink_registry_digest(&self) -> Digest {
        self.sink_registry
    }

    /// Return the exact writer-resolution authority used to derive operations.
    #[must_use]
    pub const fn writer_resolutions_digest(&self) -> Digest {
        self.writer_resolutions
    }

    /// Borrow the exact declared artifact-family vocabulary.
    #[must_use]
    pub fn families(&self) -> &[super::WriterToken] {
        self.vocabulary.families()
    }

    /// Borrow the exact declared shared-primitive vocabulary.
    #[must_use]
    pub fn shared_primitives(&self) -> &[super::WriterToken] {
        self.vocabulary.shared_primitives()
    }

    /// Borrow the exact declared reviewed-cleanup vocabulary.
    #[must_use]
    pub fn cleanup_reviews(&self) -> &[super::WriterToken] {
        self.vocabulary.cleanup_reviews()
    }

    /// Borrow the exact declared false-positive review vocabulary.
    #[must_use]
    pub fn false_positive_reviews(&self) -> &[super::WriterToken] {
        self.vocabulary.false_positive_reviews()
    }

    /// Borrow classifications in strictly increasing operation-ID order.
    #[must_use]
    pub fn classifications(&self) -> &[WriterClassification] {
        &self.classifications
    }

    /// Validate exact one-row-per-origin coverage and classification structure.
    #[must_use]
    pub fn validate_against_origin(&self, origin: &OriginLedger) -> Vec<ClassificationIssue> {
        validate_required_classifications_for_operations(
            origin
                .writer_operations()
                .iter()
                .map(|operation| WriterOperationId::new(operation.operation_id())),
            &self.classifications,
        )
    }

    /// Hash normalized writer-family semantics rather than TOML formatting.
    ///
    /// # Errors
    ///
    /// Returns an error only if the closed value cannot be represented as
    /// canonical JSON.
    pub fn normalized_digest(&self) -> Result<Digest, WriterFamilyDigestError> {
        let value = serde_json::to_value(self).map_err(WriterFamilyDigestError::Serialization)?;
        digest_json(&value).map_err(WriterFamilyDigestError::Canonical)
    }

    /// Encode one deterministic checked-in P1 TOML document.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the closed registry cannot be encoded.
    pub fn encode_p1(&self) -> Result<Vec<u8>, WriterFamilyEncodeError> {
        let mut text = toml::to_string(self).map_err(WriterFamilyEncodeError::Serialization)?;
        text.truncate(text.trim_end_matches('\n').len());
        text.push('\n');
        Ok(text.into_bytes())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriterFamilyDocument {
    schema_version: u32,
    algorithms: WriterFamilyAlgorithms,
    sink_registry: Digest,
    writer_resolutions: Digest,
    vocabulary: WriterFamilyVocabulary,
    classifications: Vec<WriterClassification>,
}

impl WriterFamilyDocument {
    fn validate(self) -> Result<WriterFamilyRegistry, WriterFamilyRegistryError> {
        if self.schema_version != WRITER_SCHEMA_VERSION {
            return Err(WriterFamilyRegistryError::SchemaVersion {
                actual: self.schema_version,
            });
        }
        if self.algorithms.writer != WRITER_ANALYZER_VERSION {
            return Err(WriterFamilyRegistryError::WriterAnalyzerVersion);
        }
        if self.algorithms.digest != DIGEST_VERSION {
            return Err(WriterFamilyRegistryError::DigestVersion);
        }
        let reviewed =
            builtin_sink_registry().map_err(WriterFamilyRegistryError::ReviewedSinkRegistry)?;
        if self.sink_registry != reviewed.digest() {
            return Err(WriterFamilyRegistryError::SinkRegistry);
        }
        for (index, pair) in self.classifications.windows(2).enumerate() {
            if pair[0].operation >= pair[1].operation {
                return Err(WriterFamilyRegistryError::ClassificationOrder { index: index + 1 });
            }
        }
        for (index, row) in self.classifications.iter().enumerate() {
            if !classification_has_valid_structure(&row.classification) {
                return Err(WriterFamilyRegistryError::SharedFamilies { index });
            }
        }
        self.vocabulary
            .validate(&self.classifications)
            .map_err(vocabulary_error)?;
        Ok(WriterFamilyRegistry {
            schema_version: self.schema_version,
            algorithms: self.algorithms,
            sink_registry: self.sink_registry,
            writer_resolutions: self.writer_resolutions,
            vocabulary: self.vocabulary,
            classifications: self.classifications,
        })
    }
}

fn vocabulary_error(issue: VocabularyIssue) -> WriterFamilyRegistryError {
    match issue {
        VocabularyIssue::Order { table, index } => WriterFamilyRegistryError::VocabularyOrder {
            table: match table {
                VocabularyTable::Families => "families",
                VocabularyTable::SharedPrimitives => "shared_primitives",
                VocabularyTable::CleanupReviews => "cleanup_reviews",
                VocabularyTable::FalsePositiveReviews => "false_positive_reviews",
            },
            index,
        },
        VocabularyIssue::Overlap => WriterFamilyRegistryError::VocabularyOverlap,
        VocabularyIssue::UndeclaredReference => {
            WriterFamilyRegistryError::UndeclaredVocabularyReference
        }
        VocabularyIssue::UnusedDeclaration => WriterFamilyRegistryError::UnusedVocabularyEntry,
        VocabularyIssue::SharedPrimitiveEdges => {
            WriterFamilyRegistryError::SharedPrimitiveEdgeConflict
        }
    }
}

/// Strict writer-family authority decoding failure.
#[derive(Debug, Error)]
pub enum WriterFamilyRegistryError {
    /// Bytes are not UTF-8.
    #[error("writer-family authority is not UTF-8")]
    Utf8(#[source] Utf8Error),
    /// TOML is malformed, duplicate, unknown, or type-invalid.
    #[error("writer-family authority is not valid closed-schema TOML")]
    Toml(#[source] toml::de::Error),
    /// The schema version is unsupported.
    #[error("writer-family schema version {actual} is unsupported")]
    SchemaVersion {
        /// Observed schema version.
        actual: u32,
    },
    /// The writer analyzer identity differs from this evaluator.
    #[error("writer-family analyzer identity does not match")]
    WriterAnalyzerVersion,
    /// The canonical digest identity differs from this evaluator.
    #[error("writer-family digest identity does not match")]
    DigestVersion,
    /// The compiled reviewed sink registry is internally invalid.
    #[error("built-in writer sink registry is invalid")]
    ReviewedSinkRegistry(#[source] RegistryError),
    /// The document was reviewed against another sink registry.
    #[error("writer-family sink registry does not match this evaluator")]
    SinkRegistry,
    /// Classification rows are not strictly sorted by operation identity.
    #[error("writer-family classifications are not strictly sorted at row {index}")]
    ClassificationOrder {
        /// First invalid row.
        index: usize,
    },
    /// A shared primitive lacks two or more sorted unique family edges.
    #[error("writer-family shared edges are invalid at row {index}")]
    SharedFamilies {
        /// Invalid row.
        index: usize,
    },
    /// One vocabulary was not strictly sorted and unique.
    #[error("writer-family {table} vocabulary is not strictly sorted at row {index}")]
    VocabularyOrder {
        /// Closed vocabulary class.
        table: &'static str,
        /// First invalid row.
        index: usize,
    },
    /// One token was declared in more than one semantic vocabulary.
    #[error("writer-family vocabularies overlap")]
    VocabularyOverlap,
    /// A classification references a token absent from its matching vocabulary.
    #[error("writer-family classification references an undeclared vocabulary token")]
    UndeclaredVocabularyReference,
    /// A vocabulary token is not referenced by any classification.
    #[error("writer-family vocabulary contains an unused token")]
    UnusedVocabularyEntry,
    /// One shared primitive was assigned inconsistent inbound family edges.
    #[error("writer-family shared primitive has inconsistent inbound edges")]
    SharedPrimitiveEdgeConflict,
}

/// Normalized writer-family digest failure.
#[derive(Debug, Error)]
pub enum WriterFamilyDigestError {
    /// The closed Rust value could not be represented as JSON.
    #[error("writer-family authority could not be serialized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("writer-family authority could not be encoded canonically")]
    Canonical(#[source] CanonicalJsonError),
}

/// Deterministic writer-family document encoding failure.
#[derive(Debug, Error)]
pub enum WriterFamilyEncodeError {
    /// The closed registry could not be represented as TOML.
    #[error("writer-family authority could not be encoded")]
    Serialization(#[source] toml::ser::Error),
}
