//! Pure repository-policy evaluation for Norn.

pub mod authoring;
pub mod baseline;
pub mod config;
pub mod debt;
pub mod digest;
mod evaluation_input;
pub mod evaluator;
pub mod facts;
pub mod finding;
pub mod path;
pub mod phase_lock;
pub mod redaction;
pub mod responses_contract;
pub mod rust;
pub mod snapshot;
pub mod strict_json;
pub mod version;
pub mod writers;

pub use digest::{Digest, digest_bytes, digest_json};
pub use evaluation_input::{
    CompleteCurrentSnapshot, GitLeafMode, GitTreeLeaf, GitTreeLeafError,
    P1_BASE_GIT_INVENTORY_IDENTITY, P1BaseSnapshot, P1BaseSnapshotError, P1EvaluationInput,
};
pub use evaluator::{
    AuthorityIssue, CurrentFactIssue, InvalidPolicy, PolicyAuthority, PolicyInvalidReason,
    PolicyReport, PolicyState, evaluate_p1,
};
pub use finding::{ByteSpan, Finding, FindingCode};
pub use path::{RepositoryPath, RepositoryPathError};
pub use phase_lock::{CampaignPhase, GitObjectId, PhaseLock};
pub use responses_contract::{ResponsesContractAuthority, ResponsesContractError};
pub use rust::{
    CfgError, CfgTruth, LocError, ModuleShapeKind, ModuleShapeViolation, ProductionMetrics,
    RustSource, RustSourceError, SourceRange, evaluate_cfg, module_shape, production_metrics,
};
pub use snapshot::{EntryKind, OwnedSnapshot, SnapshotEntry, SnapshotError};
