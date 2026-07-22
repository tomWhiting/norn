//! Background-process completion delivery tests.

use std::sync::Arc;

use uuid::Uuid;

use super::super::*;
use super::support::{HomeGuard, completion, delivery, delivery_with_runtime};
use crate::r#loop::inbound::inbound_channel;

#[test]
fn delivers_over_a_live_inbound_channel_as_a_nil_sender_steer() {
    let agent_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    let pending = Arc::new(PendingAgentMessages::new());
    let sink = delivery(
        agent_id,
        Some(tx),
        Arc::clone(&pending),
        Arc::new(EventStore::new()),
    );
    sink.deliver_completion(completion("p1", Some(0), false));

    let drained = rx.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].from, PROCESS_MANAGER_SENDER_LABEL);
    assert_eq!(drained[0].sender_id, Uuid::nil());
    assert_eq!(drained[0].kind, MessageKind::Steer);
    assert!(drained[0].seq.is_none());
    let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
    assert_eq!(payload["process_id"], "p1");
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["killed"], false);
    assert_eq!(pending.pending_for(agent_id), 0, "not durably queued");
}

#[test]
fn queues_durably_without_a_live_channel_with_a_queued_audit() {
    let agent_id = Uuid::new_v4();
    let store = Arc::new(EventStore::new());
    let pending = Arc::new(PendingAgentMessages::new());
    let sink = delivery(agent_id, None, Arc::clone(&pending), Arc::clone(&store));
    sink.deliver_completion(completion("p2", None, true));

    assert_eq!(
        pending.pending_for(agent_id),
        1,
        "queued for the next flush"
    );
    let queued = store
        .events()
        .iter()
        .filter(|event| {
            matches!(event, crate::session::events::SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::pending_messages::AGENT_MESSAGE_QUEUED_EVENT_TYPE)
        })
        .count();
    assert_eq!(queued, 1, "one agent_message.queued audit persisted");
}

#[test]
fn killed_disposition_is_distinct_from_a_normal_exit() {
    let agent_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    let sink = delivery(
        agent_id,
        Some(tx),
        Arc::new(PendingAgentMessages::new()),
        Arc::new(EventStore::new()),
    );
    sink.deliver_completion(completion("p3", None, true));
    let drained = rx.drain();
    let payload: serde_json::Value = serde_json::from_str(&drained[0].content).unwrap();
    assert_eq!(payload["killed"], true);
    assert_eq!(payload["exit_code"], serde_json::Value::Null);
    assert!(payload["hint"].as_str().unwrap().contains("killed"));
}

