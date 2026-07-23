//! Recovery authority for terminal messages whose queue write did not finish.

use std::sync::Arc;

use uuid::Uuid;

use crate::error::SessionError;
use crate::session::MailboxId;
use crate::session::store::EventStore;

use super::pending_messages::PendingAgentMessages;
use super::pending_queue::ClosedPendingMailbox;

/// Payload-free status for accepted terminal messages awaiting durable queueing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPendingRecoveryStatus {
    /// Number of exact queue records that are not yet known durable.
    pub pending_count: usize,
}

/// Payload-free status for every retained queue record not yet known durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NondurablePendingStatus {
    /// Number of retained records whose canonical queue append is unresolved.
    pub pending_count: usize,
}

/// Result of an explicit terminal queue recovery attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPendingRetryOutcome {
    /// No unresolved recovery authority exists for this recipient.
    NoRecovery,
    /// Every retained record became durable in this attempt.
    Recovered {
        /// Number of records whose recovery authority was discharged.
        retained_count: usize,
    },
}

#[derive(Clone)]
pub(super) struct TerminalPendingRecovery {
    mailbox_id: MailboxId,
    store: Arc<EventStore>,
}

impl PendingAgentMessages {
    /// Promote retained live-mailbox failures into terminal recovery.
    ///
    /// Each nondurable live record owns exact mailbox/store authority. Promotion
    /// validates one generation and store for the complete unresolved FIFO,
    /// then moves that authority into the terminal retry surface without I/O.
    pub(crate) fn promote_nondurable_for_terminal(
        &self,
        recipient_id: Uuid,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let authority = {
            let inner = self.inner.lock();
            let mut authority = inner
                .terminal_recoveries
                .get(&recipient_id)
                .map(
                    |recovery| super::pending_record::PendingPersistenceAuthority {
                        mailbox_id: recovery.mailbox_id,
                        store: Arc::clone(&recovery.store),
                    },
                );
            if let Some(queue) = inner.by_recipient.get(&recipient_id) {
                for pending in queue.iter().filter(|pending| !pending.queue_durable) {
                    let candidate = pending.persistence_authority.as_ref();
                    if candidate.is_none()
                        && pending.terminal_recovery
                        && authority
                            .as_ref()
                            .is_some_and(|current| pending.mailbox_id == Some(current.mailbox_id))
                    {
                        continue;
                    }
                    let Some(candidate) = candidate else {
                        return Err(SessionError::EventAppendFailed {
                            reason: format!(
                                "agent {recipient_id} has a nondurable record without recovery authority"
                            ),
                        });
                    };
                    if authority.as_ref().is_some_and(
                        |current: &super::pending_record::PendingPersistenceAuthority| {
                            current.mailbox_id != candidate.mailbox_id
                                || !Arc::ptr_eq(&current.store, &candidate.store)
                        },
                    ) {
                        return Err(SessionError::EventAppendFailed {
                            reason: format!(
                                "agent {recipient_id} has nondurable records for different mailbox authorities"
                            ),
                        });
                    }
                    authority = Some(candidate.clone());
                }
            }
            authority
        };
        let Some(authority) = authority else {
            return Ok(());
        };
        self.adopt_closed_pending_locked(&ClosedPendingMailbox {
            recipient_id,
            store: authority.store,
            mailbox_id: authority.mailbox_id,
        })
    }

    /// Reconcile the complete pending FIFO owned by a mailbox that has closed.
    ///
    /// Existing live-route failures and newly drained records are first adopted
    /// by a strong recovery authority. Queue I/O then retries their exact cached
    /// events in FIFO order. An error leaves every unresolved record and its
    /// store authority reachable through [`Self::retry_terminal_pending`].
    pub(crate) fn finalize_closed_pending(
        &self,
        closed: &ClosedPendingMailbox,
    ) -> Result<(), SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(closed.recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        self.finalize_closed_pending_locked(closed)
    }

    pub(super) fn finalize_closed_pending_locked(
        &self,
        closed: &ClosedPendingMailbox,
    ) -> Result<(), SessionError> {
        self.adopt_closed_pending_locked(closed)?;
        let attempt =
            self.ensure_recipient_prefix_durable_locked(closed.recipient_id, closed.store.as_ref());

        let mut inner = self.inner.lock();
        super::pending_queue::retire_durable_terminal_records(
            &mut inner,
            closed.recipient_id,
            Some(closed.mailbox_id),
        );
        if unresolved_count(&inner, closed.recipient_id, closed.mailbox_id) == 0 {
            inner.terminal_recoveries.remove(&closed.recipient_id);
        }
        attempt
    }

