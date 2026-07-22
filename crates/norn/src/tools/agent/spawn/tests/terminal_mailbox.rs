use super::*;

use crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE;
use crate::integration::hooks::{Hook, HookRegistry, StopHook, SubagentHook};
use crate::r#loop::inbound::{ChannelMessage, InboundTrySendError, MessageKind};

struct TerminalGates {
    runner_stop_entered: tokio::sync::Notify,
    release_runner_stop: tokio::sync::Notify,
    wrapper_stop_entered: tokio::sync::Notify,
    release_wrapper_stop: tokio::sync::Notify,
}

impl TerminalGates {
    fn new() -> Self {
        Self {
            runner_stop_entered: tokio::sync::Notify::new(),
            release_runner_stop: tokio::sync::Notify::new(),
            wrapper_stop_entered: tokio::sync::Notify::new(),
            release_wrapper_stop: tokio::sync::Notify::new(),
        }
    }
}

struct BlockingRunnerStop {
    gates: Arc<TerminalGates>,
}

#[async_trait]
impl StopHook for BlockingRunnerStop {
    async fn on_stop(&self, _final_text: &str) -> HookOutcome {
        self.gates.runner_stop_entered.notify_one();
        self.gates.release_runner_stop.notified().await;
        HookOutcome::Proceed
    }
}

struct BlockingWrapperStop {
    gates: Arc<TerminalGates>,
}

#[async_trait]
impl SubagentHook for BlockingWrapperStop {
    async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}

    async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
        self.gates.wrapper_stop_entered.notify_one();
        self.gates.release_wrapper_stop.notified().await;
        HookOutcome::Proceed
    }
}

fn direct_message(to_id: Uuid, content: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: Uuid::new_v4(),
        from: "/root/fork-parent".to_owned(),
        role: Some("parent".to_owned()),
        to_id,
        content: content.to_owned(),
        kind: MessageKind::Update,
        seq: None,
        timestamp: chrono::Utc::now(),
    }
}

fn canonical_queue_count(store: &EventStore, message_id: Uuid) -> usize {
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

async fn wait_for_gate(gate: &tokio::sync::Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(5), gate.notified()).await?;
    Ok(())
}

