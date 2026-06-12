//! `MessageRouter` — the workspace-shared routing surface for
//! inter-agent messages (Wave 3, replaces the orphaned `Mailbox` per
//! DECISION M5).
//!
//! The router is a directory of live inbound senders keyed by agent id,
//! with a per-recipient monotonic sequence counter minted at enqueue.
//! All messages travel the recipient's existing
//! [`InboundChannel`](crate::r#loop::inbound::InboundChannel) and drain
//! at the recipient loop's step boundaries — the router adds no second
//! queue and no sidecar storage. A message the router accepts is a
//! message some loop will drain; everything else is a typed
//! [`RouteError`], never a silent success.
//!
//! ## Registration ownership
//!
//! A route is registered when the recipient's inbound channel is
//! created and removed at its terminal transition — the same single
//! ownership as the registry entry (the spawn/fork wrapper, or
//! `close_agent`; never two actors). During the W3.1→W3.2 transition
//! the `signal_agent` tool registers routes idempotently from the
//! handles it already holds; the spawn/fork wrappers take over
//! registration when `send_message` lands (W3.2).
//! [`MessageRouter::register`] is idempotent per agent id and preserves
//! the sequence counter, so re-registration never resets per-recipient
//! ordering.
//!
//! ## Scope boundary (W3.2)
//!
//! The router routes by **capability of the harness**: any harness code
//! holding the router and a recipient id can deliver. *Permissioning* —
//! who may message whom ([`MessagingScope`] on `ChildPolicy`) — is
//! enforced by the `send_message` tool against registry ground truth in
//! W3.2, not here. The router is mechanism; policy lives one layer up.
//!
//! ## Ordering guarantee
//!
//! Sequence numbers are minted and the message enqueued under the same
//! lock (an [`InboundChannel`](crate::r#loop::inbound::InboundChannel)
//! permit is reserved *before* minting), so the order of messages on a
//! recipient's channel always matches their sequence numbers — even
//! under concurrent senders and even when the async and sync delivery
//! paths interleave. No cross-recipient order is promised.

use std::collections::HashMap;

use parking_lot::Mutex;
use uuid::Uuid;

use crate::r#loop::inbound::{ChannelMessage, InboundSender, InboundTrySendError};

/// Typed delivery failure from the router. Errors are honest: a message
/// that was not enqueued onto a channel some loop drains is reported as
/// exactly that, never queued into storage nothing reads.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// No live inbound channel is registered for the recipient. The
    /// recipient either never had an inbound channel (e.g. a root agent
    /// built without `inbound_capacity`) or was deregistered at its
    /// terminal transition.
    #[error("no live inbound route registered for agent {agent_id}")]
    NotRouted {
        /// The recipient that has no registered route.
        agent_id: Uuid,
    },
    /// The recipient's loop ended between route resolution and enqueue:
    /// its [`InboundChannel`](crate::r#loop::inbound::InboundChannel)
    /// receiver was dropped. The stale route is removed.
    #[error("inbound channel closed for agent {agent_id}: its loop has ended")]
    ChannelClosed {
        /// The recipient whose channel closed.
        agent_id: Uuid,
    },
    /// The recipient's bounded inbound channel is full and the caller
    /// used the non-blocking [`MessageRouter::try_deliver`] path (sync
    /// embeddings cannot await back-pressure). The message was **not**
    /// enqueued and no sequence number was consumed.
    #[error("inbound channel full for agent {agent_id}: bounded capacity exhausted")]
    ChannelFull {
        /// The recipient whose channel is at capacity.
        agent_id: Uuid,
    },
}

/// One recipient's routing state: its live inbound sender plus the
/// per-recipient monotonic sequence counter.
struct RouteEntry {
    inbound_tx: InboundSender,
    /// Last sequence number minted for this recipient (`0` = none yet;
    /// the first delivered message carries `seq == 1`). Minting uses
    /// `saturating_add`: at `u64::MAX` (~1.8×10¹⁹ sends, unreachable in
    /// any real session) later messages would share one number and
    /// total order would degrade to timestamp order rather than wrap
    /// or panic.
    last_seq: u64,
}

