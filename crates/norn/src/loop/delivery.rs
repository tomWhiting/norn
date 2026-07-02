//! Delivery of inter-agent traffic into a running conversation: inbound
//! message draining/partitioning/injection and child-result injection.
//!
//! These are the single injection paths for everything that reaches an
//! agent from outside its own loop. Both the runner's mid-run drains and
//! the linger-await's stop-boundary sweeps ([`super::linger`]) call
//! exactly these functions, so a message or child result is formatted,
//! persisted, and audited identically no matter when it arrives.

use std::fmt::Write as _;

use crate::agent::result_channel::frame_child_result;
use crate::agent::{PendingAgentMessage, PendingAgentMessages, append_pending_message_audit};
use crate::error::SessionError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::active_input::{ActiveInput, ActiveInputReceiver};
use crate::r#loop::inbound::{ChannelMessage, InboundChannel, MessageKind, frame_message};
use crate::provider::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AgentEventSender, AgentMessageLifecycle,
};
use crate::provider::request::{Message, MessageRole};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::helpers::append_and_notify;
use super::loop_context::LoopContext;

/// Drain the inbound channel (if present) and partition messages by
/// [`MessageKind`]. Returns `(steer, update)` — steers inject at the next
/// boundary, updates batch until the model would otherwise stop.
pub(super) fn drain_and_partition(
    inbound: Option<&mut InboundChannel>,
) -> (Vec<ChannelMessage>, Vec<ChannelMessage>) {
    let Some(ch) = inbound else {
        return (Vec::new(), Vec::new());
    };
    let drained = ch.drain();
    let mut steer = Vec::new();
    let mut update = Vec::new();
    for msg in drained {
        match msg.kind {
            MessageKind::Steer => steer.push(msg),
            MessageKind::Update => update.push(msg),
        }
    }
    (steer, update)
}

