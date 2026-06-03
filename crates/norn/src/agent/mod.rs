//! Agent registry, coordination, forking, and messaging.

pub use crate::error::AgentError;

pub mod builder;
pub mod fork;
pub mod goals;
pub mod mailbox;
pub mod monitor;
pub mod output;
pub mod registry;
pub mod result_channel;

pub use builder::{Agent, AgentBuilder};
pub use fork::{
    ContextFilter, FORK_SYNTHETIC_RESULT_MESSAGE, FORK_SYSTEM_PREAMBLE, ForkRequirement,
    ParentSystemInstruction, build_fork_output_schema, combine_system_instruction,
    format_fork_failure, format_fork_result, format_spawn_failure, format_spawn_result,
    inject_synthetic_fork_result, slugify_requirement_name, verify_no_orphan_tool_calls,
};
pub use goals::{ContinuationPolicy, Goal, GoalSignal, GoalTracker, ScheduleEntry, Scheduler};
pub use mailbox::{Mailbox, MailboxMessage};
pub use monitor::{MonitorConfig, MonitorHandle, MonitorStatus, run_monitored};
pub use output::{AgentOutput, AgentStopReason};
pub use registry::{AgentEntry, AgentRegistry, AgentStatus, SpawnGuard};
pub use result_channel::{CHILD_RESULT_CHANNEL_CAPACITY, ChildAgentResult, ChildResultSender};
