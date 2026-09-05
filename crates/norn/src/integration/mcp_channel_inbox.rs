//! Single-recipient retained inbox; claims preserve count and byte admission charges.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{Notify, watch};
use uuid::Uuid;

use super::mcp_channel_source::{McpChannelAttachment, SourceRecord};
use super::mcp_channels::{
    McpChannelError, McpChannelLimits, McpChannelMessage, McpChannelOverflow, McpChannelPolicy,
    McpChannelRejection, McpChannelStatus,
};

pub(super) struct InboxShared {
    // One short critical section serializes admission and owner transitions.
    // No guard leaves a synchronous method and no I/O occurs while it is held.
    pub(super) state: Mutex<InboxState>,
    pub(super) changed: Notify,
    pub(super) status: watch::Sender<McpChannelStatus>,
}

pub(super) struct InboxState {
    pub(super) recipient_id: Uuid,
    pub(super) limits: McpChannelLimits,
    pub(super) queue: VecDeque<RetainedMessage>,
    pub(super) sources: HashMap<u64, SourceRecord>,
    pub(super) retained_bytes: usize,
    pub(super) sequence: u64,
    pub(super) rejected: u64,
    pub(super) rejection_count_exhausted: bool,
    pub(super) last_rejection: Option<McpChannelRejection>,
    pub(super) closed: bool,
}

pub(super) struct RetainedMessage {
    pub(super) message: Arc<McpChannelMessage>,
    pub(super) bytes: usize,
    pub(super) policy: McpChannelPolicy,
    pub(super) published: bool,
    pub(super) claimed: bool,
}

impl InboxState {
    pub(super) fn snapshot(&self) -> McpChannelStatus {
        McpChannelStatus {
            recipient_id: self.recipient_id,
            limits: self.limits,
            retained_messages: self.queue.len(),
            retained_bytes: self.retained_bytes,
            rejected: self.rejected,
            rejection_count_exhausted: self.rejection_count_exhausted,
            last_rejection: self.last_rejection.clone(),
            closed: self.closed,
        }
    }
}

impl InboxShared {
    pub(super) fn publish(&self, state: &InboxState) {
        self.status.send_replace(state.snapshot());
        self.changed.notify_one();
    }

    pub(super) fn reject(&self, state: &mut InboxState, rejection: McpChannelRejection) {
        match state.rejected.checked_add(1) {
            Some(count) => state.rejected = count,
            None => state.rejection_count_exhausted = true,
        }
        tracing::warn!(source = %rejection.source, generation = rejection.generation,
            recipient = %rejection.recipient_id, reason = %rejection.reason,
            "MCP channel event refused; notification has no upstream acknowledgement");
        state.last_rejection = Some(rejection);
        self.publish(state);
    }

    fn settle(&self, message_id: Uuid) -> Result<(), McpChannelError> {
        let mut state = self.state.lock();
        let index = state
            .queue
            .iter()
            .position(|item| item.message.id == message_id && item.claimed)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        let bytes = state
            .queue
            .get(index)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?
            .bytes;
        let retained_bytes = state
            .retained_bytes
            .checked_sub(bytes)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        state
            .queue
            .remove(index)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        state.retained_bytes = retained_bytes;
        self.publish(&state);
        Ok(())
    }
}

/// Cloneable host authority to configure sources and inspect the one owned inbox.
#[derive(Clone)]
pub struct McpChannelHost {
    pub(super) shared: Arc<InboxShared>,
}

impl McpChannelHost {
    /// Create an explicit source attachment; source name/generation are bound by the MCP client.
    pub fn attachment(
        &self,
        policy: McpChannelPolicy,
        overflow: McpChannelOverflow,
    ) -> McpChannelAttachment {
        McpChannelAttachment {
            shared: Arc::clone(&self.shared),
            policy,
            overflow,
        }
    }

    /// Current occupancy and visible admission refusals.
    pub fn status(&self) -> McpChannelStatus {
        self.shared.state.lock().snapshot()
    }

    /// Push subscription to current occupancy and refusal state.
    pub fn subscribe_status(&self) -> watch::Receiver<McpChannelStatus> {
        self.shared.status.subscribe()
    }

    /// Release one held message without changing its origin, order or admission charge.
    pub fn release(
        &self,
        message_id: Uuid,
        policy: McpChannelPolicy,
    ) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        let item = state
            .queue
            .iter_mut()
            .find(|item| item.message.id == message_id && !item.claimed)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        item.policy = policy;
        self.shared.publish(&state);
        Ok(())
    }

    /// Explicitly deny one unclaimed held message and release its retained charge.
    /// The caller must persist the decision first if durable inbox history is required.
    pub fn deny(&self, message_id: Uuid) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        let index = state
            .queue
            .iter()
            .position(|item| {
                item.message.id == message_id
                    && !item.claimed
                    && item.policy == McpChannelPolicy::Hold
            })
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        let item = state
            .queue
            .get(index)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        let retained_bytes = state
            .retained_bytes
            .checked_sub(item.bytes)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        tracing::info!(source = %item.message.source, generation = item.message.generation,
            recipient = %state.recipient_id, message = %message_id, "owner denied held MCP channel message");
        state
            .queue
            .remove(index)
            .ok_or(McpChannelError::MessageUnavailable { message_id })?;
        state.retained_bytes = retained_bytes;
        self.shared.publish(&state);
        Ok(())
    }
}

/// Sole receiving owner of a retained external-message inbox; this type is not cloneable.
pub struct McpChannelInbox {
    shared: Arc<InboxShared>,
}

