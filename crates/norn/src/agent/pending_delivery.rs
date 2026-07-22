//! Stable authoritative event identity for pending-message delivery.

use chrono::Utc;
use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, frame_message};
use crate::session::events::{EventBase, EventId, SessionEvent};

const DELIVERY_EVENT_ID_PREFIX: &str = "norn:pending-agent-message:delivered:";
const QUEUE_EVENT_ID_PREFIX: &str = "norn:pending-agent-message:queued:";

/// Exact event retained across a same-process ambiguous append result.
#[derive(Clone, Debug)]
pub(super) struct PendingDeliveryAttempt {
    event: SessionEvent,
    framed_content: String,
}

impl PendingDeliveryAttempt {
    pub(super) fn new(message: &ChannelMessage, parent_id: Option<EventId>) -> Self {
        let framed_content = frame_message(message);
        let event = SessionEvent::UserMessage {
            base: EventBase {
                id: pending_delivery_event_id(message.id),
                parent_id,
                timestamp: Utc::now(),
            },
            content: framed_content.clone(),
        };
        Self {
            event,
            framed_content,
        }
    }

    pub(super) fn prepare(&self, message: &ChannelMessage) -> PreparedPendingAgentMessage {
        PreparedPendingAgentMessage {
            message: message.clone(),
            delivery_event: self.event.clone(),
            framed_content: self.framed_content.clone(),
        }
    }
}

/// One pending record prepared for its authoritative append.
#[derive(Clone, Debug)]
pub(crate) struct PreparedPendingAgentMessage {
    pub(crate) message: ChannelMessage,
    pub(crate) delivery_event: SessionEvent,
    pub(crate) framed_content: String,
}

pub(super) fn pending_delivery_event_id(message_id: Uuid) -> EventId {
    EventId::from_stable_namespace(format!("{DELIVERY_EVENT_ID_PREFIX}{message_id}"))
}

pub(super) fn pending_delivery_message_id(
    event_id: &EventId,
) -> Result<Option<Uuid>, SessionError> {
    parse_reserved_id(event_id, DELIVERY_EVENT_ID_PREFIX, "delivery")
}

pub(super) fn pending_queue_event_id(message_id: Uuid) -> EventId {
    EventId::from_stable_namespace(format!("{QUEUE_EVENT_ID_PREFIX}{message_id}"))
}

pub(super) fn pending_queue_message_id(event_id: &EventId) -> Result<Option<Uuid>, SessionError> {
    parse_reserved_id(event_id, QUEUE_EVENT_ID_PREFIX, "queue")
}

fn parse_reserved_id(
    event_id: &EventId,
    prefix: &str,
    purpose: &str,
) -> Result<Option<Uuid>, SessionError> {
    let Some(value) = event_id.as_str().strip_prefix(prefix) else {
        return Ok(None);
    };
    let parsed =
        Uuid::parse_str(value).map_err(|_error| SessionError::PendingMessageReplayInvalid {
            reason: format!("reserved pending-message {purpose} event ID is malformed"),
        })?;
    if parsed.hyphenated().to_string() != value {
        return Err(SessionError::PendingMessageReplayInvalid {
            reason: format!(
                "reserved pending-message {purpose} event ID is not canonical lowercase hyphenated UUID form"
            ),
        });
    }
    Ok(Some(parsed))
}
