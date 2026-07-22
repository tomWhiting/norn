use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::json;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::*;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::agent::{PendingAgentMessages, PendingMailboxLease};
use crate::r#loop::inbound::{InboundChannel, inbound_channel};
use crate::session::SessionBinding;
use crate::session::events::SessionEvent;
use crate::session::persistence::SessionPersistError;
use crate::session::store::{EventStore, PersistenceSink};
use crate::tools::agent::coord::test_support::{
    build_infra, envelope_for, register_agent, synthetic_handle,
};
use crate::tools::agent::handle::{AgentHandle, ChildBranchMetadata};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn test_failure(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}

#[derive(Clone, Copy)]
enum RecoverySinkMode {
    RecoverOnRetry,
    NeverRecover,
}

struct TerminalRecoverySink {
    attempts: Arc<AtomicUsize>,
    mode: RecoverySinkMode,
}

impl PersistenceSink for TerminalRecoverySink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        // `append_idempotent` makes one write plus one exact reconciliation.
        // Failing attempts 0 and 1 creates recovery; attempt 2 is close's retry.
        if matches!(self.mode, RecoverySinkMode::RecoverOnRetry) && attempt >= 2 {
            return Ok(());
        }
        Err(SessionPersistError::Io(std::io::Error::other(
            "injected terminal queue outage",
        )))
    }
}

fn recovery_store(mode: RecoverySinkMode) -> Arc<EventStore> {
    Arc::new(EventStore::with_sink(Box::new(TerminalRecoverySink {
        attempts: Arc::new(AtomicUsize::new(0)),
        mode,
    })))
}

fn terminal_message(recipient_id: Uuid, content: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::new_v4(),
        from: "/sender".to_string(),
        role: Some("worker".to_string()),
        to_id: recipient_id,
        content: content.to_string(),
        kind: MessageKind::Update,
        seq: None,
        timestamp: Utc::now(),
    }
}

fn retain_terminal_recovery(
    pending_messages: &PendingAgentMessages,
    router: &MessageRouter,
    recipient_id: Uuid,
    store: &Arc<EventStore>,
    content: &str,
) -> TestResult {
    let lease = Arc::new(PendingMailboxLease::new());
    pending_messages.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        store,
        &lease,
    )?;
    let (sender, mut inbound) = inbound_channel(2);
    router.register(recipient_id, sender.clone());
    sender
        .try_send(terminal_message(recipient_id, content))
        .map_err(|error| test_failure(format!("accept terminal message: {error}")))?;
    let transition = pending_messages.transition_live_route(
        recipient_id,
        store.as_ref(),
        router,
        &mut inbound,
        Some(&lease),
    )?;
    assert!(transition.closed.is_some(), "terminal mailbox must close");
    assert!(
        transition.first_error.is_some(),
        "the injected persistence failure must surface",
    );
    assert_eq!(
        pending_messages
            .terminal_pending_recovery_status(recipient_id)
            .map(|status| status.pending_count),
        Some(1),
    );
    Ok(())
}

fn handle_creating_terminal_recovery(
    recipient_id: Uuid,
    registry: Arc<parking_lot::RwLock<AgentRegistry>>,
    pending_messages: &Arc<PendingAgentMessages>,
    router: Arc<MessageRouter>,
    store: Arc<EventStore>,
    content: &str,
) -> TestResult<AgentHandle> {
    let lease = Arc::new(PendingMailboxLease::new());
    pending_messages.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let (route_tx, mut route_inbound) = inbound_channel(2);
    router.register(recipient_id, route_tx.clone());
    route_tx
        .try_send(terminal_message(recipient_id, content))
        .map_err(|error| test_failure(format!("accept terminal message: {error}")))?;

    // Keep the shutdown steer out of the recovery fixture: the wrapper owns
    // the routed channel above, while this deliberately closed capability makes
    // close's best-effort direct steer fail immediately.
    let (inbound_tx, inbound_rx): (_, InboundChannel) = inbound_channel(1);
    drop(inbound_rx);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    drop(status_tx);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_store = Arc::clone(&store);
    let task_pending_messages = Arc::clone(pending_messages);
    let join_handle = tokio::spawn(async move {
        task_cancel.cancelled().await;
        let transition = task_pending_messages.transition_live_route(
            recipient_id,
            task_store.as_ref(),
            router.as_ref(),
            &mut route_inbound,
            Some(&lease),
        );
        assert!(
            transition
                .as_ref()
                .is_ok_and(|transition| transition.first_error.is_some()),
            "terminal transition must retain the injected persistence failure",
        );
        assert_eq!(
            task_pending_messages
                .terminal_pending_recovery_status(recipient_id)
                .map(|status| status.pending_count),
            Some(1),
        );
        let marked = registry.write().mark_completed(recipient_id);
        assert!(
            marked.is_ok(),
            "wrapper must record terminal status: {marked:?}",
        );
    });

    Ok(AgentHandle {
        agent_id: recipient_id,
        status_rx,
        inbound_tx,
        wake_tx: mpsc::channel(1).0,
        wake_pending: Arc::new(AtomicBool::new(false)),
        cancel,
        join_handle,
        event_store: store,
        branch_metadata: ChildBranchMetadata {
            child_agent_id: recipient_id,
            parent_agent_id: Uuid::new_v4(),
            profile_name: None,
            spawned_at: Utc::now(),
        },
    })
}

