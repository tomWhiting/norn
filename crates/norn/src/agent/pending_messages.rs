//! Durable pending inter-agent messages.
//!
//! The live [`MessageRouter`](crate::agent::message_router::MessageRouter)
//! only reports success when it enqueues onto an inbound channel that a loop
//! can drain. This module covers the other honest success state: a message
//! accepted for a dormant-but-resumable recipient. The in-memory store gives
//! the running harness FIFO drain semantics, while the `agent_message.queued`
//! / `agent_message.dequeued` custom events provide the durable audit trail
//! and rebuild input.
//!
//! Crash semantics are deliberately at-least-once for pending delivery. Queue
//! callers must persist `agent_message.queued` before reporting success, and
//! resume drains must persist the framed `UserMessage` before appending
//! `agent_message.dequeued` and removing the in-memory record. A crash between
//! delivery and dequeue audit can replay the same queued message on resume, but
//! an accepted queued message is not silently lost.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// `event_type` for a message accepted into the pending-message store.
pub const AGENT_MESSAGE_QUEUED_EVENT_TYPE: &str = "agent_message.queued";

/// `event_type` for a pending message claimed for a resumed recipient.
pub const AGENT_MESSAGE_DEQUEUED_EVENT_TYPE: &str = "agent_message.dequeued";

/// Audit lifecycle for a pending inter-agent message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PendingAgentMessageLifecycle {
    /// A message was accepted for later delivery to a resumable agent.
    Queued {
        /// Unique message identifier, shared with the eventual delivered
        /// event when the resumed loop injects the message.
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
        /// Unescaped sender content, verbatim, for audit and rebuild.
        content: String,
        /// Wall-clock queue time.
        queued_at: DateTime<Utc>,
    },
    /// A pending message was drained by a resume/wake path and handed to the
    /// normal inbound injection machinery.
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

/// One pending message plus the queue-time recipient label needed to rebuild
/// the queued audit payload.
#[derive(Clone, Debug)]
pub struct PendingAgentMessage {
    /// Message that will eventually be injected into the recipient loop.
    pub message: ChannelMessage,
    /// Recipient label at queue time.
    pub to: String,
    /// Wall-clock queue time.
    pub queued_at: DateTime<Utc>,
}

impl PendingAgentMessage {
    /// Build a pending record from a routed message and queue-time recipient
    /// label.
    #[must_use]
    pub fn new(message: ChannelMessage, to: String, queued_at: DateTime<Utc>) -> Self {
        Self {
            message,
            to,
            queued_at,
        }
    }

    /// Build the durable queued audit event for this pending message.
    #[must_use]
    pub fn queued_event(&self) -> PendingAgentMessageLifecycle {
        PendingAgentMessageLifecycle::Queued {
            message_id: self.message.id,
            from_id: self.message.sender_id,
            from: self.message.from.clone(),
            role: self.message.role.clone(),
            to_id: self.message.to_id,
            to: self.to.clone(),
            kind: self.message.kind,
            content: self.message.content.clone(),
            queued_at: self.queued_at,
        }
    }

    fn from_queued_event(event: PendingAgentMessageLifecycle) -> Option<Self> {
        let PendingAgentMessageLifecycle::Queued {
            message_id,
            from_id,
            from,
            role,
            to_id,
            to,
            kind,
            content,
            queued_at,
        } = event
        else {
            return None;
        };
        Some(Self {
            message: ChannelMessage {
                id: message_id,
                sender_id: from_id,
                from,
                role,
                to_id,
                content,
                kind,
                seq: None,
                timestamp: queued_at,
            },
            to,
            queued_at,
        })
    }
}

#[derive(Default)]
struct PendingInner {
    by_recipient: HashMap<Uuid, VecDeque<PendingAgentMessage>>,
    ids: HashSet<Uuid>,
}

/// Shared pending-message store for one multi-agent session tree.
#[derive(Default)]
pub struct PendingAgentMessages {
    inner: Mutex<PendingInner>,
}

impl PendingAgentMessages {
    /// Create an empty pending-message store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild pending state from custom audit events.
    #[must_use]
    pub fn from_events(events: &[SessionEvent]) -> Self {
        let store = Self::new();
        for event in events {
            store.apply_event(event);
        }
        store
    }