/// Inject inbound messages as harness-framed user-role messages into both
/// the event store and the local conversation.
///
/// The injected turn is the `<agent_message ...>` frame built by
/// [`frame_message`] — attribution attributes are harness-resolved and the
/// sender content is escaped, so a message body can never forge a frame or
/// impersonate another sender. The persisted `UserMessage` stores the
/// framed string, so the audit record is byte-identical to what the model
/// saw and resume replays it verbatim.
///
/// Router-sequenced messages sort by their per-recipient `seq` (the
/// authoritative order — timestamps from concurrent senders are not
/// monotonic); unsequenced direct sends follow, ordered by timestamp. Each
/// router-sequenced message additionally appends an
/// [`AgentMessageLifecycle::Delivered`] audit event immediately **after**
/// its framed `UserMessage` (adjacent events, same parent chain) so the
/// store records the delivery half of the `agent_message.*` trail, and —
/// when the step has a live event channel — broadcasts the same `Delivered`
/// via [`AgentEventSender::send_message`], mirroring the dual-carrier `Sent`
/// emission at the send site. Direct sends that bypassed the router (no
/// `seq`) have no `Sent` record to pair with and emit no `Delivered`. The
/// audit follows (not precedes) the content append so a failure between the
/// two can never leave a durable record claiming delivery of a message
/// whose content never landed.
///
/// # Partial-failure durability
///
/// `msgs` is drained in place: each message is removed only after its
/// framed `UserMessage` has durably appended. If an append fails
/// mid-batch, this returns the error with the failing message **and every
/// message after it still present in `msgs`**, so the caller can preserve
/// that remainder (channel-drained steers and wake-seed messages have no
/// other durable copy at this point) for the step-exit re-queue sweep.
/// The successfully-injected prefix is consumed and never redelivered.
pub(super) async fn inject_inbound_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    msgs: &mut Vec<ChannelMessage>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<EventId>, SessionError> {
    if msgs.is_empty() {
        return Ok(Vec::new());
    }
    msgs.sort_by_key(|m| (m.seq.is_none(), m.seq.unwrap_or(0), m.timestamp));
    let mut user_event_ids = Vec::with_capacity(msgs.len());
    while let Some(msg) = msgs.first() {
        let formatted = frame_message(msg);
        // Content first: only once the framed UserMessage is durable does
        // this message count as delivered. On failure the message stays at
        // the front of `msgs` (with the rest of the batch) for the caller
        // to re-queue — nothing acknowledged is dropped.
        let user_event_id = append_and_notify(
            store,
            SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: formatted.clone(),
            },
            hooks,
        )
        .await?;

        // The content is durable; from here the message is delivered and
        // must be consumed even if the secondary audit append fails.
        let seq = msg.seq;
        let delivered = seq.map(|seq| AgentMessageLifecycle::Delivered {
            message_id: msg.id,
            from_id: msg.sender_id,
            to_id: msg.to_id,
            seq,
            delivered_at: chrono::Utc::now(),
        });
        let msg = msgs.remove(0);

        if let Some(delivered) = delivered {
            match serde_json::to_value(&delivered) {
                Ok(data) => {
                    // Best-effort: the message is already delivered, so a
                    // failed audit append is an observability gap, not a
                    // lost message — log it rather than re-queueing (which
                    // would duplicate the delivered content) or dropping it
                    // silently.
                    if let Err(error) = append_and_notify(
                        store,
                        SessionEvent::Custom {
                            base: EventBase::new(store.last_event_id()),
                            event_type: AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_string(),
                            data,
                        },
                        hooks,
                    )
                    .await
                    {
                        tracing::error!(
                            message_id = %msg.id,
                            %error,
                            "failed to persist agent_message.delivered audit event \
                             after its message was already delivered",
                        );
                    }
                }
                Err(error) => {
                    // Unreachable for this plain struct, but a lost audit
                    // record must never be silent.
                    tracing::error!(
                        message_id = %msg.id,
                        %error,
                        "failed to serialize agent_message.delivered audit event",
                    );
                }
            }
            if let Some(tx) = event_tx {
                tx.send_message(delivered);
            }
        }

        user_event_ids.push(user_event_id);
        messages.push(Message {
            role: MessageRole::User,
            content: Some(formatted),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
    }
    Ok(user_event_ids)
}

/// Drain durable queued messages for the current loop agent.
///
/// This is the resume/wake handoff for `signal_agent` sends accepted while a
/// recipient had no live router route. The messages still enter the
/// conversation through [`inject_inbound_messages`], so framing, hooks, and
/// session persistence stay identical to live-routed inbound delivery. The
/// pending store emits `agent_message.dequeued` only after the framed
/// `UserMessage` delivery has persisted; if persistence fails before that
/// point, the pending messages remain replayable on the next resume.
pub(super) async fn flush_pending_agent_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    loop_context: &LoopContext,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<EventId>, SessionError> {
    let (Some(agent_id), Some(pending)) = (
        loop_context.agent_id,
        loop_context.pending_agent_messages.as_ref(),
    ) else {
        return Ok(Vec::new());
    };
    let mut queued_messages = pending.messages_for_delivery(agent_id);
    if queued_messages.is_empty() {
        return Ok(Vec::new());
    }
    let dequeued_events = PendingAgentMessages::dequeued_events_for(agent_id, &queued_messages);
    let message_ids = queued_messages
        .iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    // The durable pending store is the authoritative copy here: on a
    // mid-batch append failure the error propagates before `mark_dequeued`,
    // so every queued message — injected or not — stays pending and is
    // replayed on the next resume/wake rather than lost.
    let injected = inject_inbound_messages(
        store,
        messages,
        &mut queued_messages,
        loop_context.hooks.as_deref(),
        event_tx,
    )
    .await?;
    for event in &dequeued_events {
        append_pending_message_audit(store, event)?;
    }
    pending.mark_dequeued(message_ids);
    Ok(injected)
}

/// Drain the inbound channel immediately after a completed tool batch:
/// steer messages inject now (the inbound contract's "immediately after
/// the current tool batch, before the next provider request"), updates
/// buffer until a would-stop boundary. Returns `true` when at least one
/// steer was injected.
///
/// Callers must invoke this only once every tool call in the batch has
/// received its result (including schema-feedback and rejected
/// post-schema results): providers require tool results to directly
/// follow the assistant tool-call turn, so a user-role injection between
/// them would produce a malformed conversation.
pub(super) async fn drain_post_batch_inbound(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<bool, SessionError> {
    let (mut steer, follow_up) = drain_and_partition(inbound);
    follow_up_buffer.extend(follow_up);
    if steer.is_empty() {
        return Ok(false);
    }
    if let Err(error) = inject_inbound_messages(store, messages, &mut steer, hooks, event_tx).await
    {
        // Preserve the failing message and every steer after it (all
        // acknowledged to their senders, none with another durable copy)
        // for the step-exit re-queue sweep.
        follow_up_buffer.extend(steer);
        return Err(error);
    }
    Ok(true)
}

/// Drain the inbound channel, then inject steer messages and any buffered
/// updates into the conversation. Returns `true` if any messages were
/// injected (indicating the loop should continue rather than return).
pub(super) async fn flush_inbound_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<EventId>, SessionError> {
    let (mut steer, update) = drain_and_partition(inbound);
    follow_up_buffer.extend(update);

    if steer.is_empty() && follow_up_buffer.is_empty() {
        return Ok(Vec::new());
    }

    let mut user_event_ids =
        match inject_inbound_messages(store, messages, &mut steer, hooks, event_tx).await {
            Ok(ids) => ids,
            Err(error) => {
                // Steers not yet injected re-join the buffer (which already
                // holds the drained Updates) for the step-exit re-queue.
                follow_up_buffer.extend(steer);
                return Err(error);
            }
        };
    // Move the buffered Updates out to inject them, but restore any that
    // fail to append so the boundary flush never vaporizes the backlog.
    let mut buffered = std::mem::take(follow_up_buffer);
    match inject_inbound_messages(store, messages, &mut buffered, hooks, event_tx).await {
        Ok(ids) => user_event_ids.extend(ids),
        Err(error) => {
            follow_up_buffer.extend(buffered);
            return Err(error);
        }
    }
    Ok(user_event_ids)
}

/// The delivery window in which an accepted inbound message was left
/// undelivered, recorded on the re-queue log trail so operators can tell
/// *where* a message fell out of live delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UndeliveredWindow {
    /// The step ended (any exit path) with the message still buffered or
    /// undrained in the step's inbound channel.
    StepExit,
    /// The child's controller deregistered its router route and swept the
    /// channel of messages the router had already accepted.
    Deregistration,
    /// The message arrived on a parked (Idle) child's channel, which has
    /// no live loop to drain it.
    IdlePark,
}

impl UndeliveredWindow {
    /// Stable label used in log records.
    const fn as_str(self) -> &'static str {
        match self {
            Self::StepExit => "step_exit",
            Self::Deregistration => "deregistration",
            Self::IdlePark => "idle_park",
        }
    }
}

