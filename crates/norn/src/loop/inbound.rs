//! Inbound channel for delivering messages to a running agent at controlled
//! tool boundaries, plus the harness-built XML frame those messages are
//! injected with.
//!
//! Inbound messages never interrupt streaming or a running tool. They are
//! drained at well-defined tool boundaries by the agent loop and injected
//! according to their [`MessageKind`]:
//!
//! - [`MessageKind::Steer`] — act on this: injected immediately after the
//!   current tool batch completes, before the next provider request.
//! - [`MessageKind::Update`] — FYI context: buffered until the model would
//!   otherwise stop; if any are present at stop time, they are injected and
//!   the loop continues.
//!
//! The channel is backed by a bounded `tokio::sync::mpsc` channel. Senders
//! are cheap to clone, allowing multiple producers to feed a single agent.
//!
//! ## Framing and attribution (security contract)
//!
//! The injected user-role turn is built **by the harness** via
//! [`frame_message`]: an `<agent_message ...>` wrapper whose attributes
//! ([`ChannelMessage::from`], `from_id`, optional `role`, `kind`, `seq`,
//! `ts`) come from harness-resolved values — registry ground truth for
//! agent senders — never from sender-controlled text. The sender's
//! `content` is XML-entity-escaped ([`escape_xml`]) before framing, so a
//! malicious sender cannot close the frame, forge a second frame, or
//! impersonate another agent: its bytes are data inside an encoding, the
//! same structural barrier JSON encoding gives tool results. This replaces
//! the prior `[Inbound from {author}]: {content}` format, whose
//! sender-supplied `author` and unescaped content were forgeable.

use std::{fmt, sync::Arc};

use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Default)]
struct InboundAdmission {
    closed: bool,
}

/// How the recipient should treat an inter-agent message (DECISION M2).
///
/// On the existing loop this maps `Steer` onto immediate post-batch
/// injection and `Update` onto FollowUp-style batching at stop
/// boundaries. When linger-await lands (W3.3), `Update` additionally
/// does **not** wake a lingering agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Act on this: drains at the next step boundary.
    Steer,
    /// FYI context: batches at step boundaries.
    Update,
}

impl MessageKind {
    /// Stable `snake_case` label used in frames and tool payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Update => "update",
        }
    }
}

/// A message destined for a running agent via an [`InboundChannel`].
///
/// Constructed only by harness code (the
/// [`MessageRouter`](crate::agent::message_router::MessageRouter), the
/// coordination tools, or a trusted embedder holding an
/// [`InboundSender`]). The attribution fields are resolved by the
/// constructor from registry ground truth — the sending *model* never
/// controls them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Unique message identifier, shared by the `agent_message.sent` and
    /// `agent_message.delivered` audit events.
    pub id: Uuid,
    /// Sender agent id (ground truth, set by the harness constructor).
    pub sender_id: Uuid,
    /// Harness-resolved attribution label: the sender's live registry
    /// path, else its tombstone path, the literal `root` for an
    /// unregistered root agent, else the bare UUID. Escaped before it
    /// enters the frame.
    pub from: String,
    /// The sender's registry role — present only when a role was
    /// actually set at spawn (forks carry `"fork"`). Never synthesized.
    pub role: Option<String>,
    /// Recipient agent id. Stamped by the router on routed deliveries;
    /// set by the caller on direct handle sends.
    pub to_id: Uuid,
    /// Message body (raw; escaped at framing time).
    pub content: String,
    /// How the recipient should treat the message.
    pub kind: MessageKind,
    /// Router-minted per-recipient sequence number — the authoritative
    /// per-recipient order. `None` for direct handle sends that bypass
    /// the router (e.g. `close_agent`'s shutdown steer, embedder sends
    /// to a root agent); such messages order by timestamp after all
    /// sequenced messages in a drained batch.
    pub seq: Option<u64>,
    /// Send time (display / fallback ordering only; `seq` is
    /// authoritative where present).
    pub timestamp: DateTime<Utc>,
}

