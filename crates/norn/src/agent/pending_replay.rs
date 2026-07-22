//! Fail-closed reconstruction of a recipient timeline's pending mailbox.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use uuid::Uuid;

use crate::error::SessionError;
use crate::r#loop::inbound::frame_message;
use crate::session::MailboxId;
use crate::session::events::SessionEvent;

use super::pending_delivery::{pending_delivery_message_id, pending_queue_message_id};
use super::pending_messages::{PendingAgentMessages, exact_frame, exact_pending, find_pending};
use super::pending_record::{
    AGENT_MESSAGE_DEQUEUED_EVENT_TYPE, AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingAgentMessage,
    PendingAgentMessageLifecycle,
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReplayQueueOwner {
    LocalCanonical,
    ForeignCanonical,
    Legacy,
}

struct ReplayQueueWitness {
    pending: PendingAgentMessage,
    owner: ReplayQueueOwner,
    queue_position: usize,
    consumed: bool,
}

struct ReplayState<'a> {
    events: &'a [SessionEvent],
    queues: HashMap<Uuid, ReplayQueueWitness>,
    legacy_user_rows: Vec<usize>,
    claimed_legacy_user_rows: HashSet<usize>,
}

impl<'a> ReplayState<'a> {
    fn new(events: &'a [SessionEvent]) -> Self {
        Self {
            events,
            queues: HashMap::new(),
            legacy_user_rows: Vec::new(),
            claimed_legacy_user_rows: HashSet::new(),
        }
    }
}

#[derive(Deserialize)]
struct LegacyDeliveredAudit {
    phase: String,
    message_id: Uuid,
    to_id: Uuid,
}

impl PendingAgentMessages {
    /// Rebuild a recipient mailbox from its durable timeline.
    ///
    /// New queue authority is bound to the session generation's `mailbox_id`,
    /// not the volatile runtime agent id. This makes direct resume work with a
    /// fresh runtime id while preventing a fork's inherited parent prefix from
    /// becoming the fork's mailbox. Foreign queue rows are retained only in
    /// this replay-local witness map so a following stable delivery can still
    /// be validated; the map is discarded before this function returns.
    ///
    /// Legacy rows without an authority marker are never adopted. The old
    /// writer copied identical rows into sender and parent timelines, while
    /// their `to_id` named only a volatile runtime instance. Neither equality
    /// nor mismatch with the current runtime proves durable mailbox ownership.
    /// Replay therefore fails closed on every unresolved legacy row instead of
    /// discarding or promoting it. An exact legacy `UserMessage` plus delivery
    /// audit proves completion regardless of which timeline retained the
    /// historical row. New authoritative rows never trust that secondary
    /// marker.
    ///
    /// # Errors
    ///
    /// Returns a typed replay error for malformed reserved rows, wrong event
    /// shapes, conflicting duplicate authority, a delivery record that does
    /// not exactly match its queued frame, or any unresolved legacy queue row
    /// whose durable mailbox ownership cannot be established.
    pub fn from_events(
        recipient_id: Uuid,
        mailbox_id: MailboxId,
        events: &[SessionEvent],
    ) -> Result<Self, SessionError> {
        let store = Self::new();
        let mut state = ReplayState::new(events);
        for (position, event) in events.iter().enumerate() {
            store.apply_replay_event(recipient_id, mailbox_id, position, event, &mut state)?;
        }
        ensure_no_unresolved_legacy_pending(&state)?;
        Ok(store)
    }

