use super::*;

/// The spawned child's `AgentToolInfra` carries the granted policy and
/// the scope-granting parent's event store — the ground truth
/// `signal_agent` enforces scope from and writes the dual-store audit
/// to.
#[tokio::test]
async fn spawned_child_infra_carries_granted_policy_and_parent_store() -> TestResult {
    struct PolicyProbe {
        seen_scope: Arc<StdMutex<Option<MessagingScope>>>,
        seen_capacity: Arc<StdMutex<Option<usize>>>,
        parent_store_matches: Arc<StdMutex<Option<bool>>>,
        parent_store: Arc<EventStore>,
    }

    #[async_trait]
    impl TestTool for PolicyProbe {
        fn name(&self) -> &'static str {
            "policy_probe"
        }
        fn description(&self) -> &'static str {
            "records the granted policy it sees"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            if let Some(infra) = ctx.get_extension::<AgentToolInfra>() {
                *self.seen_scope.lock() = infra.grant.as_ref().map(|g| g.policy.messaging);
                *self.seen_capacity.lock() =
                    infra.grant.as_ref().map(|g| g.policy.inbound_capacity);
                *self.parent_store_matches.lock() = Some(
                    infra
                        .grant
                        .as_ref()
                        .is_some_and(|g| Arc::ptr_eq(&g.parent_store, &self.parent_store)),
                );
            }
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("policy_probe".to_string()),
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

    let agent_registry = AgentRegistry::shared();
    let parent = Uuid::new_v4();
    let seen_scope = Arc::new(StdMutex::new(None));
    let seen_capacity = Arc::new(StdMutex::new(None));
    let parent_store_matches = Arc::new(StdMutex::new(None));

    // Build the parent ctx first so its infra's event store is the
    // store the probe compares against.
    let ctx = {
        let parent_event_store = Arc::new(EventStore::new());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PolicyProbe {
            seen_scope: Arc::clone(&seen_scope),
            seen_capacity: Arc::clone(&seen_capacity),
            parent_store_matches: Arc::clone(&parent_store_matches),
            parent_store: Arc::clone(&parent_event_store),
        }));
        let infra = Arc::new(AgentToolInfra {
            registry: Arc::clone(&agent_registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: parent_event_store,
            agent_id: parent,
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(registry)),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        });
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx.insert_extension(Arc::new(AgentHandles::new()));
        ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
        let mut envelope = test_envelope();
        envelope.child_policy.messaging = MessagingScope::ParentOnly;
        envelope.child_policy.inbound_capacity = 7;
        ctx.insert_extension(Arc::new(envelope));
        ctx
    };

    let tool = SpawnAgentTool::new();
    spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "introspect", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        *seen_scope.lock(),
        Some(MessagingScope::ParentOnly),
        "the child must carry the envelope's granted messaging scope",
    );
    assert_eq!(
        *seen_capacity.lock(),
        Some(7),
        "the child must carry the envelope's inbound capacity",
    );
    assert_eq!(
        *parent_store_matches.lock(),
        Some(true),
        "the child's parent_store must be the spawning parent's event store",
    );
    Ok(())
}

// -- W3.4: budgeted recursive delegation --------------------------------

/// A caller whose own granted budget has `remaining_depth = 0` may not
/// spawn at all: typed, honest refusal naming the budget, and nothing
/// is reserved.
#[tokio::test]
async fn spawn_refused_when_caller_depth_exhausted() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 0;
    ctx.insert_extension(Arc::new(envelope));

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
        reason.contains("delegation depth exhausted"),
        "the refusal names the budget: {reason}",
    );
    assert!(
        reason.contains("remaining_depth = 0"),
        "the refusal states the budget value: {reason}",
    );
    let reg = agent_registry.read();
    assert!(reg.is_empty(), "a refused spawn reserves nothing");
    assert!(reg.tombstones().is_empty(), "and leaves no tombstone");
    Ok(())
}

/// A `child_policy` argument that widens the caller's own grant is
/// refused typed (per field), naming the caller's budget; a valid
/// narrowing is stamped on the registry entry verbatim.
/// A typo'd top-level key must fail loudly — silently dropping a
/// misspelled `child_policy` would hand the child a default grant
/// where the caller intended a narrowing.
#[tokio::test]
async fn spawn_rejects_unknown_arg_keys() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(test_envelope()));
    let tool = SpawnAgentTool::new();

    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "child_polciy": { "inbound_capacity": 32 },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("a typo'd key must not be silently dropped".into());
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("child_polciy") || rendered.contains("unknown field"),
        "the failure names the unknown key: {rendered}",
    );
    Ok(())
}

/// U2-M1 regression: an `output_schema` declaring a reserved envelope
/// key as a top-level property is refused synchronously at the
/// argument boundary — required collisions would make the child's
/// schema unsatisfiable (the key is stripped before validation) and
/// optional ones silently lossy. The failure names the key.
#[tokio::test]
async fn spawn_rejects_output_schema_with_reserved_envelope_key() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(test_envelope()));
    let tool = SpawnAgentTool::new();

    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "output_schema": {
                    "type": "object",
                    "properties": {
                        "answer": { "type": "string" },
                        "tool_use_description": { "type": "string" }
                    },
                    "required": ["answer", "tool_use_description"],
                    "additionalProperties": false
                },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err("a reserved-key schema must be refused, not silently mangled".into());
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("tool_use_description") && rendered.contains("reserved"),
        "the failure names the colliding key and the convention: {rendered}",
    );
    Ok(())
}

#[tokio::test]
async fn spawn_child_policy_narrowing_enforced() -> TestResult {
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
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));
    let tool = SpawnAgentTool::new();

    // Depth widened (equal to the caller's own — not strictly less).
    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {"remaining_depth": 2, "max_concurrent_children": 32},
                    "inbound_capacity": 32,
                },
            })),
            &ctx,
        )
        .await;
    let Err(ToolError::ExecutionFailed { reason }) = result else {
        return Err(format!("expected ExecutionFailed, got {result:?}").into());
    };
    assert!(
        reason.contains("remaining_depth = 2 exceeds") && reason.contains("at most 1"),
        "names the strict decrement: {reason}",
    );

    // Inbound capacity widened.
    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "x", "model": CATALOG_MODEL, "role": "worker",
                "child_policy": {
                    "messaging": "parent_only",
                    "delegation": {"remaining_depth": 0, "max_concurrent_children": 1},
                    "inbound_capacity": 33,
                },
            })),
            &ctx,
        )
        .await;
    let Err(ToolError::ExecutionFailed { reason }) = result else {
        return Err(format!("expected ExecutionFailed, got {result:?}").into());
    };
    assert!(
        reason.contains("inbound_capacity = 33 exceeds"),
        "names the violation: {reason}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "refused narrowings reserve nothing",
    );

    // A valid narrowing is accepted and stamped verbatim.
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({
            "task": "x", "model": CATALOG_MODEL, "role": "worker",
            "child_policy": {
                "messaging": "parent_only",
                "delegation": {"remaining_depth": 1, "max_concurrent_children": 2},
                "inbound_capacity": 8,
            },
        }),
    )
    .await;
    let entry = agent_registry
        .read()
        .get(child_id)
        .ok_or("required test value")?;
    assert_eq!(entry.policy.messaging, MessagingScope::ParentOnly);
    assert_eq!(entry.policy.delegation.remaining_depth, 1);
    assert_eq!(entry.policy.delegation.max_concurrent_children, 2);
    assert_eq!(entry.policy.inbound_capacity, 8);
    Ok(())
}