/// Workspace-shared directory of live inbound senders, keyed by agent
/// id. See the module docs for ownership, scope, and ordering rules.
pub struct MessageRouter {
    inner: Mutex<HashMap<Uuid, RouteEntry>>,
}

impl MessageRouter {
    /// Create an empty router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Register (or refresh) the live inbound sender for `agent_id`.
    ///
    /// Idempotent per agent id: re-registering replaces the sender but
    /// **preserves the sequence counter**, so per-recipient total order
    /// survives repeated registration of the same agent.
    pub fn register(&self, agent_id: Uuid, inbound_tx: InboundSender) {
        let mut inner = self.inner.lock();
        match inner.get_mut(&agent_id) {
            Some(entry) => entry.inbound_tx = inbound_tx,
            None => {
                inner.insert(
                    agent_id,
                    RouteEntry {
                        inbound_tx,
                        last_seq: 0,
                    },
                );
            }
        }
    }

    /// Remove the route for `agent_id`. Called at the recipient's
    /// terminal transition by the same actor that owns its registry
    /// entry. Removing an absent route is a no-op.
    pub fn deregister(&self, agent_id: Uuid) {
        self.inner.lock().remove(&agent_id);
    }

    /// Whether a live route is currently registered for `agent_id`.
    #[must_use]
    pub fn is_routed(&self, agent_id: Uuid) -> bool {
        self.inner.lock().contains_key(&agent_id)
    }

    /// Deliver `msg` to `to`, awaiting channel capacity if the
    /// recipient's bounded inbound channel is full.
    ///
    /// The router stamps `msg.to_id = to` and mints `msg.seq`; every
    /// other field is the caller's (the calling harness code resolves
    /// sender attribution from registry ground truth). Returns the
    /// minted per-recipient sequence number.
    ///
    /// # Errors
    ///
    /// - [`RouteError::NotRouted`] — no live route for `to` (also when
    ///   the route is deregistered while awaiting capacity).
    /// - [`RouteError::ChannelClosed`] — the recipient's loop ended;
    ///   the stale route is removed.
    pub async fn deliver(&self, to: Uuid, mut msg: ChannelMessage) -> Result<u64, RouteError> {
        loop {
            // Clone the sender out so the map lock is never held across
            // an await. The permit is reserved against this clone;
            // permits are bound to the underlying channel, not the
            // clone.
            let tx = {
                let inner = self.inner.lock();
                let entry = inner
                    .get(&to)
                    .ok_or(RouteError::NotRouted { agent_id: to })?;
                entry.inbound_tx.clone()
            };

            let Ok(permit) = tx.reserve().await else {
                let mut inner = self.inner.lock();
                match inner.get(&to) {
                    // Receiver dropped: the recipient's loop has ended.
                    // Remove the stale route so later sends fail fast
                    // as NotRouted.
                    Some(entry) if entry.inbound_tx.same_channel(&tx) => {
                        inner.remove(&to);
                        return Err(RouteError::ChannelClosed { agent_id: to });
                    }
                    // The closed channel was already superseded by a
                    // fresh registration — never remove the live route;
                    // retry against it.
                    Some(_) => continue,
                    None => return Err(RouteError::NotRouted { agent_id: to }),
                }
            };

            // Mint the sequence number and enqueue under one lock: the
            // permit send is synchronous and infallible, so channel
            // order always matches sequence order (module docs,
            // "Ordering").
            let mut inner = self.inner.lock();
            let Some(entry) = inner.get_mut(&to) else {
                // Deregistered while we awaited capacity — the terminal
                // transition won; do not enqueue into a dead loop's
                // buffer.
                return Err(RouteError::NotRouted { agent_id: to });
            };
            if !entry.inbound_tx.same_channel(&tx) {
                // The route was re-registered with a different channel
                // while we awaited capacity. The permit belongs to the
                // superseded channel: sending on it would burn a
                // sequence number on a message the live loop never
                // drains. Drop the permit and retry against the current
                // route.
                drop(inner);
                drop(permit);
                continue;
            }
            let seq = entry.last_seq.saturating_add(1);
            entry.last_seq = seq;
            msg.to_id = to;
            msg.seq = Some(seq);
            permit.send(msg);
            return Ok(seq);
        }
    }

