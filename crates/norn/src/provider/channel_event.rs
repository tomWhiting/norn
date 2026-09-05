//! Typed observations of persisted external-channel input, distinct from agent messages.

use serde::Serialize;
use uuid::Uuid;

use crate::session::events::EventId;

/// External input that has entered the recipient's persisted conversation.
#[derive(Clone, Serialize)]
pub struct McpChannelDeliveryEvent {
    /// Exact persisted framed `UserMessage` event, authoritative for replay.
    pub event_id: EventId,
    /// Host-created external message identity.
    pub message_id: Uuid,
    /// Running agent that owns this inbox.
    pub recipient_id: Uuid,
    /// Host-configured MCP server name.
    pub source: String,
    /// Host-created connection generation.
    pub generation: u64,
    /// Admission order within the recipient inbox.
    pub sequence: u64,
    /// Raw external text for display; it remains untrusted input.
    pub content: String,
}

impl std::fmt::Debug for McpChannelDeliveryEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpChannelDeliveryEvent")
            .field("event_id", &self.event_id)
            .field("message_id", &self.message_id)
            .field("recipient_id", &self.recipient_id)
            .field("source", &self.source)
            .field("generation", &self.generation)
            .field("sequence", &self.sequence)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// A persisted message could not be published to the current live observers.
#[derive(Debug, thiserror::Error)]
pub enum McpChannelObservationError {
    /// A sender belonging to another agent cannot attribute this delivery.
    #[error("channel recipient {recipient} does not match event sender {sender}")]
    RecipientMismatch {
        /// Recipient fixed by the inbox.
        recipient: Uuid,
        /// Identity of the event sender.
        sender: Uuid,
    },
    /// No current observer receives this bounded event channel.
    #[error("channel recipient {recipient} has no live event observer")]
    NoObservers {
        /// Recipient whose persisted input remains available for replay.
        recipient: Uuid,
    },
}
