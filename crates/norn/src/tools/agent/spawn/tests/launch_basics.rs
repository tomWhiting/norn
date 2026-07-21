use super::*;

/// R6: spawn returns immediately with `status: "active"` while the child
/// is still blocked, then the child completes asynchronously.
#[tokio::test]
async fn spawn_returns_immediately_then_child_runs_async() -> TestResult {
    let gate = Arc::new(tokio::sync::Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
        gate: Arc::clone(&gate),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]),
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

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    // Child is gated — registry still shows it Active, not Completed.
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Active,
    );

    // Release the gate and let the child finish. `notify_one` stores a
    // permit even if the child has not yet reached `notified()`, so
    // this is race-free regardless of scheduling.
    gate.notify_one();
    let mut status_rx = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .status_rx(child_id)
        .ok_or("required test value")?;
    status_rx
        .wait_for(|status| *status == AgentStatus::Idle)
        .await?;
    // Natural completion parks the child as a wakeable idle actor.
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    assert_eq!(*status_rx.borrow_and_update(), AgentStatus::Idle);
    Ok(())
}

/// N-026 R6 (spawn path): the spawned child's own tool context carries
/// a `ScheduleHandle`, proven behaviorally — the child calls the `cron`
/// tool and the `schedule.created` event lands on the CHILD's event
/// store. A missing extension would fail the call with
/// `MissingExtension` and no such event could exist.
#[tokio::test]
async fn spawned_child_resolves_cron_tool_against_its_own_schedule_handle() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-cron".to_string(),
                call_id: None,
                name: Some("cron".to_string()),
                arguments_delta: r#"{"op":"schedule","in":"12h","message":"check the long job"}"#
                    .to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "scheduled".to_string(),
            },
            done_event(),
        ],
    ]));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let mut tools = ToolRegistry::new();
    crate::tools::registry_builder::register_cron_tool(&mut tools);
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(tools),
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "schedule a check-in", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;
    let created = child_store.events().into_iter().any(|e| {
        matches!(
            &e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::schedule::SCHEDULE_CREATED_EVENT_TYPE
        )
    });
    assert!(
        created,
        "the child's cron call must persist schedule.created to the child's own store",
    );
    Ok(())
}

#[tokio::test]
async fn spawn_agent_without_infra_returns_missing_extension() -> TestResult {
    let tool = SpawnAgentTool::new();
    let envelope = envelope_for(json!({"task": "x", "model": "m", "role": "r"}));
    let ctx = ToolContext::empty();
    let result = tool.execute(&envelope, &ctx).await;
    let Err(ToolError::MissingExtension { extension }) = result else {
        return Err(format!("expected MissingExtension, got {result:?}").into());
    };
    assert!(
        extension.contains("AgentToolInfra"),
        "error must name the missing extension type: {extension}"
    );
    Ok(())
}

/// When `AgentToolInfra.tool_registry` is `None`, spawn refuses to launch.
#[tokio::test]
async fn spawn_agent_errors_when_no_tool_registry() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: None,
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await;
    let Err(ToolError::ExecutionFailed { reason }) = result else {
        return Err(format!("expected ExecutionFailed, got {result:?}").into());
    };
    assert!(
        reason.contains("tool_registry") || reason.contains("tools"),
        "reason must mention missing registry: {reason}"
    );
    Ok(())
}

/// Spawn refuses to launch when the `AgentHandles` extension is absent —
/// a child must never run unobservable.
#[tokio::test]
async fn spawn_agent_errors_when_no_agent_handles() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);

    let tool = SpawnAgentTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await;
    let Err(ToolError::MissingExtension { extension }) = result else {
        return Err(format!("expected MissingExtension, got {result:?}").into());
    };
    assert!(extension.contains("AgentHandles"), "{extension}");
    Ok(())
}

/// R3: the spawned child's `AgentToolInfra` carries the child's own
/// `agent_id` and `parent_id`, observed from within a tool the child
/// dispatches.
#[tokio::test]
async fn spawned_child_has_correct_identity() -> TestResult {
    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("identity".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let seen_agent = Arc::new(StdMutex::new(None));
    let seen_parent = Arc::new(StdMutex::new(None));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(IdentityStubTool {
        seen_agent: Arc::clone(&seen_agent),
        seen_parent: Arc::clone(&seen_parent),
    }));
    let registry = Arc::new(registry);

    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "introspect", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        *seen_agent.lock(),
        Some(child_id),
        "child tool must see the child's agent_id",
    );
    assert_eq!(
        *seen_parent.lock(),
        Some(parent),
        "child tool must see the spawning agent as parent",
    );
    Ok(())
}

/// R5: the child receives exactly the tool definitions surviving the
/// allow-list — `tools: ["read"]` while the registry has read + edit.
#[tokio::test]
async fn spawn_filters_tool_definitions_through_allow_list() -> TestResult {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(CapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "done".to_string(),
            },
            done_event(),
        ]]),
    });

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "read" }));
    registry.register(Box::new(EchoStubTool { tool_name: "edit" }));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "limited", "model": CATALOG_MODEL, "role": "worker", "tools": ["read"]}),
    )
    .await;

    let defs = captured.lock().clone();
    assert_eq!(
        defs.len(),
        1,
        "exactly one tool definition survives the allow-list"
    );
    assert!(matches!(
        defs.as_slice(),
        [ProviderToolDefinition::Function(function)] if function.name == "read"
    ));
    Ok(())
}

/// R7: when the child completes, the parent receives a
/// `ChildAgentResult` through the result channel with `succeeded: true`.
#[tokio::test]
async fn child_completion_sends_through_result_channel() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child done".to_string(),
        },
        done_event(),
    ]]));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::clone(&router),
    );
    let sender = ChildResultSender(Arc::new(tx));
    ctx.insert_extension(Arc::new(sender));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "notify me", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert!(result.succeeded, "child completed successfully");
    assert!(result.error.is_none(), "no error on success");
    assert!(
        !result.formatted_message.is_empty(),
        "formatted message must be non-empty",
    );
    Ok(())
}
