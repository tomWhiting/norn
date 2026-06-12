//! Inter-agent coordination tools (NA-007 update of N-014) —
//! [`SignalAgentTool`], [`CloseAgentTool`].
//!
//! Close performs a depth-first post-order
//! shutdown of the target's whole subtree. Signal routes through the
//! child's [`crate::r#loop::inbound::InboundChannel`] when the parent
//! holds an [`crate::tools::agent::handle::AgentHandle`] for the
//! recipient; without a handle there is no delivery path, so signalling
//! fails honestly rather than queueing where nothing drains. Finished
//! recipients (terminal or reclaimed) produce a structured delivery
//! failure carrying their status and completion time.

mod close;
mod helpers;
mod signal;

#[cfg(test)]
pub(crate) mod test_support;

pub use close::CloseAgentTool;
pub use signal::SignalAgentTool;
