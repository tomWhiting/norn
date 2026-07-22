use super::*;

/// Route ownership (W3.2): the fork launch registers the inbound
/// route before the task starts and the completion wrapper
/// deregisters at the run's end.
#[tokio::test]
async fn fork_registers_route_at_launch_and_deregisters_at_terminal() -> TestResult {
    let gate = Arc::new(tokio::sync::Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
        gate: Arc::clone(&gate),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ]]),
    });
    let agent_registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::clone(&router),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "wait", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    assert!(
        router.is_routed(fork_id),
        "the launch path must register the fork's inbound route",
    );

    gate.notify_one();
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;
    assert!(
        !router.is_routed(fork_id),
        "the completion wrapper must deregister the route at the run's end",
    );
    Ok(())
}

/// Missing-envelope boundary: a context that can fork but carries no
/// [`CoordinationEnvelope`] is a wiring error — fork refuses with a
/// typed `MissingExtension` naming the envelope, leaking no
/// reservation.
#[tokio::test]
async fn fork_requires_coordination_envelope() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&agent_registry),
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
    ctx.insert_extension(Arc::new(AgentHandles::new()));

    let tool = ForkTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({"request": "x", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(test_error("fork without an envelope must fail typed"));
    };
    match err {
        ToolError::MissingExtension { extension } => {
            assert!(
                extension.contains("CoordinationEnvelope"),
                "error must name the missing envelope: {extension}",
            );
        }
        other => {
            return Err(test_error(format!(
                "expected MissingExtension, got {other:?}"
            )));
        }
    }
    assert!(
        agent_registry.read().list().is_empty(),
        "no reservation may leak from the refused fork",
    );
    Ok(())
}

// -- W3.4: budgeted recursive delegation (fork side) ---------------------

/// A `child_policy` argument that widens the caller's grant is refused
/// typed (nothing reserved); omitting it stamps the caller's policy
/// with the delegation depth decremented one level, and the fork path
/// nests under the spawner.
/// A typo'd top-level key must fail loudly — silently dropping a
/// misspelled `child_policy` would hand the fork a default grant
/// where the caller intended a narrowing. Mirrors the spawn-side
/// pin so `ForkArgs`' `deny_unknown_fields` cannot regress silently.
#[tokio::test]
async fn fork_rejects_unknown_arg_keys() -> TestResult {
    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(test_envelope()));
    let tool = ForkTool::new();

    let result = tool
        .execute(
            &envelope_for(json!({
                "request": "r", "model": "gpt-5.5", "requirements": [],
                "child_polciy": { "inbound_capacity": 32 },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(test_error("a typo'd key must not be silently dropped"));
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("child_polciy") || rendered.contains("unknown field"),
        "the failure names the unknown key: {rendered}",
    );
    Ok(())
}

#[tokio::test]
async fn fork_stamps_decremented_grant_and_refuses_widening() -> TestResult {
    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope.clone()));
    let tool = ForkTool::new();

    let result = tool
        .execute(
            &envelope_for(json!({
                "request": "r", "model": "gpt-5.5", "requirements": [],
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {"remaining_depth": 2, "max_concurrent_children": 32},
                    "inbound_capacity": 32,
                },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(test_error("widening must be refused"));
    };
    match err {
        ToolError::ExecutionFailed { reason } => {
            assert!(
                reason.contains("remaining_depth = 2 exceeds"),
                "names the caller's budget: {reason}",
            );
        }
        other => {
            return Err(test_error(format!(
                "expected ExecutionFailed, got {other:?}"
            )));
        }
    }
    assert!(
        agent_registry.read().is_empty(),
        "a refused fork reserves nothing",
    );

    let out = tool
        .execute(
            &envelope_for(json!({"request": "r", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let fork_id = fork_id_from(&out)?;
    let path = required(
        out.content.get("path").and_then(serde_json::Value::as_str),
        "path",
    )?;
    assert!(path.starts_with("/fork/"), "{path}");
    let entry = required(agent_registry.read().get(fork_id), "fork registry entry")?;
    assert_eq!(
        entry.policy.delegation.remaining_depth, 1,
        "default derivation decrements the caller's depth 2 to 1",
    );
    assert_eq!(entry.policy.messaging, envelope.child_policy.messaging);
    assert_eq!(
        entry.policy.inbound_capacity,
        envelope.child_policy.inbound_capacity,
    );
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;
    Ok(())
}

/// DECISIONS §0.6(c) on the fork surface: the model-suppliable
/// `loop_config.max_iterations` grant is removed. It is absent from the
/// fork schema and, because `loop_config` is `deny_unknown_fields`, a
/// fork that still passes it is rejected loudly at the argument
/// boundary — never silently dropped, and nothing is reserved.
#[tokio::test]
async fn fork_rejects_removed_max_iterations_grant() -> TestResult {
    use crate::agent::result_channel::ChildResultSender;

    // The fork schema no longer advertises the knob under loop_config.
    let tool = ForkTool::new();
    let loop_config = &tool.input_schema()["properties"]["child_policy"]["properties"]["loop_config"]
        ["properties"];
    assert!(
        loop_config.get("max_iterations").is_none(),
        "max_iterations must be absent from the fork loop_config schema: {loop_config:?}",
    );

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let result = tool
        .execute(
            &envelope_for(json!({
                "request": "r", "model": "gpt-5.5", "requirements": [],
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {
                        "remaining_depth": 0,
                        "max_concurrent_children": 32,
                    },
                    "inbound_capacity": 32,
                    "loop_config": { "max_iterations": 1 },
                },
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(test_error(
            "the removed max_iterations grant must fail loudly",
        ));
    };
    match err {
        ToolError::ExecutionFailed { reason } => {
            assert!(
                reason.contains("max_iterations"),
                "the failure names the removed field: {reason}",
            );
        }
        other => {
            return Err(test_error(format!(
                "expected ExecutionFailed, got {other:?}"
            )));
        }
    }
    assert!(
        agent_registry.read().is_empty(),
        "a refused fork reserves nothing",
    );
    Ok(())
}
