//! Resume regression from the canonical pending mailbox through live projection and replay.

use std::sync::Arc;

use crate::agent::{PendingAgentMessage, PendingAgentMessages, PendingMailboxLease};
use crate::r#loop::inbound::{ChannelMessage, MessageKind, frame_message};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::{AgentEventSender, AgentMessageLifecycle};
use crate::session::SessionBinding;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::session_view::{SessionIdentity, SessionProjection, ViewSource};

#[tokio::test]
async fn resumed_delivery_preserves_queue_provenance_and_uses_current_view_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let historical = uuid::Uuid::new_v4();
    let resumed = uuid::Uuid::new_v4();
    let mailbox = SessionBinding::ephemeral_root().mailbox_id();
    let store = Arc::new(EventStore::new());
    let original = PendingAgentMessages::new();
    let original_lease = Arc::new(PendingMailboxLease::new());
    original.register_child_mailbox(historical, mailbox, &store, &original_lease)?;
    let message = ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::new_v4(),
        from: "fixture:sender".to_owned(),
        role: None,
        to_id: historical,
        content: "message queued before resume".to_owned(),
        kind: MessageKind::Update,
        seq: Some(1),
        timestamp: chrono::Utc::now(),
    };
    let mut queued =
        PendingAgentMessage::new(message.clone(), historical.to_string(), message.timestamp);
    original.persist_for_registered_store(&store, &mut queued)?;
    let before = store.events();
    let pending = Arc::new(PendingAgentMessages::from_events(
        resumed, mailbox, &before,
    )?);
    let resumed_lease = Arc::new(PendingMailboxLease::new());
    pending.register_child_mailbox(resumed, mailbox, &store, &resumed_lease)?;
    let mut context = LoopContext::new("resume fixture");
    context.agent_id = Some(resumed);
    context.pending_agent_messages = Some(Arc::clone(&pending));
    let (tx, mut rx) = tokio::sync::broadcast::channel(1);
    let sender = AgentEventSender::new(tx, resumed, "root".to_owned());
    let mut messages = Vec::new();
    let delivered =
        super::flush_pending_agent_messages(&store, &mut messages, &context, Some(&sender)).await?;
    assert_eq!(delivered.len(), 1);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content.as_deref(),
        Some(frame_message(&message).as_str())
    );
    let observation = rx.try_recv()?;
    assert_eq!(observation.agent_id, resumed);
    let mut view = SessionProjection::new(ViewSource {
        session: SessionIdentity::Persisted("resume-fixture".to_owned()),
        agent_id: resumed,
        parent_agent_id: None,
        store_generation: uuid::Uuid::new_v4(),
    });
    view.apply_live(&observation)?;
    let after = store.events();
    assert_eq!(
        serde_json::to_value(&after[..before.len()])?,
        serde_json::to_value(&before)?
    );
    let audit = after
        .iter()
        .find_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "agent_message.delivered" => Some(data),
            _ => None,
        })
        .ok_or("delivered audit missing")?;
    assert_eq!(audit["to_id"], serde_json::json!(resumed));
    assert_eq!(audit["user_event_id"], serde_json::json!(delivered[0]));
    assert!(
        matches!(serde_json::from_value::<AgentMessageLifecycle>(audit.clone())?, AgentMessageLifecycle::Delivered { to_id, .. } if to_id == resumed)
    );
    assert!(
        super::flush_pending_agent_messages(&store, &mut messages, &context, Some(&sender))
            .await?
            .is_empty()
    );
    assert!(PendingAgentMessages::from_events(uuid::Uuid::new_v4(), mailbox, &after)?.is_empty());
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(event, SessionEvent::UserMessage { .. }))
            .count(),
        1
    );
    Ok(())
}
