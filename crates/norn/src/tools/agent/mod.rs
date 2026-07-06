//! `SpawnAgent`, `SignalAgent`, `CloseAgent`, Fork tools.
//!
//! All four tools wrap the agent infrastructure delivered in N-014.
//! Implementations live in named submodules; this file only declares them
//! and re-exports the public surface.

pub mod coord;
pub(crate) mod delegation;
pub(super) mod fork_context;
pub(super) mod fork_launch;
pub(super) mod fork_outcome;
mod fork_seed;
pub mod fork_tool;
pub mod handle;
pub mod infra;
pub(crate) mod lifecycle;
pub mod reclaim;
pub mod spawn;
mod spawn_context;
pub(crate) mod spawn_launch;
pub(crate) mod spawn_outcome;

pub use self::coord::{
    CloseAgentTool, SIGNAL_AGENT_TOOL_NAME, SignalAgentTool, WAKE_AGENT_TOOL_NAME, WakeAgentTool,
};
pub(crate) use self::fork_outcome::ForkOutcome;
pub use self::fork_tool::{FORK_TOOL_NAME, ForkTool};
pub use self::handle::{AgentHandle, AgentHandles, AgentWakeRegistry, WakeRequestOutcome};
pub use self::infra::{AgentCancellation, AgentToolInfra};
pub(crate) use self::lifecycle::append_message_audit;
pub use self::reclaim::ReclaimOnResultDelivery;
pub use self::spawn::{SPAWN_TOOL_NAME, SpawnAgentTool};