/// Re-queue inbound messages a step accepted but never delivered, so an
/// acknowledged message is never silently dropped.
///
/// This is the single re-queue path for every window in which a live
/// loop stops being the consumer of an accepted message
/// ([`UndeliveredWindow`] names them). The step wrapper calls it on
/// every exit: Update messages buffer until a would-stop boundary, so a
/// step that ends anywhere else (max-iterations, schema-unreachable,
/// truncation, cancellation, timeout, or a hard error) leaves them in
/// the buffer, and messages of either kind still sitting undrained in
/// the step's inbound channel at exit — accepted (and acknowledged to
/// their sender) after the loop's final drain — are swept into the same
/// buffer by the wrapper. The spawn controller calls it for its
/// post-deregistration channel sweeps and for messages received while
/// the child is parked Idle. Whatever arrives here goes into the
/// durable pending store keyed to `agent_id` — the same store the next
/// step's [`flush_pending_agent_messages`] drains and `wake_agent`
/// eligibility reads — with an `agent_message.queued` audit event per
/// message. Without an agent identity or pending store (loops assembled
/// outside agent coordination) the loss is logged per message at error
/// level rather than passing silently.
pub(crate) fn requeue_undelivered_inbound(
    store: &EventStore,
    agent_id: Option<uuid::Uuid>,
    pending: Option<&PendingAgentMessages>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
    window: UndeliveredWindow,
) {
    if follow_up_buffer.is_empty() {
        return;
    }
    let undelivered = std::mem::take(follow_up_buffer);
    let (Some(agent_id), Some(pending)) = (agent_id, pending) else {
        for msg in &undelivered {
            tracing::error!(
                message_id = %msg.id,
                sender = %msg.from,
                kind = msg.kind.as_str(),
                window = window.as_str(),
                "undelivered inbound message with no durable pending store \
                 on the loop context; the acknowledged message is lost",
            );
        }
        return;
    };
    for mut msg in undelivered {
        // Redelivery targets this loop's agent regardless of how the
        // original send addressed the channel (router sends stamp the
        // recipient; direct handle sends may not).
        msg.to_id = agent_id;
        let message_id = msg.id;
        let kind = msg.kind;
        let pending_record =
            PendingAgentMessage::new(msg, agent_id.to_string(), chrono::Utc::now());
        let Some(queued_event) = pending.queue(pending_record) else {
            // Already pending under the same id (a redelivered copy the
            // step drained but did not consume) — the durable record is
            // intact, nothing to add.
            tracing::debug!(
                message_id = %message_id,
                "undelivered inbound message already pending; skipping duplicate re-queue",
            );
            continue;
        };
        tracing::warn!(
            message_id = %message_id,
            recipient = %agent_id,
            kind = kind.as_str(),
            window = window.as_str(),
            "accepted inbound message left live delivery; \
             re-queued into the durable pending store",
        );
        if let Err(error) = append_pending_message_audit(store, &queued_event) {
            tracing::error!(
                message_id = %message_id,
                %error,
                "failed to persist the queued audit event for a re-queued \
                 inbound message; the in-memory pending record is still held",
            );
        }
    }
}

