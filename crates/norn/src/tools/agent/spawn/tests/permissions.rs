use super::*;

/// Permission-escape regression (blocker): the consent-boundary
/// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must be
/// forwarded from the parent's context into the child's context —
/// the child loop resolves both from its own executor's shared
/// context, so a missing forward disables enforcement entirely.
#[tokio::test]
async fn child_context_forwards_permission_policy_and_effect_index() -> TestResult {
    use crate::tool::scheduling::ToolEffectIndex;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let infra = AgentToolInfra {
        registry: AgentRegistry::shared(),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    };
    let parent_ctx = ToolContext::empty();
    let policy = Arc::new(crate::config::permissions::PermissionPolicy::from_patterns(
        &["bash"],
        &[],
        &[],
    ));
    let effects = Arc::new(ToolEffectIndex::new());
    parent_ctx.insert_extension(Arc::clone(&policy));
    parent_ctx.insert_extension(Arc::clone(&effects));

    let child_ctx = build_child_context(
        &infra,
        Uuid::new_v4(),
        Arc::new(EventStore::new()),
        &parent_ctx,
        Arc::new(crate::session::SessionBinding::ephemeral_root()),
        test_envelope().child_policy,
        tokio_util::sync::CancellationToken::new(),
    );

    let forwarded_policy = child_ctx
        .get_extension::<crate::config::permissions::PermissionPolicy>()
        .ok_or("required test value")?;
    assert!(
        Arc::ptr_eq(&forwarded_policy, &policy),
        "the child must share the parent's policy instance",
    );
    let forwarded_effects = child_ctx
        .get_extension::<ToolEffectIndex>()
        .ok_or("required test value")?;
    assert!(
        Arc::ptr_eq(&forwarded_effects, &effects),
        "the child must share the parent's effect index instance",
    );
    Ok(())
}

/// Permission-escape regression (blocker), end to end: a tool denied
/// by the parent's policy must stay denied inside a spawned child —
/// the child model calls it, dispatch blocks it, and the tool body
/// never executes.
#[tokio::test]
async fn denied_tool_stays_denied_inside_spawned_child() -> TestResult {
    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("victim".to_string()),
            arguments_delta: r#"{"command": "rm -rf /"}"#.to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "gave up".to_string(),
        },
        done_event(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CountingStubTool {
        tool_name: "victim",
        executions: Arc::clone(&executions),
    }));
    let registry = Arc::new(registry);

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        registry,
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(
        crate::config::permissions::PermissionPolicy::from_patterns(&["victim"], &[], &[]),
    ));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "try the denied tool", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a tool denied in the parent must never execute inside a spawned child",
    );
    // The child itself still finishes its step and parks idle (the deny
    // surfaces as a blocked tool result, not a child crash).
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    Ok(())
}

/// R2: a child tool call carrying a `tool_use_description` envelope field
/// is recorded verbatim in the child's [`EventStore`] on the
/// `AssistantMessage` event — the runner captures the full raw arguments
/// JSON before envelope fields are stripped — so the parent can read it
/// straight from the handle's event store.
#[tokio::test]
async fn child_tool_use_description_recorded_in_event_store() -> TestResult {
    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("probe".to_string()),
            arguments_delta: r#"{"tool_use_description":"inspecting the config"}"#.to_string(),
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

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoStubTool { tool_name: "probe" }));
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
    let out = tool
        .execute(
            &envelope_for(json!({"task": "probe it", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    let event_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;

    let events = event_store.events();
    let found = events.iter().any(|e| match e {
        SessionEvent::AssistantMessage { tool_calls, .. } => tool_calls.iter().any(|tc| {
            tc.arguments
                .get("tool_use_description")
                .and_then(serde_json::Value::as_str)
                == Some("inspecting the config")
        }),
        _ => false,
    });
    assert!(
        found,
        "tool_use_description must be recorded in the child's EventStore: {events:?}",
    );
    Ok(())
}

/// R3 (ephemeral parent): under an ephemeral parent the child's events
/// are still reachable through `AgentHandle.event_store`, and the
/// [`AgentHandles`] accessors expose the store, the provenance
/// metadata, and the child id.
#[tokio::test]
async fn child_event_store_accessible_via_agent_handle() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child output".to_string(),
        },
        done_event(),
    ]]));
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
            &envelope_for(
                json!({"task": "produce events", "model": CATALOG_MODEL, "role": "worker"}),
            ),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;

    let store_via_accessor = handles.event_store(child_id).ok_or("required test value")?;
    assert_eq!(handles.list_children(), vec![child_id]);
    let meta = handles
        .branch_metadata(child_id)
        .ok_or("required test value")?;
    assert_eq!(meta.child_agent_id, child_id);
    assert_eq!(meta.parent_agent_id, parent);
    assert!(meta.profile_name.is_none());

    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

    assert!(
        !store_via_accessor.is_empty(),
        "the child produced events the parent can read through the handle",
    );
    Ok(())
}