/// The real spawn wrapper closes direct delivery before its terminal awaits.
/// Every send accepted before closure gets one canonical queue authority, a
/// fresh send after closure fails, and capacity reserved before closure is
/// revoked rather than acknowledging an undurable send after completion.
#[tokio::test]
async fn terminal_wrapper_revokes_reserved_capacity_before_completion() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "finished".to_owned(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let parent_policy = test_envelope().child_policy;
    let parent_guard = AgentRegistry::reserve(
        &agent_registry,
        "/root/fork-parent".to_owned(),
        "fork".to_owned(),
        CATALOG_MODEL.to_owned(),
        None,
        parent_policy,
        None,
    )?;
    let parent_id = parent_guard.id();
    parent_guard.confirm()?;

    let ctx = parent_ctx(
        provider,
        parent_id,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let gates = Arc::new(TerminalGates::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Stop(Box::new(BlockingRunnerStop {
        gates: Arc::clone(&gates),
    })));
    hooks.register(Hook::Subagent(Box::new(BlockingWrapperStop {
        gates: Arc::clone(&gates),
    })));
    ctx.insert_extension(Arc::new(hooks));

    let output = SpawnAgentTool::new()
        .execute(
            &envelope_for(json!({
                "task": "finish once",
                "model": CATALOG_MODEL,
                "role": "worker",
            })),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        output.content["agent_id"]
            .as_str()
            .ok_or("spawn output must carry an agent id")?,
    )?;
    wait_for_gate(&gates.runner_stop_entered).await?;

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("spawn context must retain child handles")?;
    let handle = handles
        .remove(child_id)
        .ok_or("spawn must install the child handle before its run completes")?;
    let pre_close_a = direct_message(child_id, "accepted before close a");
    let pre_close_b = direct_message(child_id, "accepted before close b");
    let pre_close_ids = [pre_close_a.id, pre_close_b.id];
    handle.inbound_tx.send(pre_close_a).await?;
    handle.inbound_tx.send(pre_close_b).await?;
    let reserved = handle.inbound_tx.reserve().await?;

    gates.release_runner_stop.notify_one();
    wait_for_gate(&gates.wrapper_stop_entered).await?;

    let rejected_message = direct_message(child_id, "must be rejected after close");
    let rejected_message_id = rejected_message.id;
    let post_close_result = handle.inbound_tx.try_send(rejected_message);
    gates.release_wrapper_stop.notify_one();

    assert_eq!(
        post_close_result,
        Err(InboundTrySendError::Closed),
        "the terminal transition must close direct delivery before wrapper awaits",
    );
    handle.join_handle.await?;
    let revoked_message = direct_message(child_id, "must be revoked after completion");
    let revoked_message_id = revoked_message.id;
    let revoked_result = reserved.send(revoked_message);
    assert!(
        revoked_result.is_err(),
        "a capacity reservation must not acknowledge a send after terminal completion",
    );

    for message_id in pre_close_ids {
        assert_eq!(
            canonical_queue_count(handle.event_store.as_ref(), message_id),
            1,
            "every successful pre-close direct send needs one canonical Q",
        );
    }
    assert_eq!(
        canonical_queue_count(handle.event_store.as_ref(), rejected_message_id),
        0,
        "a send rejected after closure must not mint queue authority",
    );
    assert_eq!(
        canonical_queue_count(handle.event_store.as_ref(), revoked_message_id),
        0,
        "a revoked reservation must not mint queue authority",
    );
    Ok(())
}

/// A Q outage in the real spawn controller must shape every outward terminal
/// observation before delivery and prevent delivery-anchored reclamation.
#[tokio::test]
async fn terminal_queue_failure_downgrades_spawn_and_retains_authority() -> TestResult {
    use crate::agent::result_channel::ChildResultSender;
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
    };
    use crate::tools::agent::{
        ReclaimOnResultDelivery, TestChildEventStore, TestTerminalTransitionGate,
        terminal_queue_failure_store,
    };

    const PRIVATE_PAYLOAD: &str = "private accepted payload";
    const SINK_DIAGNOSTIC: &str = "spawn sink diagnostic must remain internal";

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "finished successfully".to_owned(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let parent_guard = AgentRegistry::reserve(
        &agent_registry,
        "/root/fault-parent".to_owned(),
        "fork".to_owned(),
        CATALOG_MODEL.to_owned(),
        None,
        test_envelope().child_policy,
        None,
    )?;
    let parent_id = parent_guard.id();
    parent_guard.confirm()?;
    let ctx = parent_ctx(
        provider,
        parent_id,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let pending = Arc::clone(
        &ctx.get_extension::<AgentToolInfra>()
            .ok_or("spawn context must carry agent infra")?
            .pending_messages,
    );
    let child_store = terminal_queue_failure_store(SINK_DIAGNOSTIC);
    ctx.insert_extension(Arc::new(TestChildEventStore(Arc::clone(&child_store))));
    let transition_gate = Arc::new(TestTerminalTransitionGate::new());
    ctx.insert_extension(Arc::clone(&transition_gate));
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel(4);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(result_tx))));
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<AgentEvent>(32);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(event_tx)));

    let gates = Arc::new(TerminalGates::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Subagent(Box::new(BlockingWrapperStop {
        gates: Arc::clone(&gates),
    })));
    ctx.insert_extension(Arc::new(hooks));

    let output = SpawnAgentTool::new()
        .execute(
            &envelope_for(json!({
                "task": "finish once",
                "model": CATALOG_MODEL,
                "role": "worker",
            })),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        output.content["agent_id"]
            .as_str()
            .ok_or("spawn output must carry an agent id")?,
    )?;
    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("spawn context must retain child handles")?;
    let mut status_rx = handles
        .status_rx(child_id)
        .ok_or("spawn handle must expose terminal status")?;
    let inbound = handles
        .inbound_tx(child_id)
        .ok_or("spawn handle must expose inbound sender")?;

    tokio::time::timeout(Duration::from_secs(5), transition_gate.wait_until_entered()).await?;
    let private_message = direct_message(child_id, PRIVATE_PAYLOAD);
    let private_message_id = private_message.id.to_string();
    inbound.send(private_message).await?;
    transition_gate.release();
    wait_for_gate(&gates.wrapper_stop_entered).await?;
    assert_eq!(
        pending
            .terminal_pending_recovery_status(child_id)
            .map(|status| status.pending_count),
        Some(1),
        "the failed canonical Q must retain payload authority before emission",
    );
    gates.release_wrapper_stop.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(5), result_rx.recv())
        .await?
        .ok_or("spawn controller must deliver a result")?;
    assert!(!result.succeeded, "Q failure cannot emit spawn success");
    let error = result
        .error
        .as_deref()
        .ok_or("failure must carry an error")?;
    assert_eq!(
        error, "terminal agent-message persistence failed; accepted work was not confirmed durable",
        "model-facing failure must remain the exact payload-free diagnosis",
    );
    for forbidden in [
        SINK_DIAGNOSTIC,
        PRIVATE_PAYLOAD,
        private_message_id.as_str(),
    ] {
        assert!(
            !error.contains(forbidden),
            "model-facing failure disclosed internal terminal-queue data: {forbidden}",
        );
    }
    assert_eq!(
        result.usage.input_tokens, 10,
        "own usage is preserved; result: {result:?}"
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        status_rx.wait_for(|status| *status == AgentStatus::Failed),
    )
    .await??;

    assert!(
        handles.contains(child_id),
        "unresolved Q forbids handle reclaim"
    );
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("failed child registry entry must remain")?
            .status,
        AgentStatus::Failed,
    );
    let lifecycle_failed = std::iter::from_fn(|| event_rx.try_recv().ok()).any(|event| {
        event.agent_id == child_id
            && matches!(
                event.event,
                AgentEventKind::Subagent(SubagentLifecycle::Completed {
                    succeeded: false,
                    ..
                })
            )
    });
    assert!(lifecycle_failed, "lifecycle completion must report failure");

    let handle = handles
        .remove(child_id)
        .ok_or("test cleanup requires retained handle")?;
    handle.join_handle.await?;
    Ok(())
}