impl McpChannelInbox {
    /// Create one recipient-scoped inbox with caller-supplied positive limits.
    pub fn new(recipient_id: Uuid, limits: McpChannelLimits) -> Self {
        let state = InboxState {
            recipient_id,
            limits,
            queue: VecDeque::new(),
            sources: HashMap::new(),
            retained_bytes: 0,
            sequence: 0,
            rejected: 0,
            rejection_count_exhausted: false,
            last_rejection: None,
            closed: false,
        };
        let (status, receiver) = watch::channel(state.snapshot());
        drop(receiver);
        let shared = Arc::new(InboxShared {
            state: Mutex::new(state),
            changed: Notify::new(),
            status,
        });
        Self { shared }
    }

    /// Clone the host authority without creating another receiving owner.
    pub fn host(&self) -> McpChannelHost {
        McpChannelHost {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Local identifiers of published held messages, in admission order.
    pub fn held_message_ids(&self) -> Vec<Uuid> {
        self.shared
            .state
            .lock()
            .queue
            .iter()
            .filter(|item| item.published && item.policy == McpChannelPolicy::Hold)
            .map(|item| item.message.id)
            .collect()
    }

    /// Claim the earliest published, non-held and unclaimed event without freeing quota.
    pub fn try_claim(&mut self) -> Result<Option<McpChannelDelivery>, McpChannelError> {
        self.claim_matching(false, u64::MAX)
    }

    /// Claim only a Wake message; `NextTurn` remains retained for a newly started turn.
    pub fn try_claim_wake(&mut self) -> Result<Option<McpChannelDelivery>, McpChannelError> {
        self.claim_matching(true, u64::MAX)
    }

    pub(crate) fn admitted_sequence(&self) -> u64 {
        self.shared.state.lock().sequence
    }

    pub(crate) fn claim_through(
        &mut self,
        wake_only: bool,
        sequence: u64,
    ) -> Result<Option<McpChannelDelivery>, McpChannelError> {
        self.claim_matching(wake_only, sequence)
    }

    fn claim_matching(
        &mut self,
        wake_only: bool,
        sequence: u64,
    ) -> Result<Option<McpChannelDelivery>, McpChannelError> {
        let mut state = self.shared.state.lock();
        let Some(item) = state.queue.iter_mut().find(|item| {
            item.published
                && !item.claimed
                && item.message.sequence <= sequence
                && item.policy != McpChannelPolicy::Hold
                && (!wake_only || item.policy == McpChannelPolicy::Wake)
        }) else {
            if state.closed {
                return Err(McpChannelError::Closed {
                    recipient_id: state.recipient_id,
                });
            }
            return Ok(None);
        };
        item.claimed = true;
        Ok(Some(McpChannelDelivery {
            shared: Arc::clone(&self.shared),
            message: Arc::clone(&item.message),
            wakes_session: item.policy == McpChannelPolicy::Wake,
            settled: false,
        }))
    }

    /// Await an eligible event using push notification; cancellation consumes nothing.
    pub async fn claim(&mut self) -> Result<McpChannelDelivery, McpChannelError> {
        loop {
            let shared = Arc::clone(&self.shared);
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(delivery) = self.try_claim()? {
                return Ok(delivery);
            }
            changed.await;
        }
    }

    /// Await a published Wake event without draining held or `NextTurn` events.
    pub async fn wake_ready(&self) -> Result<(), McpChannelError> {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.shared.state.lock();
                if state.queue.iter().any(|item| {
                    item.published && !item.claimed && item.policy == McpChannelPolicy::Wake
                }) {
                    return Ok(());
                }
                if state.closed {
                    return Err(McpChannelError::Closed {
                        recipient_id: state.recipient_id,
                    });
                }
            }
            changed.await;
        }
    }

    /// Stop new admission while preserving already retained messages for final handling.
    pub fn close(&mut self) {
        let mut state = self.shared.state.lock();
        state.closed = true;
        self.shared.publish(&state);
    }
}

impl Drop for McpChannelInbox {
    fn drop(&mut self) {
        self.close();
        let state = self.shared.state.lock();
        if !state.queue.is_empty() {
            tracing::warn!(recipient = %state.recipient_id, retained_messages = state.queue.len(),
                retained_bytes = state.retained_bytes,
                "MCP channel receiving owner dropped with in-memory messages still retained; no durable receipt is implied");
        }
    }
}

/// Exclusive claim on one event; dropping an unsettled claim restores eligibility.
#[must_use = "consume a delivered event or drop its claim to leave it retained"]
pub struct McpChannelDelivery {
    shared: Arc<InboxShared>,
    message: Arc<McpChannelMessage>,
    wakes_session: bool,
    settled: bool,
}

impl McpChannelDelivery {
    pub(crate) const fn wakes_session(&self) -> bool {
        self.wakes_session
    }

    /// Borrow the immutable event without releasing its retained-byte charge.
    pub fn message(&self) -> &McpChannelMessage {
        &self.message
    }

    /// Release quota only after the owning host has consumed/persisted this event.
    /// This in-memory transition is not an upstream or durable delivery receipt.
    pub fn consume(mut self) -> Result<(), McpChannelError> {
        self.consume_retained()
    }

    pub(crate) fn consume_retained(&mut self) -> Result<(), McpChannelError> {
        self.shared.settle(self.message.id)?;
        self.settled = true;
        Ok(())
    }
}

impl Drop for McpChannelDelivery {
    fn drop(&mut self) {
        if !self.settled {
            let mut state = self.shared.state.lock();
            if let Some(item) = state
                .queue
                .iter_mut()
                .find(|item| item.message.id == self.message.id)
            {
                item.claimed = false;
                self.shared.publish(&state);
            }
        }
    }
}

#[cfg(test)]
#[path = "mcp_channel_inbox_tests.rs"]
mod tests;
