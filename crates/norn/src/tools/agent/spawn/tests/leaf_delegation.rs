use super::*;

/// Omitting `child_policy` grants the caller's own policy with the
/// delegation depth decremented one level, and the auto path nests
/// under the spawning agent's registered path.
#[tokio::test]
async fn spawn_stamps_decremented_grant_and_nested_path() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;

    // Register the spawner itself first (the CLI does this for its
    // root) so the auto path has a prefix to nest under, then key the
    // spawning context to the registered id.
    let guard = AgentRegistry::reserve(
        &agent_registry,
        "/lead".to_string(),
        "lead".to_string(),
        "opus".to_string(),
        None,
        envelope.child_policy.clone(),
        None,
    )?;
    let registered_parent = guard.id();
    guard.confirm()?;

    let ctx = parent_ctx(
        provider,
        registered_parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(envelope.clone()));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "x", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let entry = agent_registry
        .read()
        .get(child_id)
        .ok_or("required test value")?;
    assert!(
        entry.path.starts_with("/lead/spawn/"),
        "auto path nests under the spawner: {}",
        entry.path,
    );
    assert_eq!(entry.parent_id, Some(registered_parent));
    assert_eq!(
        entry.policy.delegation.remaining_depth, 1,
        "the default derivation decrements the caller's depth 2 to 1",
    );
    assert_eq!(entry.policy.messaging, envelope.child_policy.messaging);
    assert_eq!(
        entry.policy.delegation.max_concurrent_children,
        envelope.child_policy.delegation.max_concurrent_children,
    );
    assert_eq!(
        entry.policy.inbound_capacity,
        envelope.child_policy.inbound_capacity,
    );
    Ok(())
}

/// A leaf child (granted depth 0) that tries to spawn is refused by
/// the registry budget with the typed message, the grandchild is never
/// registered, and the child still completes normally.
#[tokio::test]
async fn leaf_child_spawn_attempt_refused_and_run_completes() -> TestResult {
    // Child script: call spawn_agent (refused — the tool error is
    // injected as the tool result), then finish.
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc1".to_string(),
                call_id: None,
                name: Some("spawn_agent".to_string()),
                arguments_delta: json!({
                    "task": "grandchild", "model": CATALOG_MODEL, "role": "leaf",
                })
                .to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "stopping at my budget".to_string(),
            },
            done_event(),
        ],
    ]));
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    // Envelope depth 1: the child is a leaf (granted depth 0).
    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "try to delegate", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let reg = agent_registry.read();
    let entry = reg.get(child_id).ok_or("required test value")?;
    assert_eq!(
        entry.status,
        AgentStatus::Idle,
        "the child completed and idled"
    );
    assert_eq!(entry.policy.delegation.remaining_depth, 0, "leaf grant");
    assert_eq!(
        reg.len(),
        1,
        "the grandchild must never be registered: {:?}",
        reg.list(),
    );
    assert!(reg.tombstones().is_empty(), "nothing was reclaimed");
    Ok(())
}