/// Drain human active-turn input and persist it as ordinary user messages.
///
/// Active input is trusted operator text, not inter-agent traffic, so it does
/// not use `<agent_message>` framing. A delivery acknowledgement is emitted only
/// after the corresponding `UserMessage` event has been appended and the local
/// conversation has been updated.
pub(super) async fn flush_active_inputs(
    store: &EventStore,
    messages: &mut Vec<Message>,
    active_input: Option<&mut ActiveInputReceiver>,
    hooks: Option<&HookRegistry>,
) -> Result<Vec<EventId>, SessionError> {
    let Some(active_input) = active_input else {
        return Ok(Vec::new());
    };
    inject_active_inputs(store, messages, active_input.drain(), hooks).await
}

async fn inject_active_inputs(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inputs: Vec<ActiveInput>,
    hooks: Option<&HookRegistry>,
) -> Result<Vec<EventId>, SessionError> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let mut event_ids = Vec::with_capacity(inputs.len());
    for input in inputs {
        let content = input.content().to_string();
        let event_id = append_and_notify(
            store,
            SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: content.clone(),
            },
            hooks,
        )
        .await?;
        messages.push(Message {
            role: MessageRole::User,
            content: Some(content),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
        input.mark_delivered();
        event_ids.push(event_id);
    }
    Ok(event_ids)
}

/// Drain pending child-agent results and inject them into the running
/// conversation. Returns `true` if any results were injected.
///
/// `seed` carries a result that was already received outside this call —
/// the linger-await ([`super::linger`]) consumes one result when it wakes
/// and hands it here so every delivery, mid-run or lingering, goes through
/// this single injection path. `rx: None` with a seed injects the seed
/// alone; `rx: None` without one is a no-op.
///
/// Each result renders through
/// [`frame_child_result`](crate::agent::result_channel::frame_child_result)
/// — the same harness-built, content-escaped framing contract as inbound
/// messages, so a child's output cannot forge an `<agent_message>` or
/// `<agent_result>` frame in the parent's conversation. Each drained batch
/// is persisted as one `UserMessage` event and pushed as one user-role
/// message — keeping the persisted event stream and the live conversation
/// in 1:1 correspondence (a requirement of in-flight compaction mapping).
///
/// W3.6 usage rollup: every drained result's
/// [`subtree_usage`](crate::agent::result_channel::ChildAgentResult::subtree_usage)
/// — seed included — is folded into `children_usage`
/// ([`LoopContext::children_usage`](crate::agent_loop::loop_context::LoopContext)).
/// Because this function is the single consumer of the bounded result
/// channel **while the receiver is installed on the loop**, and every
/// result it consumes passes through it exactly once, each child
/// subtree is folded exactly once — no double-counting is structurally
/// possible. (A driver may take the receiver out of the `LoopContext`
/// and consume results externally between steps — the TUI does — in
/// which case those results are injected by the driver and deliberately
/// never folded: nothing reads a root's own rollup.)
pub(super) async fn drain_child_results(
    store: &EventStore,
    messages: &mut Vec<Message>,
    rx: Option<&mut tokio::sync::mpsc::Receiver<crate::agent::result_channel::ChildAgentResult>>,
    hooks: Option<&HookRegistry>,
    seed: Option<crate::agent::result_channel::ChildAgentResult>,
    children_usage: &crate::r#loop::children_usage::ChildrenUsage,
) -> Result<bool, SessionError> {
    let mut batch: Vec<crate::agent::result_channel::ChildAgentResult> = seed.into_iter().collect();
    if let Some(rx) = rx {
        while let Ok(r) = rx.try_recv() {
            batch.push(r);
        }
    }
    if batch.is_empty() {
        return Ok(false);
    }
    for result in &batch {
        children_usage.add(&result.subtree_usage);
    }

    let formatted = if batch.len() == 1 {
        frame_child_result(&batch[0])
    } else {
        let mut out = format!("Results from {} completed agents:\n\n", batch.len());
        for r in &batch {
            let _ = write!(out, "{}\n\n", frame_child_result(r));
        }
        out
    };

    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: formatted.clone(),
        },
        hooks,
    )
    .await?;
    messages.push(Message {
        role: MessageRole::User,
        content: Some(formatted),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(true)
}
