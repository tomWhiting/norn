//! Inbound channel for delivering messages to a running agent at controlled
//! tool boundaries.
//!
//! Inbound messages never interrupt streaming or a running tool. They are
//! drained at well-defined tool boundaries by the agent loop and injected
//! according to their [`DeliveryMode`]:
//!
//! - [`DeliveryMode::Steer`] — injected immediately after the current tool
//!   batch completes, before the next provider request.
//! - [`DeliveryMode::FollowUp`] — buffered until the model would otherwise
//!   stop; if any are present at stop time, they are injected and the loop
//!   continues.
//!
//! The channel is backed by a bounded `tokio::sync::mpsc` channel. Senders
//! are cheap to clone, allowing multiple producers to feed a single agent.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// When an inbound message is injected into the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryMode {
    /// Inject immediately after the current tool batch, before the next
    /// provider request.
    Steer,
    /// Buffer until the model would otherwise stop; inject only then.
    FollowUp,
}

/// A message destined for a running agent via an [`InboundChannel`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Logical author of the message (used for attribution in the injected
    /// user-role message).
    pub author: String,
    /// Message body.
    pub content: String,
    /// When the message should be injected.
    pub delivery: DeliveryMode,
    /// Send time, used to order multiple messages drained together.
    pub timestamp: DateTime<Utc>,
}

/// Receiver half of the inbound channel.
///
/// Owned by the agent loop. The loop calls [`InboundChannel::drain`] at each
/// tool boundary to pull all currently-buffered messages without awaiting.
pub struct InboundChannel {
    rx: mpsc::Receiver<ChannelMessage>,
}

impl InboundChannel {
    /// Drain all currently-buffered messages without awaiting.
    ///
    /// Returns the messages in the order they were sent. If the sender
    /// half has been dropped, returns whatever was buffered before the
    /// disconnect.
    pub fn drain(&mut self) -> Vec<ChannelMessage> {
        let mut drained = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            drained.push(msg);
        }
        drained
    }
}

/// Sender half of the inbound channel.
///
/// Cheaply cloneable so multiple producers can feed a single agent.
#[derive(Clone)]
pub struct InboundSender {
    tx: mpsc::Sender<ChannelMessage>,
}

impl InboundSender {
    /// Send a message to the agent.
    ///
    /// # Errors
    ///
    /// Returns [`mpsc::error::SendError`] if the receiving
    /// [`InboundChannel`] has been dropped.
    pub async fn send(
        &self,
        msg: ChannelMessage,
    ) -> Result<(), mpsc::error::SendError<ChannelMessage>> {
        self.tx.send(msg).await
    }
}

/// Construct a new inbound channel pair.
///
/// `buffer` is the bounded mpsc capacity. Producers awaiting [`InboundSender::send`]
/// will block when the buffer is full.
#[must_use]
pub fn inbound_channel(buffer: usize) -> (InboundSender, InboundChannel) {
    let (tx, rx) = mpsc::channel(buffer);
    (InboundSender { tx }, InboundChannel { rx })
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
    use super::*;

    fn make_message(author: &str, content: &str, delivery: DeliveryMode) -> ChannelMessage {
        ChannelMessage {
            author: author.to_string(),
            content: content.to_string(),
            delivery,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn send_three_drain_returns_all_in_order() {
        let (tx, mut rx) = inbound_channel(8);

        tx.send(make_message("alice", "first", DeliveryMode::Steer))
            .await
            .expect("send 1");
        tx.send(make_message("bob", "second", DeliveryMode::FollowUp))
            .await
            .expect("send 2");
        tx.send(make_message("carol", "third", DeliveryMode::Steer))
            .await
            .expect("send 3");

        let drained = rx.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].author, "alice");
        assert_eq!(drained[0].content, "first");
        assert_eq!(drained[0].delivery, DeliveryMode::Steer);
        assert_eq!(drained[1].author, "bob");
        assert_eq!(drained[1].delivery, DeliveryMode::FollowUp);
        assert_eq!(drained[2].author, "carol");
    }

    #[tokio::test]
    async fn drain_empty_channel_returns_empty_vec() {
        let (_tx, mut rx) = inbound_channel(8);
        let drained = rx.drain();
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn drain_after_sender_dropped_returns_buffered_messages() {
        let (tx, mut rx) = inbound_channel(4);
        tx.send(make_message("alice", "buffered", DeliveryMode::Steer))
            .await
            .expect("send");
        drop(tx);

        let drained = rx.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].content, "buffered");
    }

    #[tokio::test]
    async fn drain_after_drain_returns_empty() {
        let (tx, mut rx) = inbound_channel(4);
        tx.send(make_message("a", "x", DeliveryMode::Steer))
            .await
            .expect("send");

        let first = rx.drain();
        assert_eq!(first.len(), 1);

        let second = rx.drain();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn sender_is_clone_and_send() {
        let (tx, mut rx) = inbound_channel(4);
        let tx2 = tx.clone();
        tx.send(make_message("a", "from-original", DeliveryMode::Steer))
            .await
            .expect("send");
        tx2.send(make_message("a", "from-clone", DeliveryMode::Steer))
            .await
            .expect("send");

        let drained = rx.drain();
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn delivery_mode_serde_roundtrip() {
        let original = DeliveryMode::FollowUp;
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: DeliveryMode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn delivery_mode_copy() {
        let a = DeliveryMode::Steer;
        let b = a;
        assert_eq!(a, b);
    }
}
