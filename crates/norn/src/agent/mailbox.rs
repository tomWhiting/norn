//! Inter-agent messaging.
//!
//! [`Mailbox`] owns per-agent message queues, assigns monotonic sequence
//! numbers, and exposes [`Mailbox::wait_for_any`] so a parent agent can
//! await traffic on any of its children without polling.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

/// A message addressed from one agent to another.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MailboxMessage {
    /// Unique message identifier.
    pub id: Uuid,
    /// Sender agent id.
    pub from: Uuid,
    /// Recipient agent id.
    pub to: Uuid,
    /// Structured content payload.
    pub content: serde_json::Value,
    /// If `true`, the recipient should react at the next tool boundary
    /// rather than buffering until its current turn ends.
    pub trigger_turn: bool,
    /// Per-recipient monotonic sequence number.
    pub sequence: u64,
    /// Wall-clock timestamp at the moment of send.
    pub timestamp: DateTime<Utc>,
}

struct AgentInbox {
    pending: VecDeque<MailboxMessage>,
    next_seq: u64,
    seq_tx: watch::Sender<u64>,
}

impl AgentInbox {
    fn new() -> Self {
        let (seq_tx, _) = watch::channel(0u64);
        Self {
            pending: VecDeque::new(),
            next_seq: 0,
            seq_tx,
        }
    }
}

/// Mailbox routing messages between any number of agents.
///
/// One `Mailbox` owns inboxes for every addressable agent. Sending creates
/// the recipient inbox lazily so callers don't have to coordinate
/// initialisation order with the registry.
pub struct Mailbox {
    inboxes: Mutex<HashMap<Uuid, AgentInbox>>,
}

