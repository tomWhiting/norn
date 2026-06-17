//! Tool trait, lifecycle, registry, catalog, and scheduling.

pub use norn_macros::{ToolArgs, tool_follow_ups};

pub use self::catalog::{
    CommandSchema, SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras, ToolFieldHint,
    composite_commands,
};
pub use self::composite::{CompositeTool, assert_conservative_effect_covers_all_commands};
pub use self::context::{FlagEntry, ToolContext, ToolFlag};
pub use self::envelope::{
    DiagnosticReport, ENVELOPE_DESCRIPTION_KEY, ENVELOPE_METADATA_KEY, EnvelopeSplit, FileChange,
    FileChangeType, InboundMessage, RuntimeInputs, ToolEnvelope, split_envelope_fields,
    wrap_schema_with_envelope,
};
pub use self::failure::{ToolErrorKind, ToolErrorPayload};
pub use self::follow_up::{
    BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction, FollowUpArgsMode,
};
pub use self::lifecycle::{
    Advisory, AdvisorySeverity, BlockDecision, CheckOverride, PostCheckResult, PostValidateMode,
    PostValidateOutcome, PreValidateOutcome, RuntimeOnSuccessAction, RuntimePostValidateCheck,
    RuntimePreValidateCheck,
};
pub use self::registry::ToolRegistry;
pub use self::risk::{BashRiskTier, classify_risk};
pub use self::scheduling::{ExecutionStep, SchedulingPlan, ToolEffect};
pub use self::traits::{Tool, ToolCategory, ToolOutput};
pub use crate::error::ToolError;

pub mod availability;
pub mod catalog;
pub mod composite;
pub mod context;
pub mod envelope;
pub mod failure;

pub mod follow_up;
pub mod lifecycle;
pub mod output_budget;
pub mod registry;
pub mod risk;
pub mod scheduling;
pub mod traits;
