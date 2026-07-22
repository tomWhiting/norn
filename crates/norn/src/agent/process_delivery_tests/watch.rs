//! Background-process watch alert delivery tests.

use std::sync::Arc;

use uuid::Uuid;

use super::super::*;
use super::support::{HomeGuard, delivery, match_alert};
use crate::r#loop::inbound::inbound_channel;

/// R3: the alert content parses with every structured field and is carried on
/// a `norn:watch` nil-sender unsequenced steer. The excerpt remains byte-exact.
#[test]
fn watch_match_alert_parses_field_by_field() {
    let agent_id = Uuid::new_v4();
    let sink = delivery(
        agent_id,
        None,
        Arc::new(PendingAgentMessages::new()),
        Arc::new(EventStore::new()),
    );
    let message = sink.build_watch_message(&match_alert("w1", "p1"));
    assert_eq!(message.from, WATCH_SENDER_LABEL);
    assert_eq!(message.sender_id, Uuid::nil());
    assert_eq!(message.kind, MessageKind::Steer);
    assert!(message.seq.is_none());
    assert_eq!(message.role, None);
    assert_eq!(message.to_id, agent_id);

    let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
    assert_eq!(payload["type"], "watch_match");
    assert_eq!(payload["watch_id"], "w1");
    assert_eq!(payload["process_id"], "p1");
    assert_eq!(payload["brief"], "watch for errors");
    assert_eq!(payload["excerpt"], "ERROR: boom\n");
    assert_eq!(payload["spool_range"]["start"], 10);
    assert_eq!(payload["spool_range"]["end"], 42);
    assert!(
        payload["matched_at"].as_str().is_some(),
        "matched_at is present as a timestamp",
    );
}

/// R4: a watch-error alert carries the error and examined spool range,
/// distinctly typed from a match and without an excerpt field.
#[test]
fn watch_error_alert_carries_error_and_range() {
    let agent_id = Uuid::new_v4();
    let sink = delivery(
        agent_id,
        None,
        Arc::new(PendingAgentMessages::new()),
        Arc::new(EventStore::new()),
    );
    let alert = WatchAlert {
        watch_id: "w2".to_owned(),
        process_id: "p3".to_owned(),
        brief: "b".to_owned(),
        spool_start: 0,
        spool_end: 12,
        kind: WatchAlertKind::Error {
            error: "filter is not runnable".to_owned(),
        },
    };
    let message = sink.build_watch_message(&alert);
    let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
    assert_eq!(payload["type"], "watch_error");
    assert_eq!(payload["watch_id"], "w2");
    assert_eq!(payload["process_id"], "p3");
    assert_eq!(payload["error"], "filter is not runnable");
    assert_eq!(payload["spool_range"]["end"], 12);
    assert!(payload["excerpt"].is_null(), "an error carries no excerpt");
}

/// R3: without a live channel a watch alert lands in the durable pending store
/// with an `agent_message.queued` audit, ready for the next step.
#[test]
fn watch_alert_queues_durably_with_a_queued_audit() {
    let agent_id = Uuid::new_v4();
    let store = Arc::new(EventStore::new());
    let pending = Arc::new(PendingAgentMessages::new());
    let sink = delivery(agent_id, None, Arc::clone(&pending), Arc::clone(&store));
    sink.deliver_watch_alert(match_alert("w1", "p1"));

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

/// R3 headline: an agent lingering at a would-stop boundary is woken by a real
/// process watch match. Its framed alert persists in the conversation as a
/// `UserMessage` event.
#[tokio::test]
#[serial_test::serial]
async fn watch_match_wakes_a_lingering_agent() {
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
    let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), Some(sink)));
    let cwd = std::env::current_dir().unwrap();
    let handle = manager
        .spawn("sleep 0.2; echo WATCHED-LINE", &cwd, None)
        .await
        .unwrap();
    manager
        .attach_watch(
            handle.label(),
            "watched output".to_owned(),
            "grep WATCHED-LINE".to_owned(),
            cwd,
            None,
        )
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
        "the watch match must wake the linger and drive another iteration",
    );
    let injected = session_store.events().iter().any(|event| {
        matches!(
            event,
            crate::session::events::SessionEvent::UserMessage { content, .. }
                if content.contains("<agent_message from=\"norn:watch\"")
                    && content.contains("watch_match")
                    && content.contains("WATCHED-LINE")
        )
    });
    assert!(
        injected,
        "the injected norn:watch frame persists as a UserMessage carrying the excerpt",
    );
    manager.shutdown();
}
