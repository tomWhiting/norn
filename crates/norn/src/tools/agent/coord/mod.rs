//! Inter-agent coordination tools (NA-007 update of N-014, messaging
//! replaced in Wave 3 W3.2) — [`SignalAgentTool`], [`CloseAgentTool`].
//!
//! Close performs a depth-first post-order shutdown of the target's whole
//! subtree. `signal_agent` routes through the recipient's
//! [`crate::r#loop::inbound::InboundChannel`] via the workspace-shared
//! [`MessageRouter`](crate::agent::message_router::MessageRouter), with
//! who-may-message-whom enforced from the sender's granted
//! [`MessagingScope`](crate::agent::child_policy::MessagingScope) against
//! registry ground truth. A resolved, in-scope recipient with no live route
//! is queued into the shared pending-message store for the next resumed
//! loop step; finished recipients (terminal or reclaimed) produce a
//! structured delivery failure carrying their status and completion time.

mod close;
mod helpers;
mod signal_agent;
mod wake;

#[cfg(test)]
pub(crate) mod test_support;

pub use close::CloseAgentTool;
pub(crate) use helpers::sender_attribution;
pub use signal_agent::{SIGNAL_AGENT_TOOL_NAME, SignalAgentTool};
pub use wake::{WAKE_AGENT_TOOL_NAME, WakeAgentTool};
