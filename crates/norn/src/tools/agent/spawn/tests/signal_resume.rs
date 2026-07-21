use super::*;

#[tokio::test]
async fn signal_to_idle_child_queues_follow_up_and_wake_drains_mailbox() -> TestResult {
    use crate::tools::agent::coord::{SignalAgentTool, WakeAgentTool};

    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![
            vec![
                ProviderEvent::TextDelta {
                    text: "initial result".to_string(),
                },
                done_event(),
            ],
            vec![
                ProviderEvent::TextDelta {
                    text: "woke and handled queued work".to_string(),
                },
                done_event(),
            ],
        ]),
    });
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let spawn_tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &spawn_tool,
        &ctx,
        json!({"task": "wait for later instructions", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;
    let initial = rx.try_recv()?;
    assert_eq!(initial.agent_id, child_id);
    assert!(initial.succeeded);

    let signal_tool = SignalAgentTool::new();
    let signal_out = signal_tool
        .execute(
            &ToolEnvelope {
                tool_call_id: "signal-idle".to_owned(),
                tool_name: "signal_agent".to_owned(),
                model_args: json!({
                    "to": child_id.to_string(),
                    "kind": "steer",
                    "content": "queued instruction from parent",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!signal_out.is_error(), "{:?}", signal_out.content);
    assert_eq!(signal_out.content["queued"], true);
    assert_eq!(signal_out.content["resume_required"], true);

    let follow_ups =
        crate::tool::traits::Tool::register_follow_ups(&signal_tool, &signal_out, &ctx).await;
    assert_eq!(
        follow_ups.len(),
        1,
        "queued signal exposes a wake follow-up"
    );
    assert_eq!(follow_ups[0].tool, "wake_agent");
    assert_eq!(follow_ups[0].args["agent_id"], child_id.to_string());

    let wake_out = WakeAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "wake-idle".to_owned(),
                tool_name: "wake_agent".to_owned(),
                model_args: json!({ "agent_id": child_id.to_string() }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!wake_out.is_error(), "{:?}", wake_out.content);
    assert_eq!(wake_out.content["woken"], true);
    assert_eq!(wake_out.content["queued_messages"], 1);

    let resumed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(resumed.agent_id, child_id);
    assert!(resumed.succeeded);
    assert!(
        resumed
            .formatted_message
            .contains("woke and handled queued work"),
        "{}",
        resumed.formatted_message,
    );
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

    let requests = captured.lock().clone();
    assert_eq!(requests.len(), 2, "initial step + wake step");
    let woke_with_message = requests[1].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("<agent_message") && content.contains("queued instruction from parent")
        })
    });
    assert!(
        woke_with_message,
        "wake step must receive the queued message through the normal frame: {:?}",
        requests[1].messages,
    );
    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;
    assert!(
        infra
            .pending_messages
            .messages_for_delivery(child_id)
            .is_empty(),
        "wake step drains the durable mailbox"
    );
    Ok(())
}