/// C17 end-to-end: a real manager-owned process, wired to a live inbound
/// channel, delivers its completion as a `norn:process-manager` steer when it
/// exits -- the supervisor to sink path proven with a real subprocess.
#[tokio::test]
#[serial_test::serial]
async fn real_process_completion_delivers_a_steer_over_a_live_channel() {
    use crate::process::ProcessManager;

    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let agent_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    let sink: Arc<dyn ProcessNotifier> = Arc::new(delivery(
        agent_id,
        Some(tx),
        Arc::new(PendingAgentMessages::new()),
        Arc::new(EventStore::new()),
    ));
    let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
    let cwd = std::env::current_dir().unwrap();
    let handle = manager.spawn("echo hi", &cwd, None).await.unwrap();

    // Wait for exit, then for the supervisor to run the sink.
    let mut delivered = Vec::new();
    for _ in 0..200 {
        delivered.extend(rx.drain());
        if !delivered.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(delivered.len(), 1, "one completion steer delivered");
    assert_eq!(delivered[0].from, PROCESS_MANAGER_SENDER_LABEL);
    assert_eq!(delivered[0].kind, MessageKind::Steer);
    let payload: serde_json::Value = serde_json::from_str(&delivered[0].content).unwrap();
    assert_eq!(payload["process_id"], handle.label());
    assert_eq!(payload["exit_code"], 0);
}

/// C17 idle-child path: with no live channel the completion is queued
/// durably and a wake is requested through the registry.
#[tokio::test]
async fn idle_child_completion_queues_and_wakes() {
    use std::sync::atomic::AtomicBool;

    use tokio::sync::{mpsc, watch};
    use tokio_util::sync::CancellationToken;

    use crate::tools::agent::AgentHandle;
    use crate::tools::agent::coord::test_support::register_agent;

    let registry = AgentRegistry::shared();
    let child = register_agent(&registry, "/root/child", None);
    registry.write().mark_idle(child).expect("mark idle");

    let wake = Arc::new(AgentWakeRegistry::new());
    let (_status_tx, status_rx) = watch::channel(AgentStatus::Idle);
    let (inbound_tx, _inbound_rx) = inbound_channel(8);
    let (wake_tx, mut wake_rx) = mpsc::channel(1);
    let event_store = Arc::new(EventStore::new());
    let handle = AgentHandle {
        agent_id: child,
        status_rx,
        inbound_tx,
        wake_tx,
        wake_pending: Arc::new(AtomicBool::new(false)),
        cancel: CancellationToken::new(),
        join_handle: tokio::spawn(async {}),
        event_store: Arc::clone(&event_store),
        branch_metadata: crate::tools::agent::handle::ChildBranchMetadata {
            child_agent_id: child,
            parent_agent_id: Uuid::nil(),
            profile_name: None,
            spawned_at: Utc::now(),
        },
    };
    wake.insert(handle.wake_handle());

    let pending = Arc::new(PendingAgentMessages::new());
    let sink = delivery_with_runtime(
        child,
        None,
        Arc::clone(&pending),
        event_store,
        Some(Arc::clone(&registry)),
        Some(Arc::clone(&wake)),
    );
    sink.deliver_completion(completion("p1", Some(0), false));

    assert_eq!(
        pending.pending_for(child),
        1,
        "queued durably for the idle child"
    );
    assert!(wake_rx.recv().await.is_some(), "a wake was requested");
}

/// C18 end-to-end: an agent lingering at its would-stop boundary is woken by a
/// real background process's completion. The injected agent-message frame
/// appears in the conversation as a persisted `UserMessage` and the loop runs
/// again.
#[tokio::test]
#[serial_test::serial]
async fn completion_wakes_a_lingering_agent() {
    use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
    use crate::r#loop::linger::LingerPolicy;
    use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
    use crate::process::ProcessManager;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;

    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let agent_id = Uuid::new_v4();
    let session_store = Arc::new(EventStore::new());
    let (tx, mut inbound) = inbound_channel(8);
    let sink: Arc<dyn ProcessNotifier> = Arc::new(delivery(
        agent_id,
        Some(tx),
        Arc::new(PendingAgentMessages::new()),
        Arc::clone(&session_store),
    ));
    // A short-lived process completes within the linger deadline. Real time is
    // deliberate because a real subprocess cannot use paused virtual time.
    let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
    let cwd = std::env::current_dir().unwrap();
    manager
        .spawn("sleep 0.2; echo bg-done", &cwd, None)
        .await
        .unwrap();

    let text_turn = |text: &str| {
        vec![
            ProviderEvent::TextDelta {
                text: text.to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]
    };
    let provider = MockProvider::new(vec![text_turn("first"), text_turn("after wake")]);
    let tool_executor = MockToolExecutor::empty();
    let mut loop_context = crate::r#loop::loop_context::LoopContext::new("system");
    let config = AgentLoopConfig {
        linger: Some(LingerPolicy {
            deadline: std::time::Duration::from_secs(3),
        }),
        ..AgentLoopConfig::default()
    };
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &tool_executor,
        store: session_store.as_ref(),
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: Some(&mut inbound),
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await
    .expect("run_agent_step");

    assert!(matches!(
        result,
        crate::r#loop::config::AgentStepResult::Completed { .. }
    ));
    assert_eq!(
        provider.call_count(),
        2,
        "the completion steer must wake the linger and drive another iteration",
    );
    let injected = session_store.events().iter().any(|event| {
        matches!(
            event,
            crate::session::events::SessionEvent::UserMessage { content, .. }
                if content.contains("<agent_message from=\"norn:process-manager\"")
                    && content.contains("process_id")
        )
    });
    assert!(
        injected,
        "the injected norn:process-manager frame persists as a UserMessage"
    );
}

/// C17 durable path end-to-end: a completion queued with no live channel is
/// injected as a framed process-manager `UserMessage` by the next step's
/// pending flush.
#[tokio::test]
async fn queued_completion_is_injected_by_next_step_pending_flush() {
    use crate::r#loop::config::{AgentLoopConfig, MockToolExecutor};
    use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;

    let agent_id = Uuid::new_v4();
    let session_store = Arc::new(EventStore::new());
    let pending = Arc::new(PendingAgentMessages::new());
    let sink = delivery(
        agent_id,
        None,
        Arc::clone(&pending),
        Arc::clone(&session_store),
    );
    sink.deliver_completion(completion("p1", Some(0), false));
    assert_eq!(pending.pending_for(agent_id), 1, "queued, not delivered");

    let provider = MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "ack".to_string(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]);
    let executor = MockToolExecutor::empty();
    let mut loop_context = crate::r#loop::loop_context::LoopContext::new("system");
    loop_context.agent_id = Some(agent_id);
    loop_context.pending_agent_messages = Some(Arc::clone(&pending));
    let config = AgentLoopConfig::default();
    run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: session_store.as_ref(),
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await
    .expect("run_agent_step");

    let injected = session_store.events().iter().any(|event| {
        matches!(
            event,
            crate::session::events::SessionEvent::UserMessage { content, .. }
                if content.contains("from=\"norn:process-manager\"")
                    && content.contains("process_id")
        )
    });
    assert!(
        injected,
        "the queued completion injects as a framed UserMessage"
    );
    assert_eq!(pending.pending_for(agent_id), 0, "the queue drained");
}
