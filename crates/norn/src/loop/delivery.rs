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
use crate::agent::{PendingAgentMessages, append_pending_message_audit};
use crate::error::SessionError;
use crate::integration::hooks::HookRegistry;
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
/// [`AgentMessageLifecycle::Delivered`] audit event immediately before its
/// framed `UserMessage` (adjacent events, same parent chain) so the store
/// records the delivery half of the `agent_message.*` trail, and — when the
/// step has a live event channel — broadcasts the same `Delivered` via
/// [`AgentEventSender::send_message`], mirroring the dual-carrier `Sent`
/// emission at the send site. Direct sends that bypassed the router (no
/// `seq`) have no `Sent` record to pair with and emit no `Delivered`.
pub(super) async fn inject_inbound_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    mut msgs: Vec<ChannelMessage>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<EventId>, SessionError> {
    if msgs.is_empty() {
        return Ok(Vec::new());
    }
    msgs.sort_by_key(|m| (m.seq.is_none(), m.seq.unwrap_or(0), m.timestamp));
    let mut user_event_ids = Vec::with_capacity(msgs.len());
    for msg in msgs {
        if let Some(seq) = msg.seq {
            let delivered = AgentMessageLifecycle::Delivered {
                message_id: msg.id,
                from_id: msg.sender_id,
                to_id: msg.to_id,
                seq,
                delivered_at: chrono::Utc::now(),
            };
            match serde_json::to_value(&delivered) {
                Ok(data) => {
                    append_and_notify(
                        store,
                        SessionEvent::Custom {
                            base: EventBase::new(store.last_event_id()),
                            event_type: AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_string(),
                            data,
                        },
                        hooks,
                    )
                    .await?;
                }
                Err(e) => {
                    // Unreachable for this plain struct, but a lost audit
                    // record must never be silent.
                    tracing::error!(
                        message_id = %msg.id,
                        error = %e,
                        "failed to serialize agent_message.delivered audit event",
                    );
                }
            }
            if let Some(tx) = event_tx {
                tx.send_message(delivered);
            }
        }
        let formatted = frame_message(&msg);
        let user_event_id = append_and_notify(
            store,
            SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: formatted.clone(),
            },
            hooks,
        )
        .await?;
        user_event_ids.push(user_event_id);
        messages.push(Message {
            role: MessageRole::User,
            content: Some(formatted),
            thinking: String::new(),
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
    let queued_messages = pending.messages_for_delivery(agent_id);
    if queued_messages.is_empty() {
        return Ok(Vec::new());
    }
    let dequeued_events = PendingAgentMessages::dequeued_events_for(agent_id, &queued_messages);
    let message_ids = queued_messages
        .iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    let injected = inject_inbound_messages(
        store,
        messages,
        queued_messages,
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
    let (steer, update) = drain_and_partition(inbound);
    follow_up_buffer.extend(update);

    if steer.is_empty() && follow_up_buffer.is_empty() {
        return Ok(Vec::new());
    }

    let mut user_event_ids =
        inject_inbound_messages(store, messages, steer, hooks, event_tx).await?;
    let buffered = std::mem::take(follow_up_buffer);
    user_event_ids
        .extend(inject_inbound_messages(store, messages, buffered, hooks, event_tx).await?);
    Ok(user_event_ids)
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
/// ([`LoopContext::children_usage`](crate::r#loop::loop_context::LoopContext)).
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
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(true)
}