    fn apply_replay_event(
        &self,
        recipient_id: Uuid,
        mailbox_id: MailboxId,
        position: usize,
        event: &SessionEvent,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let queue_id = pending_queue_message_id(&event.base().id)?;
        let delivery_id = pending_delivery_message_id(&event.base().id)?;
        match event {
            SessionEvent::UserMessage { content, .. } => {
                if queue_id.is_some() {
                    return Err(invalid("queue event ID was used by a UserMessage"));
                }
                if let Some(message_id) = delivery_id {
                    self.consume_stable_delivery(message_id, content, state)?;
                } else {
                    observe_legacy_user_row(position, state);
                }
            }
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == AGENT_MESSAGE_QUEUED_EVENT_TYPE => {
                if delivery_id.is_some() {
                    return Err(invalid("delivery event ID was used by a queue row"));
                }
                self.replay_queue_row(
                    recipient_id,
                    mailbox_id,
                    position,
                    queue_id,
                    data.clone(),
                    state,
                )?;
            }
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == AGENT_MESSAGE_DEQUEUED_EVENT_TYPE => {
                if queue_id.is_some() || delivery_id.is_some() {
                    return Err(invalid(
                        "reserved pending-message event ID was used by a dequeue audit",
                    ));
                }
                Self::consume_legacy_dequeue(data.clone(), state)?;
            }
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE => {
                if queue_id.is_some() || delivery_id.is_some() {
                    return Err(invalid(
                        "reserved pending-message event ID was used by a delivered audit",
                    ));
                }
                Self::consume_legacy_delivered(data.clone(), state)?;
            }
            _ if queue_id.is_some() || delivery_id.is_some() => {
                return Err(invalid(
                    "reserved pending-message event ID was used by the wrong event shape",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn replay_queue_row(
        &self,
        recipient_id: Uuid,
        mailbox_id: MailboxId,
        position: usize,
        queue_id: Option<Uuid>,
        data: serde_json::Value,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let lifecycle = serde_json::from_value::<PendingAgentMessageLifecycle>(data)
            .map_err(|_error| invalid("agent_message.queued payload is malformed"))?;
        let Some((mut pending, authority)) = PendingAgentMessage::from_queued_event(lifecycle)
        else {
            return Err(invalid(
                "agent_message.queued event carries a non-queued lifecycle phase",
            ));
        };

        let owner = match authority {
            Some(true) => {
                if queue_id != Some(pending.message.id) {
                    return Err(invalid(
                        "canonical queue row does not use its message's stable event ID",
                    ));
                }
                let row_mailbox = pending
                    .mailbox_id
                    .ok_or_else(|| invalid("canonical queue row has no stable mailbox identity"))?;
                if pending.exact_message_timestamp.is_none() {
                    return Err(invalid(
                        "canonical queue row has no exact framed-message timestamp",
                    ));
                }
                if row_mailbox == mailbox_id {
                    pending.rebind_mailbox_owner(recipient_id);
                    ReplayQueueOwner::LocalCanonical
                } else {
                    ReplayQueueOwner::ForeignCanonical
                }
            }
            Some(false) => {
                if queue_id.is_some() {
                    return Err(invalid(
                        "non-authoritative queue observation uses the canonical ID namespace",
                    ));
                }
                if pending.mailbox_id.is_none() {
                    return Err(invalid(
                        "non-authoritative queue observation has no mailbox identity",
                    ));
                }
                if pending.exact_message_timestamp.is_none() {
                    return Err(invalid(
                        "non-authoritative queue observation has no exact message timestamp",
                    ));
                }
                return Ok(());
            }
            None => {
                if queue_id.is_some() {
                    return Err(invalid(
                        "legacy queue row unexpectedly uses the canonical ID namespace",
                    ));
                }
                if pending.mailbox_id.is_some() {
                    return Err(invalid(
                        "legacy queue row unexpectedly carries a mailbox identity",
                    ));
                }
                if pending.exact_message_timestamp.is_some() {
                    return Err(invalid(
                        "legacy queue row unexpectedly carries an exact message timestamp",
                    ));
                }
                ReplayQueueOwner::Legacy
            }
        };

        let message_id = pending.message.id;
        if let Some(existing) = state.queues.get(&message_id) {
            exact_pending(&existing.pending, &pending).map_err(|_error| {
                invalid("conflicting duplicate queue rows share one message ID")
            })?;
            if existing.owner != owner {
                return Err(invalid(
                    "duplicate queue rows disagree about recipient authority",
                ));
            }
            return Ok(());
        }
        if owner == ReplayQueueOwner::LocalCanonical {
            self.publish_replayed(pending.clone())?;
        }
        state.queues.insert(
            message_id,
            ReplayQueueWitness {
                pending,
                owner,
                queue_position: position,
                consumed: false,
            },
        );
        Ok(())
    }

    fn publish_replayed(&self, pending: PendingAgentMessage) -> Result<(), SessionError> {
        let message_id = pending.message.id;
        let mut inner = self.inner.lock();
        if let Some(existing) = find_pending(&inner, message_id) {
            return exact_pending(existing, &pending)
                .map_err(|_error| invalid("conflicting queued messages share one message ID"));
        }
        super::pending_messages::publish_pending(&mut inner, pending, true);
        Ok(())
    }

    fn consume_stable_delivery(
        &self,
        message_id: Uuid,
        content: &str,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let witness = state.queues.get_mut(&message_id).ok_or_else(|| {
            invalid("pending delivery row has no preceding canonical queue authority")
        })?;
        if witness.consumed {
            return Err(invalid("pending delivery row is duplicated"));
        }
        exact_frame(
            &frame_message(&witness.pending.message),
            content,
            message_id,
        )
        .map_err(|_error| invalid("pending delivery row conflicts with its queued frame"))?;
        witness.consumed = true;
        if witness.owner == ReplayQueueOwner::LocalCanonical {
            super::pending_messages::remove_message(&mut self.inner.lock(), message_id);
        }
        Ok(())
    }

    fn consume_legacy_dequeue(
        data: serde_json::Value,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let lifecycle = serde_json::from_value::<PendingAgentMessageLifecycle>(data)
            .map_err(|_error| invalid("agent_message.dequeued payload is malformed"))?;
        let PendingAgentMessageLifecycle::Dequeued {
            message_id, to_id, ..
        } = lifecycle
        else {
            return Err(invalid(
                "agent_message.dequeued event carries a non-dequeued lifecycle phase",
            ));
        };
        Self::consume_legacy_audit(message_id, to_id, state)
    }

    fn consume_legacy_delivered(
        data: serde_json::Value,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let lifecycle = serde_json::from_value::<LegacyDeliveredAudit>(data)
            .map_err(|_error| invalid("agent_message.delivered payload is malformed"))?;
        if lifecycle.phase != "delivered" {
            return Err(invalid(
                "agent_message.delivered event carries a non-delivered lifecycle phase",
            ));
        }
        Self::consume_legacy_audit(lifecycle.message_id, lifecycle.to_id, state)
    }

    fn consume_legacy_audit(
        message_id: Uuid,
        to_id: Uuid,
        state: &mut ReplayState<'_>,
    ) -> Result<(), SessionError> {
        let Some(witness) = state.queues.get_mut(&message_id) else {
            return Ok(());
        };
        if witness.owner != ReplayQueueOwner::Legacy || witness.consumed {
            return Ok(());
        }
        if witness.pending.message.to_id != to_id {
            return Err(invalid(
                "legacy delivery recipient conflicts with its queued message",
            ));
        }
        let expected = frame_message(&witness.pending.message);
        let matching_user = state.legacy_user_rows.iter().find(|candidate| {
            **candidate > witness.queue_position
                && !state.claimed_legacy_user_rows.contains(candidate)
                && matches!(
                    &state.events[**candidate],
                    SessionEvent::UserMessage { content, .. } if content == &expected
                )
        });
        let Some(matching_user) = matching_user else {
            return Err(invalid(
                "legacy delivery audit has no preceding exact framed UserMessage",
            ));
        };
        state.claimed_legacy_user_rows.insert(*matching_user);
        witness.consumed = true;
        Ok(())
    }
}

fn ensure_no_unresolved_legacy_pending(state: &ReplayState<'_>) -> Result<(), SessionError> {
    if state
        .queues
        .values()
        .any(|witness| witness.owner == ReplayQueueOwner::Legacy && !witness.consumed)
    {
        return Err(SessionError::PreD8PendingMessageOwnershipUnknown);
    }
    Ok(())
}

fn observe_legacy_user_row(position: usize, state: &mut ReplayState<'_>) {
    state.legacy_user_rows.push(position);
}

fn invalid(reason: &str) -> SessionError {
    SessionError::PendingMessageReplayInvalid {
        reason: reason.to_owned(),
    }
}
