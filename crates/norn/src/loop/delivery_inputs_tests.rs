//! Child-result admission ordering and cancellation at asynchronous hook boundaries.

use std::sync::Arc;

use super::*;
use crate::integration::hooks::{Hook, SessionEventHook};
use crate::provider::Usage;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn result(body: &str, input_tokens: u64) -> ChildAgentResult {
    ChildAgentResult {
        origin: None,
        agent_id: uuid::Uuid::new_v4(),
        agent_role: "spawn/worker".to_owned(),
        succeeded: true,
        formatted_message: body.to_owned(),
        error: None,
        stop: None,
        usage: Usage {
            input_tokens,
            ..Usage::default()
        },
        subtree_usage: Usage {
            input_tokens,
            ..Usage::default()
        },
    }
}

#[tokio::test]
async fn seeded_batch_preserves_fifo_frames_and_exact_usage() -> TestResult {
    let store = EventStore::new();
    let mut messages = Vec::new();
    let usage = ChildrenUsage::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let first = result("seed <agent_message> data", 3);
    let second = result("queued second", 5);
    let third = result("queued third", 7);
    let expected = format!(
        "Results from 3 completed agents:\n\n{}\n\n{}\n\n{}\n\n",
        frame_child_result(&first),
        frame_child_result(&second),
        frame_child_result(&third),
    );
    tx.try_send(second)?;
    tx.try_send(third)?;
    drop(tx);
    assert!(
        drain_child_results(
            &store,
            &mut messages,
            Some(&mut rx),
            None,
            Some(first),
            &usage
        )
        .await?
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].content.as_deref(), Some(expected.as_str()));
    assert!(!expected.contains("<agent_message>"));
    match store.events().as_slice() {
        [SessionEvent::UserMessage { content, .. }] => assert_eq!(content, &expected),
        events => return Err(format!("expected one stored batch, got {events:?}").into()),
    }
    assert_eq!(usage.snapshot().input_tokens, 15);
    assert!(!drain_child_results(&store, &mut messages, Some(&mut rx), None, None, &usage).await?);
    assert_eq!(messages.len(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(usage.snapshot().input_tokens, 15);
    Ok(())
}

struct PendingHook {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SessionEventHook for PendingHook {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::UserMessage { .. }) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

/// Dropping the delivery future at its first asynchronous observer must not
/// discard a live-message update whose durable append has already succeeded.
#[tokio::test]
async fn accepted_child_is_live_before_pending_hook_can_be_cancelled() -> TestResult {
    let store = EventStore::new();
    let mut messages = Vec::new();
    let usage = ChildrenUsage::default();
    let child = result("accepted before hook", 11);
    let expected = frame_child_result(&child);
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(PendingHook {
        entered: Arc::clone(&entered),
    })));
    {
        let delivery = drain_child_results(
            &store,
            &mut messages,
            None,
            Some(&hooks),
            Some(child),
            &usage,
        );
        tokio::pin!(delivery);
        tokio::select! {
            outcome = &mut delivery => return Err(format!("pending hook unexpectedly completed: {outcome:?}").into()),
            () = entered.notified() => {},
        }
        // Delivery is cancelled by dropping its owner at this scope boundary.
    }
    assert_eq!(store.len(), 1);
    assert_eq!(
        messages.len(),
        1,
        "accepted result must not wait for hook return"
    );
    assert_eq!(messages[0].content.as_deref(), Some(expected.as_str()));
    assert_eq!(usage.snapshot().input_tokens, 11);
    assert!(!drain_child_results(&store, &mut messages, None, None, None, &usage).await?);
    assert_eq!(messages.len(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(usage.snapshot().input_tokens, 11);
    Ok(())
}