    /// Non-blocking variant of [`MessageRouter::deliver`] for callers
    /// that cannot await (sync embeddings such as the Rhai bridge).
    ///
    /// # Errors
    ///
    /// - [`RouteError::NotRouted`] — no live route for `to`.
    /// - [`RouteError::ChannelFull`] — bounded capacity exhausted; the
    ///   message was not enqueued and no sequence number was consumed.
    /// - [`RouteError::ChannelClosed`] — the recipient's loop ended;
    ///   the stale route is removed.
    pub fn try_deliver(&self, to: Uuid, mut msg: ChannelMessage) -> Result<u64, RouteError> {
        let mut inner = self.inner.lock();
        let entry = inner
            .get_mut(&to)
            .ok_or(RouteError::NotRouted { agent_id: to })?;
        let seq = entry.last_seq.saturating_add(1);
        msg.to_id = to;
        msg.seq = Some(seq);
        match entry.inbound_tx.try_send(msg) {
            Ok(()) => {
                // Commit the counter only after a successful enqueue so
                // a Full failure consumes no sequence number.
                entry.last_seq = seq;
                Ok(seq)
            }
            Err(InboundTrySendError::Full) => Err(RouteError::ChannelFull { agent_id: to }),
            Err(InboundTrySendError::Closed) => {
                inner.remove(&to);
                Err(RouteError::ChannelClosed { agent_id: to })
            }
        }
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::*;
    use crate::r#loop::inbound::{InboundChannel, MessageKind, inbound_channel};

    fn message(sender: Uuid, content: &str, kind: MessageKind) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: sender,
            from: "/test/sender".to_owned(),
            role: None,
            to_id: Uuid::nil(),
            content: content.to_owned(),
            kind,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    fn routed(router: &MessageRouter, capacity: usize) -> (Uuid, InboundChannel) {
        let recipient = Uuid::new_v4();
        let (tx, rx) = inbound_channel(capacity);
        router.register(recipient, tx);
        (recipient, rx)
    }

    #[tokio::test]
    async fn deliver_stamps_recipient_and_monotonic_seq() {
        let router = MessageRouter::new();
        let sender = Uuid::new_v4();
        let (recipient, mut rx) = routed(&router, 8);

        for i in 1..=3u64 {
            let seq = router
                .deliver(
                    recipient,
                    message(sender, &format!("m{i}"), MessageKind::Steer),
                )
                .await
                .expect("deliver");
            assert_eq!(seq, i, "sequence numbers are minted 1..n");
        }

        let drained = rx.drain();
        assert_eq!(drained.len(), 3);
        for (i, msg) in drained.iter().enumerate() {
            assert_eq!(msg.seq, Some((i as u64) + 1), "channel order matches seq");
            assert_eq!(msg.to_id, recipient, "router stamps the recipient id");
            assert_eq!(msg.sender_id, sender);
        }
    }

    #[tokio::test]
    async fn per_recipient_sequences_are_independent() {
        let router = MessageRouter::new();
        let sender = Uuid::new_v4();
        let (a, _rx_a) = routed(&router, 8);
        let (b, _rx_b) = routed(&router, 8);

        let s1 = router
            .deliver(a, message(sender, "1", MessageKind::Steer))
            .await;
        let s2 = router
            .deliver(b, message(sender, "1", MessageKind::Steer))
            .await;
        let s3 = router
            .deliver(a, message(sender, "2", MessageKind::Steer))
            .await;
        assert_eq!(s1, Ok(1));
        assert_eq!(s2, Ok(1));
        assert_eq!(s3, Ok(2));
    }

    #[tokio::test]
    async fn deliver_to_unrouted_recipient_is_not_routed() {
        let router = MessageRouter::new();
        let recipient = Uuid::new_v4();
        let err = router
            .deliver(
                recipient,
                message(Uuid::new_v4(), "hi", MessageKind::Update),
            )
            .await
            .expect_err("no route");
        assert_eq!(
            err,
            RouteError::NotRouted {
                agent_id: recipient
            }
        );
    }

    #[tokio::test]
    async fn deliver_after_receiver_drop_reports_closed_and_removes_route() {
        let router = MessageRouter::new();
        let (recipient, rx) = routed(&router, 4);
        drop(rx);

        let err = router
            .deliver(recipient, message(Uuid::new_v4(), "hi", MessageKind::Steer))
            .await
            .expect_err("closed channel");
        assert_eq!(
            err,
            RouteError::ChannelClosed {
                agent_id: recipient
            }
        );
        assert!(
            !router.is_routed(recipient),
            "a closed route is removed so later sends fail fast",
        );
        let err = router
            .deliver(recipient, message(Uuid::new_v4(), "hi", MessageKind::Steer))
            .await
            .expect_err("route gone");
        assert_eq!(
            err,
            RouteError::NotRouted {
                agent_id: recipient
            }
        );
    }

    #[tokio::test]
    async fn deregister_removes_route() {
        let router = MessageRouter::new();
        let (recipient, _rx) = routed(&router, 4);
        assert!(router.is_routed(recipient));
        router.deregister(recipient);
        assert!(!router.is_routed(recipient));
        let err = router
            .deliver(recipient, message(Uuid::new_v4(), "hi", MessageKind::Steer))
            .await
            .expect_err("deregistered");
        assert_eq!(
            err,
            RouteError::NotRouted {
                agent_id: recipient
            }
        );
    }

    #[tokio::test]
    async fn reregistration_preserves_sequence_counter() {
        let router = MessageRouter::new();
        let sender = Uuid::new_v4();
        let recipient = Uuid::new_v4();
        let (tx1, mut rx1) = inbound_channel(4);
        router.register(recipient, tx1);
        let seq = router
            .deliver(recipient, message(sender, "first", MessageKind::Steer))
            .await
            .expect("deliver");
        assert_eq!(seq, 1);
        assert_eq!(rx1.drain().len(), 1);

        // Re-register the same agent id (e.g. signal_agent's idempotent
        // registration): the counter must not reset.
        let (tx2, mut rx2) = inbound_channel(4);
        router.register(recipient, tx2);
        let seq = router
            .deliver(recipient, message(sender, "second", MessageKind::Steer))
            .await
            .expect("deliver");
        assert_eq!(seq, 2, "sequence survives re-registration");
        let drained = rx2.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].seq, Some(2));
    }

    #[tokio::test]
    async fn try_deliver_full_channel_consumes_no_sequence() {
        let router = MessageRouter::new();
        let sender = Uuid::new_v4();
        let (recipient, mut rx) = routed(&router, 1);

        let seq = router
            .try_deliver(recipient, message(sender, "fits", MessageKind::Update))
            .expect("first fits");
        assert_eq!(seq, 1);
        let err = router
            .try_deliver(recipient, message(sender, "overflow", MessageKind::Update))
            .expect_err("capacity 1 exhausted");
        assert_eq!(
            err,
            RouteError::ChannelFull {
                agent_id: recipient
            }
        );

        // Drain and retry: the failed send must not have burned seq 2.
        assert_eq!(rx.drain().len(), 1);
        let seq = router
            .try_deliver(recipient, message(sender, "retry", MessageKind::Update))
            .expect("retry after drain");
        assert_eq!(seq, 2, "a Full failure consumes no sequence number");
    }

    #[tokio::test]
    async fn try_deliver_closed_channel_reports_and_removes_route() {
        let router = MessageRouter::new();
        let (recipient, rx) = routed(&router, 2);
        drop(rx);
        let err = router
            .try_deliver(recipient, message(Uuid::new_v4(), "hi", MessageKind::Steer))
            .expect_err("closed");
        assert_eq!(
            err,
            RouteError::ChannelClosed {
                agent_id: recipient
            }
        );
        assert!(!router.is_routed(recipient));
    }

    /// A route re-registered with a *different* channel while a
    /// `deliver` awaited capacity on the old one: the permit belongs to
    /// the superseded channel, so the deliver must retry against the
    /// live route — no silent loss, no sequence gap on the live channel.
    #[tokio::test]
    async fn deliver_retries_against_replaced_route_without_seq_gap() {
        let router = Arc::new(MessageRouter::new());
        let sender = Uuid::new_v4();
        let recipient = Uuid::new_v4();
        let (tx1, mut rx1) = inbound_channel(1);
        router.register(recipient, tx1);

        // Fill the capacity-1 channel so the next deliver awaits.
        let seq = router
            .deliver(recipient, message(sender, "first", MessageKind::Steer))
            .await
            .expect("first deliver");
        assert_eq!(seq, 1);

        let pending = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                router
                    .deliver(recipient, message(sender, "second", MessageKind::Steer))
                    .await
            })
        };
        // Let the pending deliver park on its capacity reservation.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        // Replace the route, then free capacity on the superseded
        // channel: the parked reservation resolves, the same-channel
        // check fails, and the deliver retries against the live route.
        let (tx2, mut rx2) = inbound_channel(4);
        router.register(recipient, tx2);
        assert_eq!(rx1.drain().len(), 1);

        let seq = pending
            .await
            .expect("join")
            .expect("deliver must succeed against the replaced route");
        assert_eq!(seq, 2, "the counter is preserved across the retry");
        let drained = rx2.drain();
        assert_eq!(drained.len(), 1, "the message lands on the live route");
        assert_eq!(drained[0].seq, Some(2));
        assert_eq!(drained[0].content, "second");
        assert!(
            rx1.drain().is_empty(),
            "nothing may be sent into the superseded channel",
        );
    }

    /// The superseded channel *closes* while a `deliver` awaits capacity
    /// on it: the deliver must not remove the freshly registered live
    /// route, and must retry against it.
    #[tokio::test]
    async fn deliver_retries_when_superseded_channel_closes_mid_await() {
        let router = Arc::new(MessageRouter::new());
        let sender = Uuid::new_v4();
        let recipient = Uuid::new_v4();
        let (tx1, rx1) = inbound_channel(1);
        router.register(recipient, tx1);
        router
            .deliver(recipient, message(sender, "first", MessageKind::Steer))
            .await
            .expect("first deliver");

        let pending = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                router
                    .deliver(recipient, message(sender, "second", MessageKind::Steer))
                    .await
            })
        };
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let (tx2, mut rx2) = inbound_channel(4);
        router.register(recipient, tx2);
        // Close the superseded channel: the parked reservation errors,
        // but the live route must survive and receive the retry.
        drop(rx1);

        let seq = pending
            .await
            .expect("join")
            .expect("deliver must retry against the live route");
        assert_eq!(seq, 2);
        assert!(
            router.is_routed(recipient),
            "a superseded channel closing must not remove the live route",
        );
        let drained = rx2.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].seq, Some(2));
    }

    /// Stress the per-recipient total order: many concurrent senders,
    /// mixed async/sync delivery paths, one recipient. The drained
    /// channel order must be exactly 1..n with no gaps or inversions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_senders_preserve_seq_total_order() {
        let router = Arc::new(MessageRouter::new());
        let recipient = Uuid::new_v4();
        let (tx, mut rx) = inbound_channel(1024);
        router.register(recipient, tx);

        let mut tasks = Vec::new();
        for t in 0..8u32 {
            let router = Arc::clone(&router);
            tasks.push(tokio::spawn(async move {
                let sender = Uuid::new_v4();
                for i in 0..32u32 {
                    let msg = message(sender, &format!("t{t}-m{i}"), MessageKind::Steer);
                    if (t + i) % 2 == 0 {
                        router.deliver(recipient, msg).await.expect("deliver");
                    } else {
                        router.try_deliver(recipient, msg).expect("try_deliver");
                    }
                }
            }));
        }
        for task in tasks {
            task.await.expect("sender task");
        }

        let drained = rx.drain();
        assert_eq!(drained.len(), 8 * 32);
        for (i, msg) in drained.iter().enumerate() {
            assert_eq!(
                msg.seq,
                Some((i as u64) + 1),
                "channel order must equal sequence order under concurrency",
            );
        }
    }
}
