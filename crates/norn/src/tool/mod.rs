//! Tool trait, lifecycle, registry, and scheduling.

pub use norn_macros::{ToolArgs, tool_follow_ups};

pub use self::context::{FlagEntry, ToolContext, ToolFlag};
pub use self::envelope::{
    DiagnosticReport, ENVELOPE_DESCRIPTION_KEY, ENVELOPE_METADATA_KEY, EnvelopeSplit, FileChange,
    FileChangeType, InboundMessage, RuntimeInputs, ToolEnvelope, split_envelope_fields,
    wrap_schema_with_envelope,
};
pub use self::follow_up::{BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction};
pub use self::lifecycle::{
    Advisory, AdvisorySeverity, CheckOverride, PostCheckResult, PostValidateMode,
    PostValidateOutcome, PreValidateOutcome, RuntimeOnSuccessAction, RuntimePostValidateCheck,
    RuntimePreValidateCheck,
};
pub use self::registry::ToolRegistry;
pub use self::risk::{BashRiskTier, classify_risk};
pub use self::scheduling::{ExecutionStep, SchedulingPlan, ToolEffect};
pub use self::traits::{Tool, ToolCategory, ToolOutput};
pub use crate::error::ToolError;

pub mod availability;
pub mod context;
pub mod envelope;

pub mod follow_up;
pub mod lifecycle;
pub mod registry;
pub mod risk;
pub mod scheduling;
pub mod traits;
