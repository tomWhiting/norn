//! Linearized live-route to idle/terminal mailbox transitions.

use std::sync::Arc;

use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::InboundChannel;
use crate::session::store::EventStore;

use super::message_router::MessageRouter;
use super::pending_mailbox::PendingMailboxLease;
use super::pending_messages::{PendingAgentMessage, PendingAgentMessages};
use super::pending_queue::{ClosedPendingMailbox, validate_expected_store};

pub(crate) struct MailboxRouteTransition {
    pub(crate) closed: Option<ClosedPendingMailbox>,
    pub(crate) first_error: Option<SessionError>,
    pub(crate) hard_failure: bool,
}

impl PendingAgentMessages {
    /// Remove a live route and durably sweep everything it accepted under the
    /// same recipient ordering gate used by fallback queueing.
    pub(crate) fn transition_live_route(
        &self,
        recipient_id: Uuid,
        store: &EventStore,
        router: &MessageRouter,
        inbound: &mut InboundChannel,
        terminal_controller: Option<&Arc<PendingMailboxLease>>,
    ) -> Result<MailboxRouteTransition, SessionError> {
        let enqueue_lock = self.enqueue_locks.for_recipient(recipient_id);
        let _enqueue_guard = enqueue_lock.lock();
        let terminal = terminal_controller.is_some();
        if terminal {
            // Failure must be terminal-safe too: after this point no direct
            // sender can succeed beyond the controller's final drain.
            inbound.close();
        }
        let target =
            self.mailboxes
                .target(recipient_id)
                .ok_or_else(|| SessionError::StorageError {
                    reason: format!(
                        "agent {recipient_id} has no live registered pending-message mailbox"
                    ),
                })?;
        validate_expected_store(recipient_id, Some(store), target.store.as_ref())?;

        let closed = terminal_controller.and_then(|controller| {
            self.mailboxes
                .close(recipient_id, controller)
                .map(|closed| ClosedPendingMailbox {
                    recipient_id,
                    store: closed.store,
                    mailbox_id: closed.mailbox_id,
                })
        });
        if terminal && closed.is_none() {
            return Err(SessionError::StorageError {
                reason: format!(
                    "agent {recipient_id} lost mailbox ownership during terminal transition"
                ),
            });
        }

        router.deregister(recipient_id);
        let mut first_error = None;
        let mut hard_failure = false;
        for mut message in inbound.drain() {
            message.to_id = recipient_id;
            let mut pending =
                PendingAgentMessage::new(message, recipient_id.to_string(), chrono::Utc::now());
            if let Err(error) = pending.bind_mailbox(recipient_id, target.mailbox_id) {
                hard_failure = true;
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
            let result = if terminal {
                self.stage_terminal_locked(&mut pending)
            } else {
                self.persist_and_publish_locked(target.store.as_ref(), &mut pending)
                    .map(|_| ())
            };
            if let Err(error) = result {
                hard_failure |= !self.exact_record_is_retained(&pending);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(closed) = closed.as_ref()
            && let Err(error) = self.finalize_closed_pending_locked(closed)
        {
            hard_failure |= self
                .terminal_pending_recovery_status(recipient_id)
                .is_none();
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        Ok(MailboxRouteTransition {
            closed,
            first_error,
            hard_failure,
        })
    }
}