    /// Queue `pending` if this message id is not already pending.
    ///
    /// Returns the queued audit event when the record was inserted. Duplicate
    /// message ids are ignored and return `None`, making event replay
    /// idempotent.
    pub fn queue(&self, pending: PendingAgentMessage) -> Option<PendingAgentMessageLifecycle> {
        let event = pending.queued_event();
        let mut inner = self.inner.lock();
        if !inner.ids.insert(pending.message.id) {
            return None;
        }
        inner
            .by_recipient
            .entry(pending.message.to_id)
            .or_default()
            .push_back(pending);
        Some(event)
    }

    /// Drain every pending message for `recipient_id` in FIFO order.
    ///
    /// The returned dequeued events should be appended to the store that owns
    /// the resume/wake action before the messages are injected.
    pub fn drain_for(
        &self,
        recipient_id: Uuid,
    ) -> (Vec<ChannelMessage>, Vec<PendingAgentMessageLifecycle>) {
        let mut inner = self.inner.lock();
        let Some(mut queued) = inner.by_recipient.remove(&recipient_id) else {
            return (Vec::new(), Vec::new());
        };
        let mut messages = Vec::with_capacity(queued.len());
        let mut events = Vec::with_capacity(queued.len());
        while let Some(pending) = queued.pop_front() {
            inner.ids.remove(&pending.message.id);
            events.push(PendingAgentMessageLifecycle::Dequeued {
                message_id: pending.message.id,
                to_id: recipient_id,
                dequeued_at: Utc::now(),
            });
            messages.push(pending.message);
        }
        (messages, events)
    }

