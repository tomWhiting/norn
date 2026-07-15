//! Reviewed dispositions for unresolved writer candidates.

mod authority;
mod coverage;
mod model;
mod review;

pub use authority::{
    WriterResolutionAuthority, WriterResolutionAuthorityError, WriterResolutionDigestError,
    WriterResolutionEncodeError,
};
pub use coverage::{WriterResolutionCoverage, WriterResolutionCoverageError};
pub use model::{
    WRITER_RESOLUTION_AUTHORITY_PATH, WRITER_RESOLUTION_SCHEMA_VERSION, WriterResolution,
    WriterResolutionDisposition,
};
pub use review::{
    WRITER_RESOLUTION_REVIEW_INVENTORY_PATH, WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION,
    WriterResolutionReviewDigestError, WriterResolutionReviewEncodeError,
    WriterResolutionReviewInventory, WriterResolutionReviewInventoryError,
    WriterResolutionReviewRow,
};
