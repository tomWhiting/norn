use super::*;

use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, StopHook, SubagentHook};
use crate::r#loop::inbound::InboundTrySendError;

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

struct GateRunnerStop {
    gates: Arc<TerminalGates>,
}

#[async_trait]
impl StopHook for GateRunnerStop {
    async fn on_stop(&self, _final_text: &str) -> HookOutcome {
        self.gates.runner_stop_entered.notify_one();
        self.gates.release_runner_stop.notified().await;
        HookOutcome::Proceed
    }
}

struct GateWrapperStop {
    gates: Arc<TerminalGates>,
}

#[async_trait]
impl SubagentHook for GateWrapperStop {
    async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}

    async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
        self.gates.wrapper_stop_entered.notify_one();
        self.gates.release_wrapper_stop.notified().await;
        HookOutcome::Proceed
    }
}

fn direct_message(sender_id: Uuid, recipient_id: Uuid, content: &str) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4(),
        sender_id,
        from: "root".to_owned(),
        role: None,
        to_id: recipient_id,
        content: content.to_owned(),
        kind: MessageKind::Steer,
        seq: None,
        timestamp: Utc::now(),
    }
}

async fn wait_for_gate(gate: &tokio::sync::Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(5), gate.notified()).await?;
    Ok(())
}

/// The real fork wrapper closes its direct inbound sender before terminal
/// awaits. Two messages exercise ordinary pre-close acceptance; a fresh send
/// after closure fails, and a pre-close capacity reservation is revoked rather
/// than acknowledging an undurable send after wrapper completion.
#[tokio::test]
async fn fork_terminal_wrapper_revokes_reserved_capacity_before_completion() -> TestResult {
    let provider_gate = Arc::new(tokio::sync::Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
        gate: Arc::clone(&provider_gate),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_owned(),
                call_id: None,
                name: Some("structured_output".to_owned()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ]]),
    });
    let parent_id = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent_id,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let gates = Arc::new(TerminalGates::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Stop(Box::new(GateRunnerStop {
        gates: Arc::clone(&gates),
    })));
    hooks.register(Hook::Subagent(Box::new(GateWrapperStop {
        gates: Arc::clone(&gates),
    })));
    ctx.insert_extension(Arc::new(hooks));

    let output = ForkTool::new()
        .execute(
            &envelope_for(json!({
                "request": "finish after the gate",
                "model": "gpt-5.5",
                "requirements": [],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&output)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    let inbound_tx = handle.inbound_tx;
    let join_handle = handle.join_handle;

    let first = direct_message(parent_id, fork_id, "accepted before close one");
    let second = direct_message(parent_id, fork_id, "accepted before close two");

    provider_gate.notify_one();
    wait_for_gate(&gates.runner_stop_entered).await?;

    inbound_tx.send(first.clone()).await?;
    inbound_tx.send(second.clone()).await?;
    let reserved = inbound_tx.reserve().await?;
    gates.release_runner_stop.notify_one();
    wait_for_gate(&gates.wrapper_stop_entered).await?;

    let rejected_message = direct_message(parent_id, fork_id, "too late");
    let rejected_message_id = rejected_message.id;
    let rejected_message_id_text = rejected_message_id.to_string();
    let post_close_result = inbound_tx.try_send(rejected_message);

    gates.release_wrapper_stop.notify_one();
    assert_eq!(
        post_close_result,
        Err(InboundTrySendError::Closed),
        "terminal completion must close direct delivery before any stop-hook await",
    );
    join_handle.await?;
    let revoked_message = direct_message(parent_id, fork_id, "must be revoked after completion");
    let revoked_message_id = revoked_message.id;
    let revoked_message_id_text = revoked_message_id.to_string();
    let revoked_result = reserved.send(revoked_message);
    assert!(
        revoked_result.is_err(),
        "a capacity reservation must not acknowledge a send after terminal completion",
    );

    let events = child_store.events();
    let queued: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Custom {
                base,
                event_type,
                data,
            } if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE => {
                Some((base.id.as_str(), data))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        queued.len(),
        2,
        "the terminal handoff must persist one canonical queue row per pre-close acceptance",
    );

    for accepted in [&first, &second] {
        let accepted_id = accepted.id.to_string();
        let rows: Vec<_> = queued
            .iter()
            .filter(|(_, data)| data["message_id"] == accepted_id)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "accepted message {} must have exactly one canonical queue row",
            accepted.id,
        );
        let (event_id, data) = required(
            rows.first().copied(),
            format!("canonical queue row for accepted message {}", accepted.id),
        )?;
        assert_eq!(data["authoritative"], true);
        assert!(
            data["mailbox_id"].is_string(),
            "canonical queue authority must carry the closed mailbox identity",
        );
        assert_eq!(
            *event_id,
            format!("norn:pending-agent-message:queued:{}", accepted.id),
            "terminal preservation must use the stable canonical queue event identity",
        );
    }
    assert!(
        queued.iter().all(|(_, data)| {
            data.get("message_id").and_then(serde_json::Value::as_str)
                != Some(rejected_message_id_text.as_str())
        }),
        "a send rejected after closure must not mint queue authority",
    );
    assert!(
        queued.iter().all(|(_, data)| {
            data.get("message_id").and_then(serde_json::Value::as_str)
                != Some(revoked_message_id_text.as_str())
        }),
        "a revoked reservation must not mint queue authority",
    );
    Ok(())
}
