use std::sync::Arc;

use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};
use crate::provider::request::Message;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

use super::delivery::inject_inbound_messages;
use super::inbound::{ChannelMessage, MessageKind};

struct BlockAfterDurableUserMessage {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SessionEventHook for BlockAfterDurableUserMessage {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::UserMessage { .. }) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn cancellation_after_durable_inbound_append_cannot_requeue_the_message()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let store = EventStore::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(BlockAfterDurableUserMessage {
        entered: Arc::clone(&entered),
    })));
    let mut prompt_messages: Vec<Message> = Vec::new();
    let mut inbound = vec![ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::new_v4(),
        from: "fixture".to_owned(),
        role: None,
        to_id: uuid::Uuid::nil(),
        content: "DURABLE-INBOUND".to_owned(),
        kind: MessageKind::Steer,
        seq: Some(1),
        timestamp: chrono::Utc::now(),
    }];

    let mut injection = Box::pin(inject_inbound_messages(
        &store,
        &mut prompt_messages,
        &mut inbound,
        Some(&hooks),
        None,
    ));
    tokio::select! {
        () = entered.notified() => {}
        result = injection.as_mut() => {
            return Err(std::io::Error::other(
                format!("blocking hook unexpectedly returned: {result:?}"),
            ).into());
        }
    }
    drop(injection);

    assert!(
        inbound.is_empty(),
        "durable content must leave the source before hooks await"
    );
    assert_eq!(
        store
            .events()
            .iter()
            .filter(|event| matches!(event, SessionEvent::UserMessage { .. }))
            .count(),
        1,
    );
    Ok(())
}