#[tokio::test]
async fn close_agent_recovers_terminal_queue_created_during_join_before_reclaim() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/succeeds", None);
    let store = recovery_store(RecoverySinkMode::RecoverOnRetry);
    let handles = Arc::new(AgentHandles::new());
    handles.insert(handle_creating_terminal_recovery(
        child,
        Arc::clone(&registry),
        &infra.pending_messages,
        Arc::clone(&router),
        Arc::clone(&store),
        "private success payload",
    )?);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));
    let out = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": child.to_string()})),
            &ctx,
        )
        .await?;

    assert_eq!(out.content["shut_down"][0]["status"], "reclaimed");
    assert_eq!(store.len(), 1, "the exact retained Q became durable");
    assert!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child)
            .is_none(),
        "successful recovery discharges shared authority",
    );
    assert_eq!(infra.pending_messages.pending_for(child), 0);
    assert!(
        !handles.contains(child),
        "successful close consumes the handle"
    );
    let registry = registry.read();
    assert!(registry.get(child).is_none(), "terminal entry is reclaimed");
    assert_eq!(
        registry.tombstone(child).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "completion remains observable",
    );
    Ok(())
}

#[tokio::test]
async fn close_agent_post_join_recovery_failure_keeps_terminal_registry_entry() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/post-join-fails", None);
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    let handles = Arc::new(AgentHandles::new());
    handles.insert(handle_creating_terminal_recovery(
        child,
        Arc::clone(&registry),
        &infra.pending_messages,
        Arc::clone(&router),
        Arc::clone(&store),
        "private post-join payload",
    )?);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));
    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": child.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure("unresolved post-join recovery succeeded"));
    };
    let ToolError::ExecutionFailed { reason } = error else {
        return Err(test_failure(format!(
            "expected typed execution failure, got {error:?}"
        )));
    };

    assert!(reason.contains(&child.to_string()));
    assert!(reason.contains("1 accepted message"));
    assert!(!reason.contains("private post-join payload"));
    assert!(!reason.contains("injected terminal queue outage"));
    assert!(
        !handles.contains(child),
        "the completed wrapper handle was already joined and consumed",
    );
    assert_eq!(
        registry.read().get(child).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "terminal entry retained",
    );
    assert!(registry.read().tombstone(child).is_none());
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child)
            .map(|status| status.pending_count),
        Some(1),
    );
    assert_eq!(store.len(), 0, "failed recovery cannot claim durable Q");
    Ok(())
}

#[tokio::test]
async fn close_agent_preflight_failure_preserves_token_handle_and_registry() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/preflight-fails", None);
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    retain_terminal_recovery(
        &infra.pending_messages,
        router.as_ref(),
        child,
        &store,
        "private preflight payload",
    )?;
    registry.write().mark_completed(child)?;

    let handles = Arc::new(AgentHandles::new());
    let (handle, _status_tx, _inbound_rx) = synthetic_handle(child);
    let cancel = handle.cancel.clone();
    handles.insert(handle);
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));

    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": child.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure("unresolved preflight recovery succeeded"));
    };
    let ToolError::ExecutionFailed { reason } = error else {
        return Err(test_failure(format!(
            "expected typed execution failure, got {error:?}"
        )));
    };

    assert!(reason.contains("1 accepted message"));
    assert!(!reason.contains("private preflight payload"));
    assert!(
        !cancel.is_cancelled(),
        "preflight failure cannot fire the token"
    );
    assert!(
        handles.contains(child),
        "preflight failure retains the handle"
    );
    assert_eq!(
        registry.read().get(child).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "terminal entry retained",
    );
    assert!(registry.read().tombstone(child).is_none());
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child)
            .map(|status| status.pending_count),
        Some(1),
    );

    cancel.cancel();
    let Some(handle) = handles.remove(child) else {
        return Err(test_failure("synthetic handle disappeared before cleanup"));
    };
    handle.join_handle.await?;
    Ok(())
}

/// Models the no-handle cascade window after shutdown's first postflight: the
/// descendant wrapper has since completed its drain, installed recovery, and
/// published terminal status. The terminal-observation gate must recheck and
/// refuse reclamation while exact retry still fails.
#[test]
fn no_handle_terminal_observation_rechecks_recovery_before_reclaim() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/no-handle-race", None);
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    retain_terminal_recovery(
        &infra.pending_messages,
        router.as_ref(),
        child,
        &store,
        "private no-handle payload",
    )?;
    registry.write().mark_completed(child)?;

    let result = reclaim_observed_terminal(&registry, &infra.pending_messages, child);
    let Err(error) = result else {
        return Err(test_failure("terminal observation bypassed recovery"));
    };
    let ToolError::ExecutionFailed { reason } = error else {
        return Err(test_failure(format!(
            "expected typed execution failure, got {error:?}"
        )));
    };
    assert!(reason.contains("1 accepted message"));
    assert!(!reason.contains("private no-handle payload"));
    assert_eq!(
        registry.read().get(child).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "terminal entry remains visible",
    );
    assert!(registry.read().tombstone(child).is_none());
    Ok(())
}

mod callsite_tests;