    fn adopt_closed_pending_locked(
        &self,
        closed: &ClosedPendingMailbox,
    ) -> Result<(), SessionError> {
        let recipient_id = closed.recipient_id;
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.terminal_recoveries.get(&recipient_id)
            && (existing.mailbox_id != closed.mailbox_id
                || !Arc::ptr_eq(&existing.store, &closed.store))
        {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "agent {recipient_id} already has terminal recovery authority for a different mailbox"
                ),
            });
        }
        if inner.by_recipient.get(&recipient_id).is_some_and(|queue| {
            queue.iter().any(|pending| {
                !pending.queue_durable && pending.mailbox_id != Some(closed.mailbox_id)
            })
        }) {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "agent {recipient_id} has nondurable records outside the closing mailbox"
                ),
            });
        }
        if inner.by_recipient.get(&recipient_id).is_some_and(|queue| {
            queue.iter().any(|pending| {
                !pending.queue_durable
                    && match pending.persistence_authority.as_ref() {
                        Some(authority) => {
                            authority.mailbox_id != closed.mailbox_id
                                || !Arc::ptr_eq(&authority.store, &closed.store)
                        }
                        None => !pending.terminal_recovery,
                    }
            })
        }) {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "agent {recipient_id} has nondurable records without the closing mailbox authority"
                ),
            });
        }

        let mut adopted = false;
        if let Some(queue) = inner.by_recipient.get_mut(&recipient_id) {
            for pending in queue
                .iter_mut()
                .filter(|pending| pending.mailbox_id == Some(closed.mailbox_id))
            {
                pending.terminal_recovery = true;
                pending.persistence_authority = None;
                adopted = true;
            }
        }
        if adopted {
            inner.terminal_recoveries.insert(
                recipient_id,
                TerminalPendingRecovery {
                    mailbox_id: closed.mailbox_id,
                    store: Arc::clone(&closed.store),
                },
            );
        } else {
            inner.terminal_recoveries.remove(&recipient_id);
        }
        Ok(())
    }

    /// Return payload-free status for unresolved terminal queue persistence.
    #[must_use]
    pub fn terminal_pending_recovery_status(
        &self,
        recipient_id: Uuid,
    ) -> Option<TerminalPendingRecoveryStatus> {
        let mut inner = self.inner.lock();
        let mailbox_id = inner.terminal_recoveries.get(&recipient_id)?.mailbox_id;
        let pending_count = unresolved_count(&inner, recipient_id, mailbox_id);
        if pending_count == 0 {
            inner.terminal_recoveries.remove(&recipient_id);
            return None;
        }
        Some(TerminalPendingRecoveryStatus { pending_count })
    }

    /// Return payload-free status for any retained nondurable queue record.
    #[must_use]
    pub fn nondurable_pending_status(&self, recipient_id: Uuid) -> Option<NondurablePendingStatus> {
        let pending_count = self
            .inner
            .lock()
            .by_recipient
            .get(&recipient_id)
            .map_or(0, |queue| {
                queue
                    .iter()
                    .filter(|pending| !pending.queue_durable)
                    .count()
            });
        (pending_count != 0).then_some(NondurablePendingStatus { pending_count })
    }

    /// Retry every retained terminal queue record in recipient FIFO order.
    ///
    /// A successful retry retires the in-memory recovery copies after their
    /// exact queue records become durable for a future direct resume.
    ///
    /// # Errors
    ///
    /// Returns the typed storage error while exact records remain retained.
    pub fn retry_terminal_pending(
        &self,
        recipient_id: Uuid,
    ) -> Result<TerminalPendingRetryOutcome, SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let recovery = {
            let inner = self.inner.lock();
            inner.terminal_recoveries.get(&recipient_id).cloned()
        };
        let Some(recovery) = recovery else {
            return Ok(TerminalPendingRetryOutcome::NoRecovery);
        };
        let retained_count = {
            let inner = self.inner.lock();
            let queue = inner.by_recipient.get(&recipient_id);
            if queue.is_some_and(|queue| {
                queue.iter().any(|pending| {
                    !pending.queue_durable && pending.mailbox_id != Some(recovery.mailbox_id)
                })
            }) {
                return Err(SessionError::EventAppendFailed {
                    reason: format!(
                        "agent {recipient_id} has unresolved terminal records for more than one mailbox"
                    ),
                });
            }
            unresolved_count(&inner, recipient_id, recovery.mailbox_id)
        };
        self.ensure_recipient_prefix_durable_locked(recipient_id, recovery.store.as_ref())?;
        let mut inner = self.inner.lock();
        super::pending_queue::retire_durable_terminal_records(
            &mut inner,
            recipient_id,
            Some(recovery.mailbox_id),
        );
        let unresolved = unresolved_count(&inner, recipient_id, recovery.mailbox_id);
        if unresolved != 0 {
            return Err(SessionError::TerminalPendingMessagesUnresolved {
                pending_count: unresolved,
            });
        }
        inner.terminal_recoveries.remove(&recipient_id);
        Ok(TerminalPendingRetryOutcome::Recovered { retained_count })
    }
}

fn unresolved_count(
    inner: &super::pending_messages::PendingInner,
    recipient_id: Uuid,
    mailbox_id: MailboxId,
) -> usize {
    inner.by_recipient.get(&recipient_id).map_or(0, |queue| {
        queue
            .iter()
            .filter(|pending| {
                pending.terminal_recovery
                    && pending.mailbox_id == Some(mailbox_id)
                    && !pending.queue_durable
            })
            .count()
    })
}
