//! Agent registry, coordination, forking, and messaging.

pub use crate::error::AgentError;

pub(crate) mod assembly;
pub mod builder;
mod builder_setters;
pub mod child_policy;
pub mod fork;
pub mod goals;
pub mod handle;
pub mod instance;
pub mod message_router;
pub mod monitor;
pub mod output;
pub mod registry;
pub mod result_channel;
pub mod resume;
pub mod session_spec;

pub use crate::r#loop::config::TruncationKind;
pub use crate::r#loop::inbound::{ChannelMessage, InboundSender, MessageKind};
pub use assembly::validate_workspace_root;
pub use builder::AgentBuilder;
pub use child_policy::{ChildPolicy, CoordinationEnvelope, DelegationBudget, MessagingScope};
pub use fork::{
    ContextFilter, FORK_SYNTHETIC_RESULT_MESSAGE, FORK_SYSTEM_PREAMBLE, ForkRequirement,
    ParentSystemInstruction, build_fork_output_schema, combine_system_instruction,
    format_fork_failure, format_fork_result, format_spawn_failure, format_spawn_result,
    inject_synthetic_fork_result, slugify_requirement_name, verify_no_orphan_tool_calls,
};
pub use goals::{ContinuationPolicy, Goal, GoalSignal, GoalTracker, ScheduleEntry, Scheduler};
pub use handle::{AgentHandle, ResolvedAgentInfo};
pub use instance::Agent;
pub use message_router::{MessageRouter, RouteError};
pub use monitor::{MonitorConfig, MonitorHandle, MonitorStatus, run_monitored};
pub use output::{AgentOutput, AgentStopReason, RunOutcome};
pub use registry::{AgentEntry, AgentRegistry, AgentStatus, AgentTombstone, SpawnGuard};
pub use result_channel::{ChildAgentResult, ChildResultSender, frame_child_result};
pub use resume::rebuild_action_log;
pub use session_spec::SessionSpec;
