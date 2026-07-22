//! Canonical pending-message queue publication and mailbox registration.

use std::sync::Arc;

use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::frame_message;
use crate::session::MailboxId;
use crate::session::store::EventStore;

use super::pending_delivery::{pending_delivery_event_id, pending_queue_event_id};
use super::pending_mailbox::PendingMailboxLease;
use super::pending_messages::{
    AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessage, PendingAgentMessageLifecycle,
    PendingAgentMessages, exact_frame, exact_pending, find_pending, find_pending_mut,
    publish_pending, remove_message,
};

pub(crate) struct QueuedPendingMessage {
    pub(crate) mailbox_store: Arc<EventStore>,
    pub(crate) observation: PendingAgentMessageLifecycle,
    pub(crate) published: bool,
}

pub(crate) struct ClosedPendingMailbox {
    pub(super) recipient_id: Uuid,
    pub(super) store: Arc<EventStore>,
    pub(super) mailbox_id: MailboxId,
}

impl ClosedPendingMailbox {
    #[must_use]
    pub(crate) fn recipient_id(&self) -> Uuid {
        self.recipient_id
    }
}

impl PendingAgentMessages {
    pub(super) fn exact_record_is_retained(&self, proposed: &PendingAgentMessage) -> bool {
        find_pending(&self.inner.lock(), proposed.message.id)
            .is_some_and(|existing| exact_pending(existing, proposed).is_ok())
    }

    /// Register the root mailbox before the root registry entry becomes live.
    pub(crate) fn register_root_mailbox(
        &self,
        agent_id: Uuid,
        mailbox_id: MailboxId,
        store: &Arc<EventStore>,
        controller: &Arc<PendingMailboxLease>,
    ) -> Result<(), SessionError> {
        self.register_mailbox(agent_id, mailbox_id, store, controller)
    }

    /// Register a child mailbox before its registry entry becomes live.
    pub(crate) fn register_child_mailbox(
        &self,
        agent_id: Uuid,
        mailbox_id: MailboxId,
        store: &Arc<EventStore>,
        controller: &Arc<PendingMailboxLease>,
    ) -> Result<(), SessionError> {
        self.register_mailbox(agent_id, mailbox_id, store, controller)
    }

    fn register_mailbox(
        &self,
        agent_id: Uuid,
        mailbox_id: MailboxId,
        store: &Arc<EventStore>,
        controller: &Arc<PendingMailboxLease>,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(agent_id);
        let _enqueue_guard = enqueue_lock.lock();
        self.mailboxes
            .register(agent_id, mailbox_id, store, controller)
    }

    /// Close child mailbox acceptance before its live route is removed.
    pub(crate) fn close_child_mailbox(
        &self,
        agent_id: Uuid,
        controller: &Arc<PendingMailboxLease>,
    ) -> Option<ClosedPendingMailbox> {
        let enqueue_lock = self.enqueue_locks.for_recipient(agent_id);
        let _enqueue_guard = enqueue_lock.lock();
        self.mailboxes
            .close(agent_id, controller)
            .map(|target| ClosedPendingMailbox {
                recipient_id: agent_id,
                store: target.store,
                mailbox_id: target.mailbox_id,
            })
    }