/// XML-entity-escape `s` for both text and attribute positions:
/// `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, `"` → `&quot;`.
///
/// Escaping all four everywhere is deliberate defense-in-depth: content
/// only needs the first three, attributes the quote — applying the full
/// set uniformly leaves no position-dependent reasoning to get wrong.
#[must_use]
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// Build the harness-framed injection text for an inbound message.
///
/// There is exactly one injection formatter: every drained
/// [`ChannelMessage`] — router-delivered, `close_agent` steer, or
/// embedder send — renders through this function. The persisted
/// `UserMessage` event stores this exact string, so the audit record is
/// byte-identical to what the model saw and resume replays it verbatim.
#[must_use]
pub fn frame_message(msg: &ChannelMessage) -> String {
    let role_attr = msg.role.as_deref().map_or_else(String::new, |role| {
        format!(" role=\"{}\"", escape_xml(role))
    });
    let seq_attr = msg
        .seq
        .map_or_else(String::new, |seq| format!(" seq=\"{seq}\""));
    format!(
        "<agent_message from=\"{from}\" from_id=\"{from_id}\"{role_attr} kind=\"{kind}\"{seq_attr} ts=\"{ts}\">\n{content}\n</agent_message>",
        from = escape_xml(&msg.from),
        from_id = msg.sender_id,
        kind = msg.kind.as_str(),
        ts = msg.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
        content = escape_xml(&msg.content),
    )
}

/// Receiver half of the inbound channel.
///
/// Owned by the agent loop. The loop calls [`InboundChannel::drain`] at each
/// tool boundary to pull all currently-buffered messages without awaiting.
/// A lingering loop ([`LingerPolicy`](crate::agent_loop::linger::LingerPolicy))
/// additionally awaits [`InboundChannel::steer_ready`] so a
/// [`MessageKind::Steer`] wakes it immediately while
/// [`MessageKind::Update`]s keep buffering (DECISION M2).
pub struct InboundChannel {
    rx: mpsc::Receiver<ChannelMessage>,
    admission: Arc<Mutex<InboundAdmission>>,
    /// Messages pulled off the mpsc by [`Self::steer_ready`] while
    /// waiting for a steer. [`Self::drain`] returns these first, so the
    /// readiness await never consumes a message — `drain` stays the one
    /// consumer and send order is preserved.
    peeked: Vec<ChannelMessage>,
}

impl InboundChannel {
    /// Close the receiver to new sends while retaining every already-buffered
    /// message for a final drain. Terminal controller teardown uses this as
    /// the direct-sender half of mailbox closure.
    pub(crate) fn close(&mut self) {
        let mut admission = self.admission.lock();
        if !admission.closed {
            admission.closed = true;
            self.rx.close();
        }
    }

    /// Drain all currently-buffered messages without awaiting.
    ///
    /// Returns the messages in the order they were sent — anything
    /// buffered by an earlier [`Self::steer_ready`] await first (it
    /// arrived first), then whatever sits in the channel. If the sender
    /// half has been dropped, returns whatever was buffered before the
    /// disconnect.
    pub fn drain(&mut self) -> Vec<ChannelMessage> {
        let mut drained = std::mem::take(&mut self.peeked);
        while let Ok(msg) = self.rx.try_recv() {
            drained.push(msg);
        }
        drained
    }

    /// Drain buffered messages only when at least one [`MessageKind::Steer`]
    /// is ready.
    ///
    /// This is the non-blocking idle-wake primitive for event loops that
    /// cannot hold a mutable borrow across `select!`. Updates alone are put
    /// back into the peek buffer and return `None`, preserving the invariant
    /// that [`MessageKind::Update`] does not wake an idle or lingering agent.
    /// When a steer is present, the full drained batch is returned in arrival
    /// order so preceding updates ride along with the wake exactly as they do
    /// in [`Self::steer_ready`].
    pub fn drain_if_steer_ready(&mut self) -> Option<Vec<ChannelMessage>> {
        let drained = self.drain();
        if drained.is_empty() {
            return None;
        }
        if drained.iter().any(|msg| msg.kind == MessageKind::Steer) {
            return Some(drained);
        }
        self.peeked = drained;
        None
    }

