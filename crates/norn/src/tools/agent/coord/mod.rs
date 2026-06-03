//! Inter-agent coordination tools (NA-007 update of N-014) —
//! [`SignalAgentTool`], [`CloseAgentTool`].
//!
//! Close performs a depth-first post-order
//! shutdown of the target's whole subtree. Signal routes through the
//! child's [`crate::r#loop::inbound::InboundChannel`] when the parent
//! holds an [`crate::tools::agent::handle::AgentHandle`] for the
//! recipient, falling back to the [`crate::agent::mailbox::Mailbox`] for
//! peers and unknown targets.

mod close;
mod helpers;
mod signal;

#[cfg(test)]
pub(crate) mod test_support;

pub use close::CloseAgentTool;
pub use signal::SignalAgentTool;
