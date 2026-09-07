//! Exact-once handoff from the durable pending queue into the conversation.

use crate::agent::{PendingAgentMessageLifecycle, append_pending_message_audit};
use crate::error::SessionError;
use crate::provider::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AgentEventSender, AgentMessageLifecycle,
};
use crate::provider::request::{Message, MessageRole};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::delivery::BoundDelivery;
use super::helpers::append_and_notify;
use super::loop_context::LoopContext;

/// Append one stable event without blocking unrelated executor tasks.
pub(crate) fn append_idempotent_off_executor(
    store: &EventStore,
    event: SessionEvent,
) -> Result<EventId, SessionError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| store.append_idempotent(event))
        }
        _ => store.append_idempotent(event),
    }
}

/// Deliver every queued message for the current loop agent exactly once.
///
/// One ordinary `UserMessage`, keyed by the queued message UUID, is the sole
/// authoritative delivery/consumption record. The pending entry is removed
/// synchronously after that append and before hooks or event broadcasts can
/// yield to cancellation. Replaying session events therefore removes a queue
/// entry even when the process died before its secondary dequeue/delivery
/// audits were emitted.
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
    if pending.pending_for(agent_id) == 0 {
        return Ok(Vec::new());
    }
    let Some(_flush_guard) = pending.try_delivery_flush(agent_id) else {
        return Err(SessionError::EventAppendFailed {
            reason: format!(
                "pending-message delivery is already active for agent {agent_id}; \
                 concurrent steps for one agent are not permitted"
            ),
        });
    };
    let mut delivered_ids = Vec::new();
    while pending.pending_for(agent_id) > 0 {
        pending.ensure_head_durable(agent_id, store)?;
        let Some(prepared) = pending.prepare_next_delivery(agent_id, store.last_event_id()) else {
            return Err(SessionError::EventAppendFailed {
                reason: format!(
                    "pending-message FIFO for agent {agent_id} disappeared during delivery"
                ),
            });
        };
        let user_event_id = append_idempotent_off_executor(store, prepared.delivery_event.clone())?;

        // No await is permitted between the authoritative append and queue
        // consumption. Cancellation can omit only secondary observations.
        pending.commit_delivery(agent_id, prepared.message.id, &prepared.framed_content)?;
        delivered_ids.push(user_event_id.clone());
        messages.push(Message {
            response_items: Vec::new(),
            role: MessageRole::User,
            content: Some(prepared.framed_content),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        });

        let dequeued = PendingAgentMessageLifecycle::Dequeued {
            message_id: prepared.message.id,
            to_id: agent_id,
            dequeued_at: chrono::Utc::now(),
        };
        if let Err(error) = append_pending_message_audit(store, &dequeued) {
            tracing::error!(
                message_id = %prepared.message.id,
                %error,
                "pending message is durably delivered but its secondary \
                 agent_message.dequeued audit could not be persisted",
            );
        }

        if let Some(hooks) = loop_context.hooks.as_deref() {
            hooks.run_on_event(&prepared.delivery_event).await;
        }
        emit_delivered_observation(
            store,
            loop_context,
            event_tx,
            &prepared.message,
            &user_event_id,
        )
        .await;
    }
    Ok(delivered_ids)
}

async fn emit_delivered_observation(
    store: &EventStore,
    loop_context: &LoopContext,
    event_tx: Option<&AgentEventSender>,
    message: &crate::r#loop::inbound::ChannelMessage,
    user_event_id: &EventId,
) {
    let delivered = AgentMessageLifecycle::Delivered {
        message_id: message.id,
        from_id: message.sender_id,
        from: message.from.clone(),
        to_id: message.to_id,
        seq: message.seq,
        delivered_at: chrono::Utc::now(),
    };
    match serde_json::to_value(BoundDelivery {
        lifecycle: &delivered,
        user_event_id,
    }) {
        Ok(data) => {
            if let Err(error) = append_and_notify(
                store,
                SessionEvent::Custom {
                    base: EventBase::new(store.last_event_id()),
                    event_type: AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_owned(),
                    data,
                },
                loop_context.hooks.as_deref(),
            )
            .await
            {
                tracing::error!(
                    message_id = %message.id,
                    %error,
                    "pending message is durably delivered but its secondary \
                     agent_message.delivered audit could not be persisted",
                );
            }
        }
        Err(error) => {
            tracing::error!(
                message_id = %message.id,
                %error,
                "failed to serialize secondary agent_message.delivered audit",
            );
        }
    }
    if let Some(event_tx) = event_tx {
        event_tx.send_message(delivered);
    }
}

#[cfg(test)]
mod notification_binding_tests {
    use std::sync::Arc;

    use super::{EventStore, LoopContext, SessionEvent, flush_pending_agent_messages};
    use crate::agent::{PendingAgentMessage, PendingAgentMessages, PendingMailboxLease};
    use crate::r#loop::inbound::{ChannelMessage, MessageKind};
    use crate::session::SessionBinding;

    #[tokio::test]
    async fn pending_delivered_audit_binds_input_instead_of_dequeue_audit()
    -> Result<(), Box<dyn std::error::Error>> {
        let recipient = uuid::Uuid::new_v4();
        let store = Arc::new(EventStore::new());
        let pending = Arc::new(PendingAgentMessages::new());
        let lease = Arc::new(PendingMailboxLease::new());
        pending.register_child_mailbox(
            recipient,
            SessionBinding::ephemeral_root().mailbox_id(),
            &store,
            &lease,
        )?;
        let message = ChannelMessage {
            id: uuid::Uuid::new_v4(),
            sender_id: uuid::Uuid::nil(),
            from: "norn:watch".to_owned(),
            role: None,
            to_id: recipient,
            content: "pending original body".to_owned(),
            kind: MessageKind::Update,
            seq: Some(9),
            timestamp: chrono::Utc::now(),
        };
        let queued_at = message.timestamp;
        let mut queued = PendingAgentMessage::new(message, recipient.to_string(), queued_at);
        pending.persist_for_registered_store(&store, &mut queued)?;
        let mut context = LoopContext::new("fixture");
        context.agent_id = Some(recipient);
        context.pending_agent_messages = Some(Arc::clone(&pending));
        let mut prompt = Vec::new();
        let ids = flush_pending_agent_messages(&store, &mut prompt, &context, None).await?;
        let [input_id] = ids.as_slice() else {
            return Err("expected one pending delivery".into());
        };
        assert!(matches!(
            store.get(input_id),
            Some(SessionEvent::UserMessage { .. })
        ));
        let events = store.events();
        let delivered = events
            .iter()
            .find_map(|event| match event {
                SessionEvent::Custom {
                    event_type, data, ..
                } if event_type == "agent_message.delivered" => Some(data),
                _ => None,
            })
            .ok_or("delivered audit missing")?;
        assert_eq!(delivered["user_event_id"], serde_json::json!(input_id));
        let dequeued = events.iter().position(|event| matches!(event, SessionEvent::Custom { event_type, .. } if event_type == "agent_message.dequeued")).ok_or("dequeue audit missing")?;
        let input = events
            .iter()
            .position(|event| &event.base().id == input_id)
            .ok_or("input missing")?;
        assert!(dequeued > input);
        assert_eq!(pending.pending_for(recipient), 0);
        assert_eq!(prompt.len(), 1);
        assert!(
            flush_pending_agent_messages(&store, &mut prompt, &context, None)
                .await?
                .is_empty()
        );
        Ok(())
    }
}
