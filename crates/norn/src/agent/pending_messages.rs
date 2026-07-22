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
//! The framed `UserMessage` is the authoritative consumption record. Its event
//! ID is derived from the queued message UUID in a reserved namespace, and an
//! in-process ambiguous write retries the exact cached event. On restart,
//! replay consumes a queued record only when it finds that namespaced
//! `UserMessage` with the exact expected frame. Dequeue/delivery audit events
//! are secondary observability and never decide whether content is replayed.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, frame_message};
use crate::session::events::EventId;

use super::pending_delivery::{PendingDeliveryAttempt, PreparedPendingAgentMessage};
use super::pending_mailbox::{MailboxRegistry, RecipientEnqueueLocks};
pub use super::pending_record::{
    AGENT_MESSAGE_DEQUEUED_EVENT_TYPE, AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessage,
    PendingAgentMessageLifecycle, append_pending_message_audit,
};
use super::pending_teardown::TerminalPendingRecovery;

#[derive(Default)]
pub(super) struct PendingInner {
    pub(super) by_recipient: HashMap<Uuid, VecDeque<PendingAgentMessage>>,
    pub(super) ids: HashSet<Uuid>,
    pub(super) preparing_ids: HashSet<Uuid>,
    pub(super) active_delivery_recipients: HashSet<Uuid>,
    pub(super) terminal_recoveries: HashMap<Uuid, TerminalPendingRecovery>,
}

/// Shared pending-message store for one multi-agent session tree.
pub struct PendingAgentMessages {
    pub(super) inner: Mutex<PendingInner>,
    pub(super) mailboxes: MailboxRegistry,
    pub(super) enqueue_locks: RecipientEnqueueLocks,
}

impl Default for PendingAgentMessages {
    fn default() -> Self {
        Self {
            inner: Mutex::new(PendingInner::default()),
            mailboxes: MailboxRegistry::default(),
            enqueue_locks: RecipientEnqueueLocks::default(),
        }
    }
}

/// Recipient-scoped ownership of one pending delivery flush.
///
/// This guard contains no mutex guard and remains `Send` across async hooks.
/// Dropping it releases only its recipient's claim under a short lock.
pub(crate) struct PendingDeliveryFlush<'a> {
    pending: &'a PendingAgentMessages,
    recipient_id: Uuid,
}

impl Drop for PendingDeliveryFlush<'_> {
    fn drop(&mut self) {
        self.pending
            .inner
            .lock()
            .active_delivery_recipients
            .remove(&self.recipient_id);
    }
}

impl PendingAgentMessages {
    /// Create an empty pending-message store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Prepare the FIFO head for one authoritative `UserMessage` append.
    ///
    /// The prepared event is retained in the pending record before it leaves
    /// the mutex. Concurrent flushes and same-process retries therefore use
    /// byte-identical event identity, parent, timestamp, and content.
    pub(crate) fn prepare_next_delivery(
        &self,
        recipient_id: Uuid,
        parent_id: Option<EventId>,
    ) -> Option<PreparedPendingAgentMessage> {
        let mut inner = self.inner.lock();
        let pending = inner.by_recipient.get_mut(&recipient_id)?.front_mut()?;
        if pending.delivery_attempt.is_none() {
            pending.delivery_attempt =
                Some(PendingDeliveryAttempt::new(&pending.message, parent_id));
        }
        pending
            .delivery_attempt
            .as_ref()
            .map(|attempt| attempt.prepare(&pending.message))
    }

    /// Acquire exclusive ownership of one recipient's pending flush.
    ///
    /// The async caller holds this guard across secondary hooks. A competing
    /// step fails fast instead of cloning the same prepared head into a second
    /// provider request.
    pub(crate) fn try_delivery_flush(
        &self,
        recipient_id: Uuid,
    ) -> Option<PendingDeliveryFlush<'_>> {
        let mut inner = self.inner.lock();
        if !inner.active_delivery_recipients.insert(recipient_id) {
            return None;
        }
        Some(PendingDeliveryFlush {
            pending: self,
            recipient_id,
        })
    }

    /// Commit the FIFO head after its exact framed `UserMessage` has become
    /// durable. No public drain/removal API exists: content leaves the mailbox
    /// only through this authority-bound transition.
    pub(crate) fn commit_delivery(
        &self,
        recipient_id: Uuid,
        message_id: Uuid,
        framed_content: &str,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock();
        let Some(head) = inner
            .by_recipient
            .get(&recipient_id)
            .and_then(|queue| queue.front())
        else {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "pending message {message_id} became durable without a queued FIFO head"
                ),
            });
        };
        if head.message.id != message_id {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "pending message {message_id} became durable out of recipient FIFO order"
                ),
            });
        }
        let expected = frame_message(&head.message);
        exact_frame(&expected, framed_content, message_id)?;
        remove_message(&mut inner, message_id);
        Ok(())
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
}

pub(super) fn find_pending(inner: &PendingInner, message_id: Uuid) -> Option<&PendingAgentMessage> {
    inner
        .by_recipient
        .values()
        .flat_map(|queue| queue.iter())
        .find(|pending| pending.message.id == message_id)
}

pub(super) fn find_pending_mut(
    inner: &mut PendingInner,
    message_id: Uuid,
) -> Option<&mut PendingAgentMessage> {
    inner
        .by_recipient
        .values_mut()
        .flat_map(|queue| queue.iter_mut())
        .find(|pending| pending.message.id == message_id)
}

pub(super) fn publish_pending(
    inner: &mut PendingInner,
    mut pending: PendingAgentMessage,
    queue_durable: bool,
) {
    let message_id = pending.message.id;
    let recipient_id = pending.mailbox_owner;
    pending.queue_durable = queue_durable;
    inner.ids.insert(message_id);
    inner
        .by_recipient
        .entry(recipient_id)
        .or_default()
        .push_back(pending);
}

pub(super) fn exact_pending(
    existing: &PendingAgentMessage,
    proposed: &PendingAgentMessage,
) -> Result<(), SessionError> {
    let message_id = proposed.message.id;
    let existing = serde_json::to_vec(&existing.queued_event()).map_err(|error| {
        SessionError::EventAppendFailed {
            reason: format!("failed to compare an existing queued message: {error}"),
        }
    })?;
    let proposed = serde_json::to_vec(&proposed.queued_event()).map_err(|error| {
        SessionError::EventAppendFailed {
            reason: format!("failed to compare a proposed queued message: {error}"),
        }
    })?;
    if existing == proposed {
        return Ok(());
    }
    Err(SessionError::EventAppendFailed {
        reason: format!(
            "pending message ID {message_id} is already associated with different content"
        ),
    })
}

pub(super) fn exact_frame(
    existing: &str,
    proposed: &str,
    message_id: Uuid,
) -> Result<(), SessionError> {
    if existing == proposed {
        return Ok(());
    }
    Err(SessionError::EventAppendFailed {
        reason: format!(
            "pending message ID {message_id} is associated with conflicting framed content"
        ),
    })
}

pub(super) fn remove_message(inner: &mut PendingInner, message_id: Uuid) {
    if !inner.ids.remove(&message_id) {
        return;
    }
    for queue in inner.by_recipient.values_mut() {
        if let Some(index) = queue
            .iter()
            .position(|pending| pending.message.id == message_id)
        {
            queue.remove(index);
            break;
        }
    }
    inner.by_recipient.retain(|_, queue| !queue.is_empty());
}
