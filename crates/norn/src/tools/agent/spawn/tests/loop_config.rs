use super::*;

/// R5: a granted `loop_config.step_timeout_secs` actually binds on
/// the child — a child whose provider never responds is cut off at
/// the granted wall-clock cap with the typed `TimedOut` outcome,
/// delivered honestly as a failed result.
#[tokio::test]
async fn granted_step_timeout_binds_on_child() -> TestResult {
    use crate::agent::output::AgentStopReason;

    // A gate that is never released: without the granted timeout the
    // child would hang forever.
    let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
        gate: Arc::new(tokio::sync::Notify::new()),
        responses: StdMutex::new(vec![vec![done_event()]]),
    });
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({
            "task": "will time out", "model": CATALOG_MODEL, "role": "worker",
            "child_policy": {
                "messaging": "siblings_and_parent",
                "delegation": {
                    "remaining_depth": 0,
                    "max_concurrent_children": 32,
                },
                "inbound_capacity": 32,
                "loop_config": { "step_timeout_secs": 1 },
            },
        }),
    )
    .await;

    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    let result = rx.try_recv()?;
    assert!(!result.succeeded);
    assert!(
        matches!(result.stop, Some(AgentStopReason::TimedOut { .. })),
        "typed timeout outcome expected, got {:?}",
        result.stop,
    );
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("timed out")),
        "the failure names the timeout: {:?}",
        result.error,
    );
    Ok(())
}

/// R5 × `deny_unknown_fields`: a typo'd knob *inside*
/// `child_policy.loop_config` is rejected at the argument boundary —
/// nothing is reserved, and the failure names the unknown field
/// instead of silently leaving the child on library defaults.
#[tokio::test]
async fn spawn_rejects_unknown_loop_config_fields() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {
                        "remaining_depth": 0,
                        "max_concurrent_children": 32,
                    },
                    "inbound_capacity": 32,
                    "loop_config": { "linger_seconds": 5 },
                },
            })),
            &ctx,
        )
        .await;
    let Err(ToolError::ExecutionFailed { reason }) = result else {
        return Err(format!("expected ExecutionFailed, got {result:?}").into());
    };
    assert!(
        reason.contains("linger_seconds"),
        "the failure names the unknown field: {reason}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "a refused spawn reserves nothing",
    );
    Ok(())
}

/// R5 status quo: a spawn without `loop_config` stamps
/// `loop_config: None` on the child's grant — the launch then runs
/// `AgentLoopConfig::default()` byte-for-byte (pinned at unit level
/// by `loop_config_none_resolves_to_default_config_exactly`), so
/// existing spawns are behaviorally untouched by R5.
#[tokio::test]
async fn spawn_without_loop_config_stamps_none() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "plain", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .policy
            .loop_config,
        None,
        "inherit-with-decrement from an envelope without loop_config grants None",
    );
    Ok(())
}

/// Seam I2-3 (idle-park drain): a message pushed through the
/// parent-held [`AgentHandle::inbound_tx`] while the child is parked
/// Idle must become durable (pending store + `agent_message.queued`
/// audit) and wake-eligible — `wake_agent`'s pending-store gate
/// accepts it and the woken step delivers it into the child's
/// conversation. This is the router guarantee ("a message some loop
/// will drain") extended across the park window.
#[tokio::test]
async fn message_to_parked_child_becomes_durable_and_wake_delivers_it() -> TestResult {
    use crate::r#loop::inbound::{ChannelMessage, MessageKind};
    use crate::tools::agent::coord::WakeAgentTool;

    // Response 1 completes the initial task (child parks Idle);
    // response 2 answers the wake step that drains the mailbox.
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::TextDelta {
                text: "initial done".to_string(),
            },
            done_event(),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "drained mailbox".to_string(),
            },
            done_event(),
        ],
    ]));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "wait for mail", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );

    // Take the parent-held handle: its inbound sender feeds the parked
    // child's channel directly (bypassing the router, which is
    // deregistered while parked), and its event store is the child's.
    let handle = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .remove(child_id)
        .ok_or("required test value")?;

    let message = ChannelMessage {
        id: Uuid::new_v4(),
        sender_id: parent,
        from: "root".to_owned(),
        role: None,
        to_id: child_id,
        content: "note for the parked child".to_owned(),
        kind: MessageKind::Update,
        seq: None,
        timestamp: Utc::now(),
    };
    handle.inbound_tx.send(message).await?;

    // The park arm must route the acknowledged message into the
    // durable pending store.
    tokio::time::timeout(Duration::from_secs(5), async {
        while infra.pending_messages.pending_for(child_id) != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let queued_audit_present = handle.event_store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
        )
    });
    assert!(
        queued_audit_present,
        "the idle-park re-queue must persist an agent_message.queued audit",
    );

    // The pending-store wake gate is now authoritative: the stranded
    // message makes the parked child wakeable.
    let wake_out = WakeAgentTool::new()
        .execute(
            &envelope_for(json!({ "agent_id": child_id.to_string() })),
            &ctx,
        )
        .await?;
    assert!(!wake_out.is_error(), "{:?}", wake_out.content);
    assert_eq!(wake_out.content["woken"], true);
    assert_eq!(wake_out.content["queued_messages"], 1);

    // The woken step drains the pending store and injects the framed
    // message into the child's conversation, then parks Idle again.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let delivered = handle.event_store.events().iter().any(|e| {
                matches!(
                    e,
                    SessionEvent::UserMessage { content, .. }
                        if content.contains("<agent_message")
                            && content.contains("note for the parked child")
                )
            });
            if delivered && infra.pending_messages.pending_for(child_id) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;

    let mut status_rx = handle.status_rx.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        status_rx
            .wait_for(|status| *status == AgentStatus::Idle)
            .await
    })
    .await??;
    Ok(())
}