    /// Receive the next buffered message, awaiting arrival when the
    /// buffer is empty.
    ///
    /// Messages come back in the order [`Self::drain`] would return
    /// them: anything buffered by an earlier [`Self::steer_ready`] await
    /// first, then channel arrivals. Returns `None` once every sender
    /// has been dropped and the buffer is exhausted.
    ///
    /// This is the idle-park primitive for controller tasks that hold a
    /// parked agent's channel (the spawn launch wrapper's Idle `select!`):
    /// it wakes on *any* arrival — steer and update alike — so a message
    /// pushed through a parent-held [`InboundSender`] while the agent is
    /// parked can be routed into the agent's durable pending store
    /// instead of sitting undrainable in the buffer. [`Self::steer_ready`]
    /// is deliberately unsuitable there: its update-suppressing wake
    /// (DECISION M2) protects a *running or lingering* loop from FYI
    /// interruptions, but a parked agent has no loop to interrupt and
    /// parking must not strand acknowledged messages.
    ///
    /// Cancel-safe: the inner `recv` is cancel-safe and a received
    /// message is returned immediately (never held across an await), so
    /// a caller dropping this future loses nothing.
    pub async fn recv(&mut self) -> Option<ChannelMessage> {
        if !self.peeked.is_empty() {
            return Some(self.peeked.remove(0));
        }
        self.rx.recv().await
    }

    /// Await until a [`MessageKind::Steer`] message is available, moving
    /// every received message (steer and update alike) into the peek
    /// buffer for the next [`Self::drain`]. Returns `true` the moment a
    /// steer is buffered; returns `false` when every sender has been
    /// dropped and the channel is exhausted (callers disable their wake
    /// arm — awaiting again would resolve instantly forever).
    ///
    /// [`MessageKind::Update`]s deliberately do not resolve the await
    /// (DECISION M2): they buffer and ride out with the next drain.
    ///
    /// Cancel-safe: the inner `recv` is cancel-safe and every received
    /// message is moved into the peek buffer before the next await
    /// point, so a caller dropping this future (e.g. a `select!` taken
    /// by another arm) loses nothing.
    pub async fn steer_ready(&mut self) -> bool {
        // A steer already peeked by a previous, cancelled await wakes
        // immediately — it has not been drained yet.
        if self.peeked.iter().any(|m| m.kind == MessageKind::Steer) {
            return true;
        }
        loop {
            match self.rx.recv().await {
                Some(msg) => {
                    let is_steer = msg.kind == MessageKind::Steer;
                    self.peeked.push(msg);
                    if is_steer {
                        return true;
                    }
                }
                None => return false,
            }
        }
    }
}

impl Drop for InboundChannel {
    fn drop(&mut self) {
        self.close();
    }
}

/// Sender half of the inbound channel.
///
/// Cheaply cloneable so multiple producers can feed a single agent.
#[derive(Clone)]
pub struct InboundSender {
    tx: mpsc::Sender<ChannelMessage>,
    admission: Arc<Mutex<InboundAdmission>>,
}

/// Capacity reserved on an [`InboundSender`].
///
/// Unlike Tokio's raw permit, this capability is revoked when the matching
/// [`InboundChannel`] closes. Reservation alone is therefore not acceptance:
/// callers must handle the result of [`Self::send`]. This keeps a permit held
/// across terminal teardown from publishing after the controller's final
/// durable drain.
#[must_use = "a reserved inbound permit must be sent or dropped"]
pub struct InboundPermit<'a> {
    permit: mpsc::Permit<'a, ChannelMessage>,
    admission: Arc<Mutex<InboundAdmission>>,
}

/// A reserved inbound message whose publication lost the terminal-close race.
pub struct InboundPermitSendError(Box<ChannelMessage>);