    /// Return every pending message for `recipient_id` in FIFO order without
    /// removing it.
    ///
    /// Used by resume/wake delivery so persistence can happen before the
    /// in-memory queue is marked consumed.
    #[must_use]
    pub fn messages_for_delivery(&self, recipient_id: Uuid) -> Vec<ChannelMessage> {
        self.inner
            .lock()
            .by_recipient
            .get(&recipient_id)
            .map(|queue| {
                queue
                    .iter()
                    .map(|pending| pending.message.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Build dequeue audit events for `messages`.
    #[must_use]
    pub fn dequeued_events_for(
        recipient_id: Uuid,
        messages: &[ChannelMessage],
    ) -> Vec<PendingAgentMessageLifecycle> {
        messages
            .iter()
            .map(|message| PendingAgentMessageLifecycle::Dequeued {
                message_id: message.id,
                to_id: recipient_id,
                dequeued_at: Utc::now(),
            })
            .collect()
    }

    /// Remove queued messages after their framed delivery and dequeue audit
    /// have both persisted.
    pub fn mark_dequeued(&self, message_ids: impl IntoIterator<Item = Uuid>) {
        for message_id in message_ids {
            self.remove_message(message_id);
        }
    }

    /// Number of pending messages for one recipient.
    #[must_use]
    pub fn pending_for(&self, recipient_id: Uuid) -> usize {
        self.inner
            .lock()
            .by_recipient
            .get(&recipient_id)
            .map_or(0, VecDeque::len)
    }

    /// Total pending messages across all recipients.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().ids.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn apply_event(&self, event: &SessionEvent) {
        let SessionEvent::Custom {
            event_type, data, ..
        } = event
        else {
            return;
        };
        match event_type.as_str() {
            AGENT_MESSAGE_QUEUED_EVENT_TYPE => {
                match serde_json::from_value::<PendingAgentMessageLifecycle>(data.clone()) {
                    Ok(lifecycle) => {
                        if let Some(pending) = PendingAgentMessage::from_queued_event(lifecycle) {
                            let _ = self.queue(pending);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "pending agent messages: invalid queued audit payload",
                        );
                    }
                }
            }
            AGENT_MESSAGE_DEQUEUED_EVENT_TYPE
            | crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE => {
                if let Some(message_id) = data
                    .get("message_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
                {
                    self.remove_message(message_id);
                }
            }
            _ => {}
        }
    }

    /// Remove one pending message by id.
    pub fn remove_message(&self, message_id: Uuid) {
        let mut inner = self.inner.lock();
        if !inner.ids.remove(&message_id) {
            return;
        }
        for queue in inner.by_recipient.values_mut() {
            if let Some(idx) = queue
                .iter()
                .position(|pending| pending.message.id == message_id)
            {
                queue.remove(idx);
                break;
            }
        }
        inner.by_recipient.retain(|_, queue| !queue.is_empty());
    }
}

/// Append a pending-message audit event to `store`.
///
/// # Errors
///
/// Returns [`SessionError::EventAppendFailed`](crate::error::SessionError::EventAppendFailed) if the payload cannot be
/// serialized, or any [`SessionError`](crate::error::SessionError) propagated by [`EventStore::append`].
pub fn append_pending_message_audit(
    store: &EventStore,
    event: &PendingAgentMessageLifecycle,
) -> Result<crate::session::events::EventId, crate::error::SessionError> {
    let event_type = event.session_event_type();
    let data = serde_json::to_value(event).map_err(|error| {
        crate::error::SessionError::EventAppendFailed {
            reason: format!(
                "failed to serialize {event_type} audit for message {}: {error}",
                event.message_id()
            ),
        }
    })?;
    // Audit appends ride the loop's hot path (pending drains, requeue
    // sweeps), so the sink I/O is kept off the executor exactly like
    // every other loop append.
    crate::r#loop::append_off_executor(
        store,
        SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: event_type.to_owned(),
            data,
        },
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::r#loop::inbound::MessageKind;

    fn message(to_id: Uuid, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "/root/sender".to_owned(),
            role: Some("worker".to_owned()),
            to_id,
            content: content.to_owned(),
            kind: MessageKind::Update,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn queue_and_drain_are_fifo_per_recipient() {
        let store = PendingAgentMessages::new();
        let recipient = Uuid::new_v4();
        let first = message(recipient, "first");
        let second = message(recipient, "second");

        assert!(
            store
                .queue(PendingAgentMessage::new(
                    first.clone(),
                    "/root/child".to_owned(),
                    first.timestamp,
                ))
                .is_some()
        );
        assert!(
            store
                .queue(PendingAgentMessage::new(
                    second.clone(),
                    "/root/child".to_owned(),
                    second.timestamp,
                ))
                .is_some()
        );
        assert_eq!(store.pending_for(recipient), 2);

        let (messages, events) = store.drain_for(recipient);
        assert_eq!(
            messages
                .iter()
                .map(|msg| msg.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"],
        );
        assert_eq!(events.len(), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn duplicate_message_id_is_ignored() {
        let store = PendingAgentMessages::new();
        let recipient = Uuid::new_v4();
        let msg = message(recipient, "once");
        let pending =
            PendingAgentMessage::new(msg.clone(), "/root/child".to_owned(), msg.timestamp);

        assert!(store.queue(pending.clone()).is_some());
        assert!(store.queue(pending).is_none());
        assert_eq!(store.pending_for(recipient), 1);
    }

    #[test]
    fn delivery_read_does_not_consume_until_marked_dequeued() {
        let store = PendingAgentMessages::new();
        let recipient = Uuid::new_v4();
        let msg = message(recipient, "pending");
        let pending =
            PendingAgentMessage::new(msg.clone(), "/root/child".to_owned(), msg.timestamp);

        assert!(store.queue(pending).is_some());

        let messages = store.messages_for_delivery(recipient);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, msg.id);
        assert_eq!(store.pending_for(recipient), 1);

        store.mark_dequeued(messages.iter().map(|message| message.id));

        assert_eq!(store.pending_for(recipient), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn rebuild_from_events_removes_dequeued_and_delivered_messages() {
        let recipient = Uuid::new_v4();
        let kept = message(recipient, "kept");
        let drained = message(recipient, "drained");
        let delivered = message(recipient, "delivered");
        let event_store = EventStore::new();

        for msg in [&kept, &drained, &delivered] {
            let event =
                PendingAgentMessage::new(msg.clone(), "/root/child".to_owned(), msg.timestamp)
                    .queued_event();
            append_pending_message_audit(&event_store, &event).unwrap();
        }
        append_pending_message_audit(
            &event_store,
            &PendingAgentMessageLifecycle::Dequeued {
                message_id: drained.id,
                to_id: recipient,
                dequeued_at: Utc::now(),
            },
        )
        .unwrap();
        event_store
            .append(SessionEvent::Custom {
                base: EventBase::new(event_store.last_event_id()),
                event_type: crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE
                    .to_owned(),
                data: serde_json::json!({
                    "phase": "delivered",
                    "message_id": delivered.id,
                    "from_id": delivered.sender_id,
                    "to_id": recipient,
                    "seq": 1_u64,
                    "delivered_at": Utc::now(),
                }),
            })
            .unwrap();

        let rebuilt = PendingAgentMessages::from_events(&event_store.events());
        assert_eq!(rebuilt.pending_for(recipient), 1);
        let (messages, _) = rebuilt.drain_for(recipient);
        assert_eq!(messages[0].id, kept.id);
    }
}
