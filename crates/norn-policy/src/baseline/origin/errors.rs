//! Closed immutable-origin decode and authority failures.

use thiserror::Error;

use super::super::items::ItemGroupError;
use super::super::model::WriterSpanError;
use super::super::production::ProductionFactError;
use crate::digest::CanonicalJsonError;
use crate::phase_lock::GitObjectIdError;
use crate::strict_json::StrictJsonError;

/// Strict immutable-origin failures.
#[derive(Debug, Error)]
pub enum OriginError {
    /// JSON was malformed, ambiguous, or outside the closed schema.
    #[error("origin ledger is not valid strict JSON")]
    Json(#[source] StrictJsonError),
    /// The immutable-origin schema is unsupported.
    #[error("origin schema version {actual} is unsupported")]
    SchemaVersion {
        /// Observed schema version.
        actual: u32,
    },
    /// The analyzer identity does not match the compiled evaluator.
    #[error("origin analyzer identity does not match this evaluator")]
    AnalyzerVersion,
    /// The digest identity does not match the compiled evaluator.
    #[error("origin digest identity does not match this evaluator")]
    DigestVersion,
    /// The ledger names a commit other than the accepted P1 base.
    #[error("origin commit does not match the accepted P1 base")]
    BaseCommit,
    /// The ledger names a tree other than the accepted P1 base.
    #[error("origin tree does not match the accepted P1 base")]
    BaseTree,
    /// A compiled base identifier was structurally impossible.
    #[error("compiled P1 base identity is invalid")]
    BaseIdentity(#[source] GitObjectIdError),
    /// The validated repository policy could not be normalized canonically.
    #[error("repository policy authority digest could not be computed")]
    RepositoryPolicyDigest(#[source] CanonicalJsonError),
    /// The ledger names a generated-include registry other than the exact P1 authority.
    #[error("origin generated-include registry does not match the accepted P1 authority")]
    GeneratedRegistry,
    /// Exact source rows do not reproduce their retained inventory digest.
    #[error("origin source-inventory rows do not match their digest")]
    SourceInventoryDigest,
    /// Exact source rows were unsorted or repeated a path.
    #[error("origin source-inventory rows are not strictly sorted at row {index}")]
    SourceInventoryOrder {
        /// First invalid row.
        index: usize,
    },
    /// One exact source row had no reachable classification.
    #[error("origin source-inventory classification is invalid at row {index}")]
    SourceInventoryClassification {
        /// Invalid source row.
        index: usize,
    },
    /// Compile-test fixture rows were unsorted or selected one path repeatedly.
    #[error("origin compile-test fixture rows are not strictly sorted at row {index}")]
    CompileTestFixtureOrder {
        /// First invalid row.
        index: usize,
    },
    /// A compile-test fixture does not name an exclusive test source and harness.
    #[error("origin compile-test fixture source is invalid at row {index}")]
    CompileTestFixtureSource {
        /// Invalid fixture row.
        index: usize,
    },
    /// A production row's identity was forged or stale.
    #[error("production origin identity mismatch at row {index}")]
    ProductionId {
        /// Zero-based row index.
        index: usize,
    },
    /// A production row contained invalid canonical facts.
    #[error("production origin fact is invalid at row {index}")]
    ProductionFact {
        /// Zero-based row index.
        index: usize,
        /// Canonical conversion or validation failure.
        #[source]
        source: ProductionFactError,
    },
    /// An item aggregate's identity was forged or stale.
    #[error("item-group origin identity mismatch at row {index}")]
    ItemGroupId {
        /// Zero-based row index.
        index: usize,
    },
    /// An item aggregate contained impossible multiplicities.
    #[error("item-group origin fact is invalid at row {index}")]
    ItemGroup {
        /// Zero-based row index.
        index: usize,
        /// Aggregate validation failure.
        #[source]
        source: ItemGroupError,
    },
    /// A debt row's identity was forged or stale.
    #[error("debt origin identity mismatch at row {index}")]
    DebtId {
        /// Zero-based row index.
        index: usize,
    },
    /// A writer row's identity was forged or stale.
    #[error("writer origin identity mismatch at row {index}")]
    WriterId {
        /// Zero-based row index.
        index: usize,
    },
    /// A writer row's operation identity disagreed with its canonical fields.
    #[error("writer operation identity mismatch at row {index}")]
    WriterOperationId {
        /// Zero-based row index.
        index: usize,
    },
    /// A writer row had a reversed span.
    #[error("writer origin span is invalid at row {index}")]
    WriterSpan {
        /// Zero-based row index.
        index: usize,
        /// Structural span failure.
        #[source]
        source: WriterSpanError,
    },
    /// Production rows were unsorted or repeated a path.
    #[error("production origin rows are not strictly path-sorted at row {index}")]
    ProductionOrder {
        /// First invalid row.
        index: usize,
    },
    /// Item groups were unsorted or repeated a stable key.
    #[error("item-group origin rows are not strictly sorted at row {index}")]
    ItemGroupOrder {
        /// First invalid row.
        index: usize,
    },
    /// Debt multiset rows were unsorted or duplicated.
    #[error("debt origin rows are not strictly sorted at row {index}")]
    DebtOrder {
        /// First invalid row.
        index: usize,
    },
    /// Writer rows were unsorted or duplicated.
    #[error("writer origin rows are not strictly sorted at row {index}")]
    WriterOrder {
        /// First invalid row.
        index: usize,
    },
    /// Two fact families produced the same domain-separated origin ID.
    #[error("origin ledger contains a duplicate origin identity")]
    DuplicateOriginId,
}

/// Caller-computed authority mismatch.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OriginAuthorityError {
    /// Normalized repository policy differs from the origin pin.
    #[error("origin repository-policy digest does not match")]
    RepositoryPolicy,
    /// Complete source inventory differs from the origin pin.
    #[error("origin source-inventory digest does not match")]
    SourceInventory,
    /// Generated-include technical registry differs from the origin pin.
    #[error("origin generated-include registry digest does not match")]
    GeneratedRegistry,
}

/// Normalized-origin digest failure.
#[derive(Debug, Error)]
pub enum OriginDigestError {
    /// The closed Rust value could not be represented as JSON.
    #[error("origin value could not be serialized")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON encoding failed.
    #[error("origin value could not be encoded canonically")]
    Canonical(#[source] CanonicalJsonError),
}

/// Deterministic immutable-origin document encoding failure.
#[derive(Debug, Error)]
pub enum OriginEncodeError {
    /// The closed ledger could not be represented as JSON.
    #[error("origin ledger could not be encoded")]
    Serialization(#[source] serde_json::Error),
}
