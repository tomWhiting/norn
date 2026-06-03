//! Agent-tagged provider events for multi-agent observability.
//!
//! [`ProviderEvent`] is the provider contract — raw SSE stream events
//! with no knowledge of agent identity. [`AgentEvent`] wraps a
//! [`ProviderEvent`] with the emitting agent's `id` and `role`, making
//! every event attributable in a multi-agent runtime.
//!
//! [`AgentEventSender`] ensures tagging is automatic: callers pass bare
//! [`ProviderEvent`] values, and the sender stamps them with the
//! agent's identity before broadcasting. A child sender is created
//! from a parent via [`AgentEventSender::for_child`], sharing the
//! underlying channel but carrying the child's identity.
//!
//! [`SharedAgentEventChannel`] is a [`ToolContext`](crate::tool::context::ToolContext)
//! extension holding the raw broadcast sender, so fork/spawn tools can
//! clone it and build child senders without threading the channel
//! through every function signature.

use std::sync::Arc;

use tokio::sync::broadcast;
use uuid::Uuid;

use super::events::ProviderEvent;

/// A [`ProviderEvent`] tagged with the identity of the agent that
/// produced it.
///
/// Every agent in the runtime — root, fork, spawn — emits
/// `AgentEvent` values on a shared broadcast channel. The TUI
/// dispatches on `agent_id` to route root events to the main scroll
/// region and child events to the activity panel.
#[derive(Clone, Debug)]
pub struct AgentEvent {
    /// Registry id of the emitting agent.
    pub agent_id: Uuid,
    /// Human-readable role label (e.g. `"root"`, `"fork/gpt-5.5"`).
    pub agent_role: Arc<str>,
    /// The underlying provider event.
    pub event: ProviderEvent,
}

/// Broadcast sender that auto-tags [`ProviderEvent`] values with agent
/// identity before sending.
///
/// Each agent holds its own `AgentEventSender` wrapping the same
/// underlying `broadcast::Sender<AgentEvent>`. The identity fields are
/// set at construction time and stamped onto every event — callers
/// cannot forget to tag.
#[derive(Clone, Debug)]
pub struct AgentEventSender {
    tx: broadcast::Sender<AgentEvent>,
    agent_id: Uuid,
    agent_role: Arc<str>,
}

impl AgentEventSender {
    /// Create a sender for a specific agent on the given channel.
    #[must_use]
    pub fn new(tx: broadcast::Sender<AgentEvent>, agent_id: Uuid, agent_role: String) -> Self {
        Self {
            tx,
            agent_id,
            agent_role: Arc::from(agent_role),
        }
    }

    /// Tag and broadcast a [`ProviderEvent`].
    ///
    /// The `agent_role` clone is a reference-count bump (`Arc<str>`),
    /// not a heap allocation.
    pub fn send(&self, event: ProviderEvent) {
        let _ = self.tx.send(AgentEvent {
            agent_id: self.agent_id,
            agent_role: Arc::clone(&self.agent_role),
            event,
        });
    }

    /// Create a child sender sharing the same broadcast channel but
    /// carrying a different agent identity.
    #[must_use]
    pub fn for_child(&self, agent_id: Uuid, agent_role: String) -> Self {
        Self {
            tx: self.tx.clone(),
            agent_id,
            agent_role: Arc::from(agent_role),
        }
    }

    /// The id of the agent this sender is tagged with.
    #[must_use]
    pub fn agent_id(&self) -> Uuid {
        self.agent_id
    }
}

/// Shared broadcast sender installed as a
/// [`ToolContext`](crate::tool::context::ToolContext) extension.
///
/// Fork and spawn tools read this extension to create child
/// [`AgentEventSender`] instances. The TUI driver installs it during
/// runtime setup; REPL and print modes install it when they need
/// streaming events.
#[derive(Clone, Debug)]
pub struct SharedAgentEventChannel(pub broadcast::Sender<AgentEvent>);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;

    #[test]
    fn sender_tags_events_with_agent_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        sender.send(ProviderEvent::TextDelta {
            text: "hello".to_string(),
        });
        let received = rx.try_recv().unwrap();
        assert_eq!(received.agent_id, Uuid::nil());
        assert_eq!(&*received.agent_role, "root");
        assert!(matches!(received.event, ProviderEvent::TextDelta { text } if text == "hello"));
    }

    #[test]
    fn for_child_shares_channel_with_different_identity() {
        let (tx, mut rx) = broadcast::channel::<AgentEvent>(16);
        let parent = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        let child_id = Uuid::from_u128(42);
        let child = parent.for_child(child_id, "fork/haiku".to_string());

        parent.send(ProviderEvent::TextDelta {
            text: "from parent".to_string(),
        });
        child.send(ProviderEvent::TextDelta {
            text: "from child".to_string(),
        });

        let first = rx.try_recv().unwrap();
        assert_eq!(first.agent_id, Uuid::nil());
        assert_eq!(&*first.agent_role, "root");

        let second = rx.try_recv().unwrap();
        assert_eq!(second.agent_id, child_id);
        assert_eq!(&*second.agent_role, "fork/haiku");
    }

    #[test]
    fn send_succeeds_with_no_receivers() {
        let (tx, rx) = broadcast::channel::<AgentEvent>(16);
        drop(rx);
        let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());
        sender.send(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        });
    }
}
