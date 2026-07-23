use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::agent::{AGENT_MESSAGE_QUEUED_EVENT_TYPE, TerminalPendingRetryOutcome};
use crate::error::ToolError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::session::persistence::SessionPersistError;
use crate::session::store::PersistenceSink;
use crate::tools::agent::coord::CloseAgentTool;
use crate::tools::agent::{AgentHandle, TestChildEventStore};

const PRIVATE_PAYLOAD: &str = "private idle queue payload";
const SINK_DIAGNOSTIC: &str = "idle queue sink diagnostic";

struct SwitchableQueueSink {
    reject_queue: Arc<AtomicBool>,
}

impl PersistenceSink for SwitchableQueueSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if self.reject_queue.load(Ordering::SeqCst)
            && matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == AGENT_MESSAGE_QUEUED_EVENT_TYPE
            )
        {
            return Err(SessionPersistError::Io(std::io::Error::other(
                SINK_DIAGNOSTIC,
            )));
        }
        Ok(())
    }
}

fn queue_event_count(store: &EventStore, message_id: Uuid) -> usize {
    let expected_event_id = format!("norn:pending-agent-message:queued:{message_id}");
    let expected_message_id = message_id.to_string();
    store
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::Custom {
                    base,
                    event_type,
                    data,
                } if event_type == AGENT_MESSAGE_QUEUED_EVENT_TYPE
                    && base.id.as_str() == expected_event_id
                    && data.get("message_id").and_then(serde_json::Value::as_str)
                        == Some(expected_message_id.as_str())
                    && data.get("authoritative").and_then(serde_json::Value::as_bool)
                        == Some(true)
            )
        })
        .count()
}

fn direct_update(parent_id: Uuid, child_id: Uuid) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: parent_id,
        from: "root".to_owned(),
        role: None,
        to_id: child_id,
        content: PRIVATE_PAYLOAD.to_owned(),
        kind: MessageKind::Update,
        seq: None,
        timestamp: chrono::Utc::now(),
    }
}

/// A Q outage while a persistent child is parked must not become silent loss
/// when the wrapper is aborted before its terminal mailbox transition.
#[tokio::test]
async fn idle_queue_failure_then_wrapper_abort_retains_exact_recovery() -> TestResult {
    let reject_queue = Arc::new(AtomicBool::new(true));
    let child_store = Arc::new(EventStore::with_sink(Box::new(SwitchableQueueSink {
        reject_queue: Arc::clone(&reject_queue),
    })));
    let weak_child_store = Arc::downgrade(&child_store);
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "initial task complete".to_owned(),
        },
        done_event(),
    ]]));
    let parent_id = Uuid::new_v4();
    let registry = AgentRegistry::shared();
    let temp = tempfile::tempdir()?;
    let (ctx, _manager, _root_session_id) = persistent_parent_ctx(
        temp.path(),
        provider,
        parent_id,
        &registry,
        Arc::new(ToolRegistry::new()),
    )?;
    ctx.insert_extension(Arc::new(TestChildEventStore(Arc::clone(&child_store))));

    let child_id = spawn_and_join(
        &SpawnAgentTool::new(),
        &ctx,
        json!({"task": "park after completion", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;
    assert_eq!(
        registry
            .read()
            .get(child_id)
            .ok_or("spawned child must remain registered")?
            .status,
        AgentStatus::Idle,
    );

    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("spawn context must retain agent infra")?;
    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("spawn context must retain agent handles")?;
    let inbound = handles
        .inbound_tx(child_id)
        .ok_or("spawned child must expose its inbound sender")?;

    // Stop the test fixture from keeping the failed store alive. After the
    // wrapper and handle disappear, only an exact recovery authority may own it.
    ctx.insert_extension(Arc::new(TestChildEventStore(Arc::new(EventStore::new()))));
    drop(child_store);

    let message = direct_update(parent_id, child_id);
    let expected_message = serde_json::to_value(&message)?;
    let message_id = message.id;
    inbound.send(message).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while infra.pending_messages.pending_for(child_id) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    let store_before_close = weak_child_store
        .upgrade()
        .ok_or("live controller must still own the child store")?;
    assert_eq!(
        queue_event_count(store_before_close.as_ref(), message_id),
        0,
        "the injected Q outage must leave no canonical queue event",
    );
    drop(store_before_close);

    let handle = handles
        .remove(child_id)
        .ok_or("spawned child handle must be installed")?;
    handle.join_handle.abort();
    handles.insert(handle);

    let close = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-idle-queue-orphan".to_owned(),
                tool_name: "close_agent".to_owned(),
                model_args: json!({
                    "agent_id": child_id.to_string(),
                    "reason": "test wrapper death",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await;
    let Err(ToolError::ExecutionFailed { reason }) = close else {
        return Err("close must refuse to reclaim unresolved accepted work".into());
    };
    assert!(reason.contains(&child_id.to_string()));
    assert!(reason.contains("1 accepted message"));
    assert!(!reason.contains(PRIVATE_PAYLOAD));
    assert!(!reason.contains(SINK_DIAGNOSTIC));

    assert!(
        registry.read().get(child_id).is_some(),
        "unresolved accepted work must retain the registry entry",
    );
    assert_eq!(
        registry
            .read()
            .get(child_id)
            .ok_or("dead child must remain observable")?
            .status,
        AgentStatus::Failed,
        "a joined dead wrapper cannot remain falsely Idle",
    );
    assert!(
        registry.read().tombstone(child_id).is_none(),
        "unresolved accepted work must forbid reclamation",
    );
    let retained = infra.pending_messages.messages_for_delivery(child_id);
    assert_eq!(retained.len(), 1, "one exact message must remain retained");
    assert_eq!(
        serde_json::to_value(&retained[0])?,
        expected_message,
        "the recovery FIFO must retain the byte-equivalent message",
    );
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child_id)
            .map(|status| status.pending_count),
        Some(1),
        "wrapper death must promote the idle failure into strong recovery",
    );

    // Drop any remaining public handle and await the aborted task. The weak
    // store must still upgrade solely through retained recovery authority.
    if let Some(handle) = handles.remove(child_id) {
        let AgentHandle { join_handle, .. } = handle;
        let joined = join_handle.await;
        assert!(
            joined.is_err(),
            "the public wrapper abort must be observable"
        );
    }
    let retained_store = weak_child_store
        .upgrade()
        .ok_or("terminal recovery must strongly own the failed child store")?;

    reject_queue.store(false, Ordering::SeqCst);
    let close_retry = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-idle-queue-retry".to_owned(),
                tool_name: "close_agent".to_owned(),
                model_args: json!({
                    "agent_id": child_id.to_string(),
                    "reason": "retry retained queue authority",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert_eq!(
        close_retry.content["shut_down"][0]["status"], "reclaimed",
        "a later close must recover the exact Q before reclaiming",
    );
    assert_eq!(infra.pending_messages.pending_for(child_id), 0);
    assert!(registry.read().get(child_id).is_none());
    assert_eq!(
        registry
            .read()
            .tombstone(child_id)
            .ok_or("recovered close must publish a tombstone")?
            .status,
        AgentStatus::Failed,
    );
    assert_eq!(
        queue_event_count(retained_store.as_ref(), message_id),
        1,
        "recovery must publish exactly one canonical Q",
    );
    assert_eq!(
        infra.pending_messages.retry_terminal_pending(child_id)?,
        TerminalPendingRetryOutcome::NoRecovery,
    );
    assert_eq!(
        queue_event_count(retained_store.as_ref(), message_id),
        1,
        "an idempotent retry must not duplicate the canonical Q",
    );
    Ok(())
}