impl fmt::Debug for InboundPermitSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundPermitSendError")
            .field("message", &"[REDACTED]")
            .finish()
    }
}

impl InboundPermitSendError {
    /// Recover the message that was not published.
    #[must_use]
    pub fn into_inner(self) -> ChannelMessage {
        *self.0
    }
}

impl fmt::Display for InboundPermitSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("inbound channel closed before the reserved message was published")
    }
}

impl std::error::Error for InboundPermitSendError {}

impl InboundPermit<'_> {
    /// Publish the reserved message if the receiver is still accepting work.
    ///
    /// # Errors
    ///
    /// Returns the message in [`InboundPermitSendError`] when terminal closure
    /// or receiver drop revoked the reservation before publication.
    pub fn send(self, msg: ChannelMessage) -> Result<(), InboundPermitSendError> {
        let Self { permit, admission } = self;
        let admission = admission.lock();
        if admission.closed {
            return Err(InboundPermitSendError(Box::new(msg)));
        }
        permit.send(msg);
        Ok(())
    }
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
        let Ok(permit) = self.reserve().await else {
            return Err(mpsc::error::SendError(msg));
        };
        permit
            .send(msg)
            .map_err(|error| mpsc::error::SendError(error.into_inner()))
    }

    /// Reserve capacity for one message, awaiting if the bounded buffer is
    /// full. Terminal closure can revoke an acquired permit, so successful
    /// reservation is not itself acceptance; [`InboundPermit::send`] reports
    /// whether publication won the close race.
    ///
    /// # Errors
    ///
    /// Returns [`mpsc::error::SendError`] if the receiving
    /// [`InboundChannel`] has been dropped.
    pub async fn reserve(&self) -> Result<InboundPermit<'_>, mpsc::error::SendError<()>> {
        let permit = self.tx.reserve().await?;
        Ok(InboundPermit {
            permit,
            admission: Arc::clone(&self.admission),
        })
    }

    /// Whether `self` and `other` are clones of the same underlying
    /// channel. The [`MessageRouter`](crate::agent::message_router::MessageRouter)
    /// uses this to detect a route re-registered with a *different*
    /// channel while it awaited capacity on the old one.
    #[must_use]
    pub fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }

    /// Non-blocking send for callers that cannot await. The message is
    /// dropped on failure (the error variant says why; callers that need
    /// the bytes back keep their own copy).
    ///
    /// # Errors
    ///
    /// Returns [`InboundTrySendError::Full`] when the bounded buffer is
    /// at capacity and [`InboundTrySendError::Closed`] when the
    /// receiving [`InboundChannel`] has been dropped.
    pub fn try_send(&self, msg: ChannelMessage) -> Result<(), InboundTrySendError> {
        let admission = self.admission.lock();
        if admission.closed {
            return Err(InboundTrySendError::Closed);
        }
        self.tx.try_send(msg).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => InboundTrySendError::Full,
            mpsc::error::TrySendError::Closed(_) => InboundTrySendError::Closed,
        })
    }
}

/// Why a non-blocking [`InboundSender::try_send`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InboundTrySendError {
    /// The bounded buffer is at capacity.
    #[error("inbound channel full")]
    Full,
    /// The receiving [`InboundChannel`] has been dropped.
    #[error("inbound channel closed")]
    Closed,
}

/// Construct a new inbound channel pair.
///
/// `buffer` is the bounded mpsc capacity. Producers awaiting [`InboundSender::send`]
/// will block when the buffer is full.
#[must_use]
pub fn inbound_channel(buffer: usize) -> (InboundSender, InboundChannel) {
    let (tx, rx) = mpsc::channel(buffer);
    let admission = Arc::new(Mutex::new(InboundAdmission::default()));
    (
        InboundSender {
            tx,
            admission: Arc::clone(&admission),
        },
        InboundChannel {
            rx,
            admission,
            peeked: Vec::new(),
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "inbound_tests.rs"]
mod tests;
