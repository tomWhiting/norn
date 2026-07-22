//! Spawn-controller state transitions and completion delivery.
//!
//! The controller loop lives in [`super::spawn_controller`]; this module
//! keeps the small transitions it invokes explicit so route publication,
//! registry state, result delivery, and stranded-message persistence do not
//! become interleaved ad hoc inside that loop.

use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use super::reclaim::{ReclaimHandshake, reclaim_delivered_child};
use super::spawn_outcome::ChildOutcomeSummary;
#[cfg(test)]
use crate::agent::PendingAgentMessages;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
#[cfg(test)]
use crate::r#loop::inbound::InboundChannel;
use crate::r#loop::inbound::InboundSender;
#[cfg(test)]
use crate::r#loop::{UndeliveredWindow, requeue_undelivered_inbound};
#[cfg(test)]
use crate::session::store::EventStore;

pub(super) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_owned()
}

/// Sweep every message still buffered in the child's inbound channel into
/// the child's durable pending store.
#[cfg(test)]
pub(crate) fn requeue_stranded_inbound(
    store: &EventStore,
    child_id: Uuid,
    pending: Option<&PendingAgentMessages>,
    inbound_rx: &mut InboundChannel,
    window: UndeliveredWindow,
) {
    let mut stranded = inbound_rx.drain();
    if stranded.is_empty() {
        return;
    }
    if let Err(error) =
        requeue_undelivered_inbound(store, Some(child_id), pending, &mut stranded, window)
    {
        tracing::error!(
            child_id = %child_id,
            %error,
            "failed to persist queued audit event(s) while sweeping the \
             child's stranded inbound channel; affected messages will not \
             survive a restart",
        );
    }
}

pub(super) fn mark_idle(registry: &RwLock<AgentRegistry>, child_id: Uuid) {
    let mut registry = registry.write();
    if let Err(error) = registry.mark_idle(child_id) {
        super::reclaim::log_terminal_transition_violation(
            &registry,
            child_id,
            "spawn_agent",
            &error,
        );
    }
}

/// Restore the live route before any observer can see the child as active.
pub(super) fn activate_route(
    router: &MessageRouter,
    inbound_tx: &InboundSender,
    registry: &RwLock<AgentRegistry>,
    status_tx: &watch::Sender<AgentStatus>,
    child_id: Uuid,
) {
    router.register(child_id, inbound_tx.clone());
    let mut registry = registry.write();
    if let Err(error) = registry.mark_active(child_id) {
        super::reclaim::log_terminal_transition_violation(
            &registry,
            child_id,
            "spawn_agent",
            &error,
        );
    }
    let _ = status_tx.send_replace(AgentStatus::Active);
}

pub(super) fn mark_closed(registry: &RwLock<AgentRegistry>, child_id: Uuid) {
    let mut registry = registry.write();
    if let Err(error) = registry.mark_closed(child_id) {
        super::reclaim::log_terminal_transition_violation(
            &registry,
            child_id,
            "spawn_agent",
            &error,
        );
    }
}

pub(super) async fn deliver_step_result(
    result_sender: Option<&ChildResultSender>,
    child_id: Uuid,
    agent_role: &str,
    summary: &ChildOutcomeSummary,
) {
    let succeeded = summary.status == AgentStatus::Completed;
    let subtree_usage = summary.usage.clone() + summary.children_usage.clone();
    if let Some(sender) = result_sender {
        let formatted_message = if succeeded {
            crate::agent::fork::format_spawn_result(
                child_id,
                agent_role,
                summary.output_text.as_deref().unwrap_or("(no output)"),
            )
        } else {
            crate::agent::fork::format_spawn_failure(
                child_id,
                agent_role,
                summary.error.as_deref().unwrap_or("unknown error"),
            )
        };
        let result = ChildAgentResult {
            agent_id: child_id,
            agent_role: agent_role.to_owned(),
            succeeded,
            formatted_message,
            error: summary.error.clone(),
            stop: summary.stop.clone(),
            usage: summary.usage.clone(),
            subtree_usage,
        };
        if let Err(error) = sender.0.send(result).await {
            tracing::error!(
                child_id = %child_id,
                %error,
                "spawn_agent: failed to send result through child result channel",
            );
        }
    } else {
        tracing::error!(
            child_id = %child_id,
            "spawn_agent: no child-result channel on the spawning context; \
             the child's result cannot be delivered",
        );
    }
}

pub(super) async fn reclaim_after_result_delivery(
    reclaim: &mut Option<ReclaimHandshake>,
    registry: &RwLock<AgentRegistry>,
    child_id: Uuid,
) {
    if let Some(handshake) = reclaim.take() {
        if handshake.handle_installed.await.is_err() {
            tracing::warn!(
                child_id = %child_id,
                "spawn_agent: handle-installed ack dropped before launch completed; \
                 reclaiming without a stored handle",
            );
        }
        reclaim_delivered_child(registry, &handshake.handles, child_id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::*;
    use crate::agent::{AGENT_MESSAGE_QUEUED_EVENT_TYPE, PendingMailboxLease};
    use crate::r#loop::inbound::{ChannelMessage, MessageKind, inbound_channel};
    use crate::session::SessionBinding;
    use crate::session::events::SessionEvent;

    fn message(to_id: Uuid, content: &str, kind: MessageKind) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "/root/parent".to_owned(),
            role: None,
            to_id,
            content: content.to_owned(),
            kind,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn requeue_stranded_inbound_queues_all_kinds_with_audits() {
        let child_id = Uuid::new_v4();
        let store = Arc::new(EventStore::new());
        let pending = PendingAgentMessages::new();
        let mailbox_lease = Arc::new(PendingMailboxLease::new());
        assert!(
            pending
                .register_child_mailbox(
                    child_id,
                    SessionBinding::ephemeral_root().mailbox_id(),
                    &store,
                    &mailbox_lease,
                )
                .is_ok(),
            "test mailbox registration should succeed",
        );
        let (tx, mut rx) = inbound_channel(8);
        assert!(
            tx.send(message(child_id, "steer me", MessageKind::Steer))
                .await
                .is_ok(),
            "steer should fit the open test channel",
        );
        assert!(
            tx.send(message(child_id, "fyi", MessageKind::Update))
                .await
                .is_ok(),
            "update should fit the open test channel",
        );

        requeue_stranded_inbound(
            &store,
            child_id,
            Some(&pending),
            &mut rx,
            UndeliveredWindow::Deregistration,
        );

        assert_eq!(pending.pending_for(child_id), 2);
        let drained = pending.messages_for_delivery(child_id);
        assert_eq!(
            drained
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["steer me", "fyi"],
            "FIFO order must survive the sweep",
        );
        let audits = store
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SessionEvent::Custom { event_type, .. }
                        if event_type == AGENT_MESSAGE_QUEUED_EVENT_TYPE
                )
            })
            .count();
        assert_eq!(audits, 2, "one queued audit per message");
        assert!(rx.drain().is_empty(), "the channel is left empty");
    }

    #[tokio::test]
    async fn requeue_stranded_inbound_is_a_no_op_on_an_empty_channel() {
        let child_id = Uuid::new_v4();
        let store = EventStore::new();
        let pending = PendingAgentMessages::new();
        let (_tx, mut rx) = inbound_channel(4);

        requeue_stranded_inbound(
            &store,
            child_id,
            Some(&pending),
            &mut rx,
            UndeliveredWindow::Deregistration,
        );

        assert!(pending.is_empty());
        assert!(store.is_empty());
    }
}
