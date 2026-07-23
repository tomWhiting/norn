//! Durable pending-message record and audit payload.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::session::MailboxId;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::pending_delivery::{PendingDeliveryAttempt, pending_queue_event_id};

/// `event_type` for a message accepted into the pending-message store.
pub const AGENT_MESSAGE_QUEUED_EVENT_TYPE: &str = "agent_message.queued";

/// `event_type` for a pending message claimed for a resumed recipient.
pub const AGENT_MESSAGE_DEQUEUED_EVENT_TYPE: &str = "agent_message.dequeued";

#[derive(Clone)]
pub(super) struct PendingPersistenceAuthority {
    pub(super) mailbox_id: MailboxId,
    pub(super) store: Arc<EventStore>,
}

impl fmt::Debug for PendingPersistenceAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingPersistenceAuthority")
            .field("mailbox_id", &"[REDACTED]")
            .field("store", &"[REDACTED]")
            .finish()
    }
}

/// Audit lifecycle for a pending inter-agent message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PendingAgentMessageLifecycle {
    /// A message was accepted for later delivery to a resumable agent.
    Queued {
        /// Unique message identifier.
        message_id: Uuid,
        /// Sender agent id.
        from_id: Uuid,
        /// Sender label at queue time.
        from: String,
        /// Sender role, when known.
        role: Option<String>,
        /// Recipient agent id.
        to_id: Uuid,
        /// Recipient label at queue time.
        to: String,
        /// Delivery semantics requested by the sender.
        kind: MessageKind,
        /// Router-minted sequence, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        /// Unescaped sender content, verbatim.
        content: String,
        /// Wall-clock queue time.
        queued_at: DateTime<Utc>,
        /// Original framed-message timestamp. New authority preserves it
        /// separately from queue time so replay reconstructs the exact U bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_timestamp: Option<DateTime<Utc>>,
        /// Whether this row belongs to the recipient timeline. Sender and
        /// parent copies are audit-only and never participate in replay.
        ///
        /// `None` is the pre-authority durable shape. The old writer copied
        /// identical rows into sender and parent timelines, and `to_id` was a
        /// volatile runtime identifier rather than durable mailbox authority.
        /// Replay therefore validates completed legacy rows but fails closed
        /// on every unresolved row instead of guessing ownership.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authoritative: Option<bool>,
        /// Stable identity of the recipient timeline. Present on every new
        /// authoritative row and its observation copies; absent only on the
        /// legacy pre-D8 shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mailbox_id: Option<MailboxId>,
    },
    /// Secondary audit emitted after authoritative delivery.
    Dequeued {
        /// Unique message identifier.
        message_id: Uuid,
        /// Recipient agent id.
        to_id: Uuid,
        /// Wall-clock drain time.
        dequeued_at: DateTime<Utc>,
    },
}

impl PendingAgentMessageLifecycle {
    /// Unique id of the message this event concerns.
    #[must_use]
    pub const fn message_id(&self) -> Uuid {
        match self {
            Self::Queued { message_id, .. } | Self::Dequeued { message_id, .. } => *message_id,
        }
    }

    /// Session-store event type for this phase.
    #[must_use]
    pub const fn session_event_type(&self) -> &'static str {
        match self {
            Self::Queued { .. } => AGENT_MESSAGE_QUEUED_EVENT_TYPE,
            Self::Dequeued { .. } => AGENT_MESSAGE_DEQUEUED_EVENT_TYPE,
        }
    }
}

/// One pending message plus its queue-time recipient label.
#[derive(Clone, Debug)]
pub struct PendingAgentMessage {
    /// Message eventually injected into the recipient loop.
    pub message: ChannelMessage,
    /// Recipient label at queue time.
    pub to: String,
    /// Wall-clock queue time.
    pub queued_at: DateTime<Utc>,
    /// Runtime recipient that owns this timeline mailbox. This may differ
    /// from the historical `message.to_id` after a direct session resume.
    pub(super) mailbox_owner: Uuid,
    pub(super) mailbox_id: Option<MailboxId>,
    pub(super) exact_message_timestamp: Option<DateTime<Utc>>,
    pub(super) queue_durable: bool,
    pub(super) terminal_recovery: bool,
    pub(super) persistence_authority: Option<PendingPersistenceAuthority>,
    pub(super) queue_event: Option<SessionEvent>,
    pub(super) delivery_attempt: Option<PendingDeliveryAttempt>,
}

