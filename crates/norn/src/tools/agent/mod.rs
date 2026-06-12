//! `SpawnAgent`, `SignalAgent`, `CloseAgent`, Fork tools.
//!
//! All four tools wrap the agent infrastructure delivered in N-014.
//! Implementations live in named submodules; this file only declares them
//! and re-exports the public surface.

pub mod coord;
pub(super) mod fork_pipeline;
mod fork_seed;
pub mod fork_tool;
pub mod handle;
pub mod infra;
mod lifecycle;
pub mod reclaim;
pub mod spawn;
mod spawn_context;
mod spawn_outcome;

pub use self::coord::{CloseAgentTool, SignalAgentTool};
pub(crate) use self::fork_pipeline::ForkOutcome;
pub use self::fork_tool::{FORK_TOOL_NAME, ForkTool};
pub use self::handle::{AgentHandle, AgentHandles};
pub use self::infra::AgentToolInfra;
pub(crate) use self::lifecycle::append_message_audit;
pub use self::reclaim::ReclaimOnResultDelivery;
pub use self::spawn::{SPAWN_TOOL_NAME, SpawnAgentTool};
