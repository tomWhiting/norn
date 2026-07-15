//! Immutable P1 origin facts and reviewed legacy governance.

mod evaluate;
mod exact_base;
mod governance;
mod items;
mod model;
mod origin;
mod production;
mod reconstruct;
mod token;

pub use evaluate::{
    CurrentRepositoryFacts, LegacyDisposition, LegacyEvaluation, LegacyEvaluationError,
    LegacyIssue, LegacyIssueCode, LegacyKind, LocCeilings, LocCeilingsError, evaluate_legacy,
};
pub use exact_base::{
    ExactP1Base, ExactP1BaseError, GeneratedRegistryIdentityError,
    P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    generated_registry_technical_identity,
};
pub use governance::{
    GovernanceAnchorError, GovernanceAuthoringError, GovernanceDigestError, GovernanceError,
    GovernanceLinkError, GovernanceTable, GovernanceTransitionError, LegacyGovernance,
    LegacyGovernanceEntry, LegacyState, P1_GOVERNANCE_ANCHOR_IDENTITY, P1GovernanceReview,
    ReviewedDebtGovernanceRow, ReviewedGovernanceAnchor, ReviewedLocGovernanceRow,
};
pub use items::{
    ItemComparisonError, ItemComparisonSide, ItemGroupError, ItemGroupFact, ItemReclassification,
    compare_item_groups,
};
pub use model::{
    DebtOriginFact, ORIGIN_SCHEMA_VERSION, OriginId, OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE,
    WriterOperationFact, WriterSpanError,
};
pub use origin::{OriginAuthorityError, OriginDigestError, OriginEncodeError, OriginError};
pub use production::{ProductionFactError, ProductionFileFact, ProductionLocClass};
pub use reconstruct::{BaselineFactsError, RepositoryBaselineFacts};
pub use token::{GovernanceToken, GovernanceTokenError};
