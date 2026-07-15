//! Conservative filesystem-writer discovery and classification.

mod candidate;
pub(crate) mod classify;
mod families;
mod findings;
pub(crate) mod identity;
mod imports;
mod input;
mod model;
mod registry;
mod resolution;
mod scan;
mod syntax;

pub use candidate::{
    WriterCandidate, WriterCandidateForm, WriterCandidateId, WriterCandidateSemantics,
};
pub use classify::{
    ClassificationIssue, WriterClassification, WriterClassificationKind,
    validate_writer_classifications,
};
pub use families::{
    WriterFamilyDigestError, WriterFamilyEncodeError, WriterFamilyRegistry,
    WriterFamilyRegistryError,
};
pub use findings::{WriterFindingError, canonical_writer_findings};
pub use input::{WriterScanError, WriterSource};
pub use model::{
    FlowClass, OperationKind, SinkDiscovery, SinkOrigin, UnknownSinkReason,
    WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION, WriterInventory, WriterOperation,
    WriterOperationId, WriterRole, WriterSourceIdentity, WriterToken, WriterTokenError,
};
pub use registry::{
    DefinitionSpec, ReceiverConstraint, RegistryError, SinkRegistry, SinkSelector, SinkSpec,
    builtin_sink_registry,
};
pub use resolution::{
    WRITER_RESOLUTION_AUTHORITY_PATH, WRITER_RESOLUTION_REVIEW_INVENTORY_PATH,
    WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION, WRITER_RESOLUTION_SCHEMA_VERSION, WriterResolution,
    WriterResolutionAuthority, WriterResolutionAuthorityError, WriterResolutionCoverage,
    WriterResolutionCoverageError, WriterResolutionDigestError, WriterResolutionDisposition,
    WriterResolutionEncodeError, WriterResolutionReviewDigestError,
    WriterResolutionReviewEncodeError, WriterResolutionReviewInventory,
    WriterResolutionReviewInventoryError, WriterResolutionReviewRow,
};
pub use scan::analyze_writers;