    /// Stage a route-accepted message after terminal mailbox closure.
    pub(crate) fn stage_after_close(
        &self,
        closed: &ClosedPendingMailbox,
        pending: &mut PendingAgentMessage,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(closed.recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        pending.bind_mailbox(closed.recipient_id, closed.mailbox_id)?;
        self.stage_terminal_locked(pending)
    }

    pub(super) fn stage_terminal_locked(
        &self,
        pending: &mut PendingAgentMessage,
    ) -> Result<(), SessionError> {
        let message_id = pending.message.id;
        let mut inner = self.inner.lock();
        if let Some(existing) = find_pending(&inner, message_id) {
            exact_pending(existing, pending)?;
            if let Some(existing) = find_pending_mut(&mut inner, message_id) {
                existing.terminal_recovery = true;
            }
            return Ok(());
        }
        pending.terminal_recovery = true;
        publish_pending(&mut inner, pending.clone(), false);
        Ok(())
    }

    pub(crate) fn persist_for_registered_recipient(
        &self,
        pending: &mut PendingAgentMessage,
    ) -> Result<QueuedPendingMessage, SessionError> {
        self.persist_for_registered_target(None, pending)
    }

    pub(crate) fn persist_for_registered_store(
        &self,
        expected_store: &EventStore,
        pending: &mut PendingAgentMessage,
    ) -> Result<QueuedPendingMessage, SessionError> {
        self.persist_for_registered_target(Some(expected_store), pending)
    }

    fn persist_for_registered_target(
        &self,
        expected_store: Option<&EventStore>,
        pending: &mut PendingAgentMessage,
    ) -> Result<QueuedPendingMessage, SessionError> {
        let recipient_id = pending.mailbox_owner;
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let target =
            self.mailboxes
                .target(recipient_id)
                .ok_or_else(|| SessionError::StorageError {
                    reason: format!(
                        "agent {recipient_id} has no live registered pending-message mailbox"
                    ),
                })?;
        validate_expected_store(recipient_id, expected_store, target.store.as_ref())?;
        pending.bind_mailbox(recipient_id, target.mailbox_id)?;
        let published = self.persist_and_publish_locked(target.store.as_ref(), pending)?;
        Ok(QueuedPendingMessage {
            mailbox_store: target.store,
            observation: pending.queued_observation(),
            published,
        })
    }

    pub(crate) fn validate_registered_store(
        &self,
        recipient_id: Uuid,
        expected_store: &EventStore,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let target =
            self.mailboxes
                .target(recipient_id)
                .ok_or_else(|| SessionError::StorageError {
                    reason: format!(
                        "agent {recipient_id} has no live registered pending-message mailbox"
                    ),
                })?;
        validate_expected_store(recipient_id, Some(expected_store), target.store.as_ref())
    }

    pub(super) fn persist_and_publish_locked(
        &self,
        store: &EventStore,
        pending: &mut PendingAgentMessage,
    ) -> Result<bool, SessionError> {
        let message_id = pending.message.id;
        let framed = frame_message(&pending.message);
        {
            let mut inner = self.inner.lock();
            if let Some(existing) = find_pending(&inner, message_id) {
                exact_pending(existing, pending)?;
                let durable = existing.queue_durable;
                drop(inner);
                if !durable {
                    self.ensure_recipient_prefix_durable_locked(pending.mailbox_owner, store)?;
                }
                return Ok(false);
            }
            if !inner.preparing_ids.insert(message_id) {
                return Err(SessionError::EventAppendFailed {
                    reason: format!("pending message {message_id} is already being durably queued"),
                });
            }
        }
        let attempt: Result<bool, SessionError> = (|| {
            self.ensure_recipient_prefix_durable_locked(pending.mailbox_owner, store)?;
            let event = pending.prepare_queue_event(store)?;
            if delivery_is_durable(store, pending, &framed)? {
                return Ok(false);
            }
            crate::r#loop::append_idempotent_off_executor(store, event)?;
            Ok(true)
        })();

        let mut inner = self.inner.lock();
        inner.preparing_ids.remove(&message_id);
        match attempt {
            Ok(true) => {
                publish_pending(&mut inner, pending.clone(), true);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                // The exact cached Q is the only safe way to reconcile an
                // ambiguous persistence failure. Retain it locally without
                // claiming durability; later FIFO work retries this identity.
                publish_pending(&mut inner, pending.clone(), false);
                Err(SessionError::EventAppendFailed {
                    reason: format!(
                        "pending message {message_id} has indeterminate queue durability; do not resend: {error}"
                    ),
                })
            }
        }
    }

    pub(crate) fn ensure_head_durable(
        &self,
        recipient_id: Uuid,
        store: &EventStore,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let target =
            self.mailboxes
                .target(recipient_id)
                .ok_or_else(|| SessionError::StorageError {
                    reason: format!(
                        "agent {recipient_id} has no live registered pending-message mailbox"
                    ),
                })?;
        validate_expected_store(recipient_id, Some(store), target.store.as_ref())?;
        self.ensure_recipient_prefix_durable_locked(recipient_id, store)
    }

    pub(super) fn ensure_recipient_prefix_durable_locked(
        &self,
        recipient_id: Uuid,
        store: &EventStore,
    ) -> Result<(), SessionError> {
        loop {
            let (event, message_id) = {
                let mut inner = self.inner.lock();
                let Some(pending) = inner
                    .by_recipient
                    .get_mut(&recipient_id)
                    .and_then(|queue| queue.iter_mut().find(|pending| !pending.queue_durable))
                else {
                    return Ok(());
                };
                (pending.prepare_queue_event(store)?, pending.message.id)
            };
            crate::r#loop::append_idempotent_off_executor(store, event)?;
            let mut inner = self.inner.lock();
            let Some(pending_record) =
                inner.by_recipient.get_mut(&recipient_id).and_then(|queue| {
                    queue
                        .iter_mut()
                        .find(|queued| queued.message.id == message_id)
                })
            else {
                return Err(SessionError::EventAppendFailed {
                    reason: format!(
                        "pending message {message_id} disappeared after queue durability"
                    ),
                });
            };
            pending_record.queue_durable = true;
        }
    }
}

pub(super) fn retire_durable_terminal_records(
    inner: &mut super::pending_messages::PendingInner,
    recipient_id: Uuid,
    mailbox_id: Option<crate::session::MailboxId>,
) {
    let retired: Vec<Uuid> = inner
        .by_recipient
        .get(&recipient_id)
        .into_iter()
        .flatten()
        .filter(|pending| {
            pending.terminal_recovery && pending.queue_durable && pending.mailbox_id == mailbox_id
        })
        .map(|pending| pending.message.id)
        .collect();
    for message_id in retired {
        remove_message(inner, message_id);
    }
}

fn delivery_is_durable(
    store: &EventStore,
    pending: &PendingAgentMessage,
    framed_content: &str,
) -> Result<bool, SessionError> {
    let message_id = pending.message.id;
    let Some(event) = store.event_by_id(&pending_delivery_event_id(message_id)) else {
        return Ok(false);
    };
    let queue = store
        .event_by_id(&pending_queue_event_id(message_id))
        .ok_or_else(|| SessionError::EventAppendFailed {
            reason: format!(
                "stable pending delivery ID for message {message_id} has no canonical queue authority"
            ),
        })?;
    let crate::session::events::SessionEvent::Custom {
        event_type, data, ..
    } = queue
    else {
        return Err(SessionError::EventAppendFailed {
            reason: format!(
                "stable pending queue ID for message {message_id} belongs to the wrong event shape"
            ),
        });
    };
    if event_type != AGENT_MESSAGE_QUEUED_EVENT_TYPE
        || data
            != serde_json::to_value(pending.queued_event()).map_err(|error| {
                SessionError::EventAppendFailed {
                    reason: format!(
                        "failed to validate queued authority for message {message_id}: {error}"
                    ),
                }
            })?
    {
        return Err(SessionError::EventAppendFailed {
            reason: format!(
                "stable pending queue ID for message {message_id} conflicts with the proposed message"
            ),
        });
    }
    let crate::session::events::SessionEvent::UserMessage { content, .. } = event else {
        return Err(SessionError::EventAppendFailed {
            reason: format!(
                "stable pending delivery ID for message {message_id} belongs to the wrong event shape"
            ),
        });
    };
    exact_frame(&content, framed_content, message_id)?;
    Ok(true)
}

pub(super) fn validate_expected_store(
    recipient_id: Uuid,
    expected: Option<&EventStore>,
    registered: &EventStore,
) -> Result<(), SessionError> {
    if expected.is_none_or(|expected| std::ptr::eq(expected, registered)) {
        return Ok(());
    }
    Err(SessionError::StorageError {
        reason: format!(
            "agent {recipient_id} is registered to a different pending-message mailbox store"
        ),
    })
}
