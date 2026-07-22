//! Recipient-scoped queue ordering and live mailbox registration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::error::SessionError;
use crate::session::MailboxId;
use crate::session::store::EventStore;

/// Task-owned proof that a child controller can still consume its mailbox.
///
/// Registration retains only a weak reference. A controller that exits or is
/// aborted drops the final strong lease even when an `AgentHandle` still owns
/// the historical event store, so a stale registry entry cannot accept mail.
#[derive(Debug)]
pub(crate) struct PendingMailboxLease {
    open: AtomicBool,
}

impl PendingMailboxLease {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            open: AtomicBool::new(true),
        }
    }

    fn close(&self) {
        self.open.store(false, Ordering::Release);
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
}

struct MailboxRegistration {
    store: Weak<EventStore>,
    mailbox_id: MailboxId,
    controller: Weak<PendingMailboxLease>,
}

pub(super) struct MailboxTarget {
    pub(super) store: Arc<EventStore>,
    pub(super) mailbox_id: MailboxId,
}

#[derive(Default)]
pub(super) struct MailboxRegistry {
    inner: Mutex<HashMap<Uuid, MailboxRegistration>>,
}

impl MailboxRegistry {
    pub(super) fn register(
        &self,
        agent_id: Uuid,
        mailbox_id: MailboxId,
        store: &Arc<EventStore>,
        controller: &Arc<PendingMailboxLease>,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.get(&agent_id)
            && registration_is_live(existing)
            && (existing.mailbox_id != mailbox_id
                || existing
                    .store
                    .upgrade()
                    .is_some_and(|current| !Arc::ptr_eq(&current, store))
                || !Weak::ptr_eq(&existing.controller, &Arc::downgrade(controller)))
        {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "agent {agent_id} already has a different live durable mailbox timeline"
                ),
            });
        }
        inner.insert(
            agent_id,
            MailboxRegistration {
                store: Arc::downgrade(store),
                mailbox_id,
                controller: Arc::downgrade(controller),
            },
        );
        Ok(())
    }

    pub(super) fn target(&self, agent_id: Uuid) -> Option<MailboxTarget> {
        let mut inner = self.inner.lock();
        let registration = inner.get(&agent_id)?;
        let store = registration_is_live(registration)
            .then(|| registration.store.upgrade())
            .flatten();
        let Some(store) = store else {
            inner.remove(&agent_id);
            return None;
        };
        Some(MailboxTarget {
            store,
            mailbox_id: registration.mailbox_id,
        })
    }

    pub(super) fn close(
        &self,
        agent_id: Uuid,
        controller: &Arc<PendingMailboxLease>,
    ) -> Option<MailboxTarget> {
        let mut inner = self.inner.lock();
        let registration = inner.get(&agent_id)?;
        let owns_registration = Weak::ptr_eq(&registration.controller, &Arc::downgrade(controller));
        if !owns_registration {
            return None;
        }
        let target = registration.store.upgrade().map(|store| MailboxTarget {
            store,
            mailbox_id: registration.mailbox_id,
        });
        controller.close();
        inner.remove(&agent_id);
        target
    }
}

fn registration_is_live(registration: &MailboxRegistration) -> bool {
    registration
        .controller
        .upgrade()
        .is_some_and(|lease| lease.is_open())
}

#[derive(Default)]
pub(super) struct RecipientEnqueueLocks {
    inner: Mutex<HashMap<Uuid, Weak<Mutex<()>>>>,
}

impl RecipientEnqueueLocks {
    pub(super) fn for_recipient(&self, recipient_id: Uuid) -> Arc<Mutex<()>> {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.get(&recipient_id).and_then(Weak::upgrade) {
            return existing;
        }
        let lock = Arc::new(Mutex::new(()));
        inner.insert(recipient_id, Arc::downgrade(&lock));
        lock
    }
}
