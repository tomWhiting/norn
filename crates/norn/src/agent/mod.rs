//! Agent registry, coordination, forking, and messaging.

pub use crate::error::AgentError;

pub(crate) mod arming;
pub(crate) mod assembly;
mod build_support;
pub mod builder;
mod builder_setters;
pub mod child_policy;
pub mod fork;
mod fork_context_filter;
pub mod goals;
pub mod handle;
pub mod instance;
mod mcp;
pub mod message_router;
pub mod output;
pub mod pending_messages;
pub mod process_delivery;
pub(crate) mod prompt_install;
pub mod registry;
pub(crate) mod registry_assembly;
pub mod result_channel;
pub mod resume;
pub(crate) mod session_open;
pub mod session_spec;
pub mod variants;

pub use crate::r#loop::config::TruncationKind;
pub use crate::r#loop::inbound::{ChannelMessage, InboundSender, MessageKind};
pub use assembly::validate_workspace_root;
pub use builder::AgentBuilder;
pub use child_policy::{
    ChildLoopConfig, ChildPolicy, CoordinationEnvelope, DelegationBudget, MessagingScope,
};
pub use fork::{
    ContextFilter, FORK_SYNTHETIC_RESULT_MESSAGE, FORK_SYSTEM_PREAMBLE, ForkIdentity,
    ForkRequirement, ParentSystemInstruction, build_fork_output_schema, build_fork_preamble,
    combine_system_instruction, format_fork_failure, format_fork_result, format_spawn_failure,
    format_spawn_result, inject_synthetic_fork_result, slugify_requirement_name,
    verify_no_orphan_tool_calls,
};
pub use goals::{ContinuationPolicy, Goal, GoalSignal, GoalTracker};
pub use handle::{AgentHandle, ResolvedAgentInfo};
pub use instance::{Agent, AgentParts};
pub use message_router::{MessageRouter, RouteError};
pub use output::{AgentOutput, AgentStopReason, RunOutcome};
pub use pending_messages::{
    AGENT_MESSAGE_DEQUEUED_EVENT_TYPE, AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessage,
    PendingAgentMessageLifecycle, PendingAgentMessages, append_pending_message_audit,
};
pub use registry::{AgentEntry, AgentRegistry, AgentStatus, AgentTombstone, SpawnGuard};
pub use result_channel::{ChildAgentResult, ChildResultSender, frame_child_result};
pub use resume::rebuild_action_log;
pub use session_spec::SessionSpec;

#[cfg(test)]
mod credential_affinity_tests;
#[cfg(test)]
mod fork_canonical_resolution_tests;
#[cfg(test)]
mod fork_d3_projection_tests;
#[cfg(test)]
mod fork_provider_compaction_tests;