impl PendingAgentMessage {
    /// Build a pending record from a routed message.
    #[must_use]
    pub fn new(message: ChannelMessage, to: String, queued_at: DateTime<Utc>) -> Self {
        let mailbox_owner = message.to_id;
        let exact_message_timestamp = Some(message.timestamp);
        Self {
            message,
            to,
            queued_at,
            mailbox_owner,
            mailbox_id: None,
            exact_message_timestamp,
            queue_durable: false,
            terminal_recovery: false,
            persistence_authority: None,
            queue_event: None,
            delivery_attempt: None,
        }
    }

    pub(super) fn queued_event(&self) -> PendingAgentMessageLifecycle {
        self.queued_event_with_authority(Some(true))
    }

    pub(crate) fn queued_observation(&self) -> PendingAgentMessageLifecycle {
        self.queued_event_with_authority(Some(false))
    }

    fn queued_event_with_authority(
        &self,
        authoritative: Option<bool>,
    ) -> PendingAgentMessageLifecycle {
        PendingAgentMessageLifecycle::Queued {
            message_id: self.message.id,
            from_id: self.message.sender_id,
            from: self.message.from.clone(),
            role: self.message.role.clone(),
            to_id: self.message.to_id,
            to: self.to.clone(),
            kind: self.message.kind,
            seq: self.message.seq,
            content: self.message.content.clone(),
            queued_at: self.queued_at,
            message_timestamp: self.exact_message_timestamp,
            authoritative,
            mailbox_id: self.mailbox_id,
        }
    }

    pub(super) fn from_queued_event(
        event: PendingAgentMessageLifecycle,
    ) -> Option<(Self, Option<bool>)> {
        let PendingAgentMessageLifecycle::Queued {
            message_id,
            from_id,
            from,
            role,
            to_id,
            to,
            kind,
            seq,
            content,
            queued_at,
            message_timestamp,
            authoritative,
            mailbox_id,
        } = event
        else {
            return None;
        };
        Some((
            Self {
                message: ChannelMessage {
                    id: message_id,
                    sender_id: from_id,
                    from,
                    role,
                    to_id,
                    content,
                    kind,
                    seq,
                    timestamp: message_timestamp.unwrap_or(queued_at),
                },
                to,
                queued_at,
                mailbox_owner: to_id,
                mailbox_id,
                exact_message_timestamp: message_timestamp,
                queue_durable: true,
                terminal_recovery: false,
                persistence_authority: None,
                queue_event: None,
                delivery_attempt: None,
            },
            authoritative,
        ))
    }

    pub(super) fn rebind_mailbox_owner(&mut self, mailbox_owner: Uuid) {
        self.mailbox_owner = mailbox_owner;
    }

    pub(super) fn bind_mailbox(
        &mut self,
        mailbox_owner: Uuid,
        mailbox_id: MailboxId,
    ) -> Result<(), SessionError> {
        if self
            .mailbox_id
            .is_some_and(|existing| existing != mailbox_id)
        {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "pending message {} is already bound to a different mailbox",
                    self.message.id
                ),
            });
        }
        self.mailbox_owner = mailbox_owner;
        self.mailbox_id = Some(mailbox_id);
        Ok(())
    }

    pub(super) fn prepare_queue_event(
        &mut self,
        store: &EventStore,
    ) -> Result<SessionEvent, SessionError> {
        if let Some(event) = self.queue_event.as_ref() {
            return Ok(event.clone());
        }
        if self.mailbox_id.is_none() {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "pending message {} has no stable recipient mailbox identity",
                    self.message.id
                ),
            });
        }
        let data = serde_json::to_value(self.queued_event()).map_err(|error| {
            SessionError::EventAppendFailed {
                reason: format!(
                    "failed to serialize canonical queued message {}: {error}",
                    self.message.id
                ),
            }
        })?;
        let event = SessionEvent::Custom {
            base: EventBase {
                id: pending_queue_event_id(self.message.id),
                parent_id: store.last_event_id(),
                timestamp: self.queued_at,
            },
            event_type: AGENT_MESSAGE_QUEUED_EVENT_TYPE.to_owned(),
            data,
        };
        self.queue_event = Some(event.clone());
        Ok(event)
    }
}

/// Append a secondary pending-message audit event.
///
/// # Errors
///
/// Returns a typed append error when serialization or persistence fails.
pub fn append_pending_message_audit(
    store: &EventStore,
    event: &PendingAgentMessageLifecycle,
) -> Result<EventId, SessionError> {
    let event_type = event.session_event_type();
    let data = serde_json::to_value(event).map_err(|error| SessionError::EventAppendFailed {
        reason: format!(
            "failed to serialize {event_type} audit for message {}: {error}",
            event.message_id()
        ),
    })?;
    crate::r#loop::append_off_executor(
        store,
        SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: event_type.to_owned(),
            data,
        },
    )
}