impl Mailbox {
    /// Create an empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inboxes: Mutex::new(HashMap::new()),
        }
    }

    /// Append a message to the recipient's queue and return the assigned
    /// sequence number.
    pub fn send(
        &self,
        from: Uuid,
        to: Uuid,
        content: serde_json::Value,
        trigger_turn: bool,
    ) -> u64 {
        let mut inboxes = self.inboxes.lock();
        let inbox = inboxes.entry(to).or_insert_with(AgentInbox::new);
        inbox.next_seq = inbox.next_seq.saturating_add(1);
        let sequence = inbox.next_seq;
        let msg = MailboxMessage {
            id: Uuid::new_v4(),
            from,
            to,
            content,
            trigger_turn,
            sequence,
            timestamp: Utc::now(),
        };
        inbox.pending.push_back(msg);
        // `send_replace` always succeeds even with zero subscribers.
        inbox.seq_tx.send_replace(sequence);
        sequence
    }

    /// Drain pending messages for `agent_id`. Returns an empty `Vec` if
    /// no messages are pending or the inbox has never seen traffic.
    pub fn recv(&self, agent_id: Uuid) -> Vec<MailboxMessage> {
        let mut inboxes = self.inboxes.lock();
        match inboxes.get_mut(&agent_id) {
            Some(inbox) => inbox.pending.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// Return the count of pending messages for `agent_id` without
    /// draining them.
    #[must_use]
    pub fn peek(&self, agent_id: Uuid) -> usize {
        let inboxes = self.inboxes.lock();
        inboxes
            .get(&agent_id)
            .map_or(0, |inbox| inbox.pending.len())
    }

    /// Return the current sequence number for `agent_id` (0 if absent).
    ///
    /// Useful for capturing a snapshot before calling
    /// [`Mailbox::wait_for_any`] so callers can detect any subsequent
    /// activity without missing a message.
    #[must_use]
    pub fn current_sequence(&self, agent_id: Uuid) -> u64 {
        let inboxes = self.inboxes.lock();
        inboxes.get(&agent_id).map_or(0, |inbox| inbox.next_seq)
    }

    /// Await new traffic on any of the listed agents past the matching
    /// `since_sequences` entry. Returns the subset of agent ids whose
    /// sequence has advanced.
    ///
    /// The wait is race-free: each per-agent `watch::Receiver` is
    /// subscribed under the inbox lock, so a [`Mailbox::send`] that
    /// completes after subscribe is guaranteed to wake the waiter.
    /// `since_sequences` is read positionally — entries shorter than
    /// `agent_ids` are treated as `0`.
    pub async fn wait_for_any(&self, agent_ids: &[Uuid], since_sequences: &[u64]) -> Vec<Uuid> {
        if agent_ids.is_empty() {
            return Vec::new();
        }

        let watches: Vec<(Uuid, watch::Receiver<u64>, u64)> = {
            let mut inboxes = self.inboxes.lock();
            agent_ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let since = since_sequences.get(i).copied().unwrap_or(0);
                    let inbox = inboxes.entry(*id).or_insert_with(AgentInbox::new);
                    (*id, inbox.seq_tx.subscribe(), since)
                })
                .collect()
        };

        let ready: Vec<Uuid> = watches
            .iter()
            .filter(|(_, rx, since)| *rx.borrow() > *since)
            .map(|(id, _, _)| *id)
            .collect();
        if !ready.is_empty() {
            return ready;
        }

        let futs = watches.into_iter().map(|(id, mut rx, since)| {
            Box::pin(async move {
                loop {
                    if rx.changed().await.is_err() {
                        return id;
                    }
                    if *rx.borrow() > since {
                        return id;
                    }
                }
            })
        });
        let (winner, _idx, _rest) =
            futures_util::future::select_all(futs.collect::<Vec<_>>()).await;
        vec![winner]
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn send_recv_drains_messages() {
        let mailbox = Mailbox::new();
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();

        for i in 0..3 {
            mailbox.send(from, to, serde_json::json!({ "i": i }), false);
        }
        assert_eq!(mailbox.peek(to), 3);

        let drained = mailbox.recv(to);
        assert_eq!(drained.len(), 3);
        for (i, msg) in drained.iter().enumerate() {
            assert_eq!(msg.sequence, (i as u64) + 1);
            assert_eq!(msg.from, from);
            assert_eq!(msg.to, to);
            assert_eq!(msg.content["i"], i);
        }

        assert_eq!(mailbox.recv(to).len(), 0);
        assert_eq!(mailbox.peek(to), 0);
    }

    #[tokio::test]
    async fn sequence_numbers_increment_per_recipient() {
        let mailbox = Mailbox::new();
        let from = Uuid::new_v4();
        let to_a = Uuid::new_v4();
        let to_b = Uuid::new_v4();

        let first_to_a = mailbox.send(from, to_a, serde_json::json!(1), false);
        let first_to_b = mailbox.send(from, to_b, serde_json::json!(1), false);
        let second_to_a = mailbox.send(from, to_a, serde_json::json!(2), false);

        assert_eq!(first_to_a, 1);
        assert_eq!(first_to_b, 1);
        assert_eq!(second_to_a, 2);
    }

    #[tokio::test]
    async fn trigger_turn_flag_preserved() {
        let mailbox = Mailbox::new();
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        mailbox.send(from, to, serde_json::json!("a"), false);
        mailbox.send(from, to, serde_json::json!("b"), true);

        let drained = mailbox.recv(to);
        assert!(!drained[0].trigger_turn);
        assert!(drained[1].trigger_turn);
    }

    #[tokio::test]
    async fn recv_on_empty_mailbox_returns_empty() {
        let mailbox = Mailbox::new();
        let id = Uuid::new_v4();
        assert!(mailbox.recv(id).is_empty());
        assert_eq!(mailbox.peek(id), 0);
        assert_eq!(mailbox.current_sequence(id), 0);
    }

    #[tokio::test]
    async fn wait_for_any_returns_immediately_if_seq_already_advanced() {
        let mailbox = Mailbox::new();
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        mailbox.send(from, to, serde_json::json!(1), false);

        let ready = mailbox.wait_for_any(&[to], &[0]).await;
        assert_eq!(ready, vec![to]);
    }

    #[tokio::test]
    async fn wait_for_any_wakes_on_send() {
        let mailbox = Arc::new(Mailbox::new());
        let recipient = Uuid::new_v4();
        let sender = Uuid::new_v4();

        let baseline = mailbox.current_sequence(recipient);
        let mailbox_clone = Arc::clone(&mailbox);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            mailbox_clone.send(sender, recipient, serde_json::json!("ping"), false);
        });

        let ready = mailbox.wait_for_any(&[recipient], &[baseline]).await;
        assert_eq!(ready, vec![recipient]);

        let messages = mailbox.recv(recipient);
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn wait_for_any_empty_ids_returns_empty() {
        let mailbox = Mailbox::new();
        let ready = mailbox.wait_for_any(&[], &[]).await;
        assert!(ready.is_empty());
    }

    #[tokio::test]
    async fn message_serde_roundtrip() {
        let mailbox = Mailbox::new();
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        mailbox.send(from, to, serde_json::json!({ "k": "v" }), true);
        let msg = mailbox.recv(to).into_iter().next().expect("one msg");
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: MailboxMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.from, from);
        assert_eq!(back.to, to);
        assert_eq!(back.sequence, 1);
        assert!(back.trigger_turn);
    }
}
