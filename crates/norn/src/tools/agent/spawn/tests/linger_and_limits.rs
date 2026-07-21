use super::*;

/// Root boundary pin (W3.5): a parent context that publishes no
/// [`AgentCancellation`] still launches children — with free-standing
/// run tokens, exactly the pre-cascade behavior — and the child's own
/// handle token remains fully functional: cancelling it ends the run
/// with the real Cancelled outcome through the wrapper.
#[tokio::test]
async fn root_without_published_token_launches_free_standing_children() -> TestResult {
    use crate::agent::output::AgentStopReason;

    let entered = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ParkedProvider {
        entered: Arc::clone(&entered),
        calls: Arc::clone(&calls),
    });
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    assert!(
        ctx.get_extension::<AgentCancellation>().is_none(),
        "this root deliberately publishes no token (the documented boundary)",
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "long haul", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    entered.notified().await;

    // The child's own (free-standing) token is the parent's control
    // surface, exactly as before the cascade landed.
    let handle = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .remove(child_id)
        .ok_or("required test value")?;
    assert!(!handle.cancel.is_cancelled());
    handle.cancel.cancel();
    handle.join_handle.await?;

    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Failed,
    );
    Ok(())
}

// -- R5 closure: per-child loop config (including linger) ---------------

/// Routes provider scripts for the mid-tree linger test. The
/// grandchild's stream is gated on the child's would-stop turn having
/// been *requested* (`child_calls` >= 2) plus a real delay, so the
/// grandchild's result arrives only after the child's model has
/// stopped — the child holds it at its stop boundary solely because
/// its granted `child_policy.loop_config.linger_secs` is in effect.
/// Distinct usage per level/call so the W3.6 rollup through the
/// lingering child is pinned numerically.
struct LingerTreeProvider {
    child_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for LingerTreeProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use std::sync::atomic::Ordering as AtomicOrdering;
        // The managed dynamic-context Developer message now rides at the
        // tail of every request (prompt-cache fix), so route on the last
        // non-Developer message — the turn content that actually seeds
        // this child.
        let last = request
            .messages
            .iter()
            .rev()
            .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        if last == "linger-grandchild-task" {
            let calls = Arc::clone(&self.child_calls);
            let s = stream::once(async move {
                // Hold the grandchild until the child's would-stop
                // turn has been requested, then add a real delay so
                // the child is parked in its linger await (not its
                // non-blocking boundary sweep) when this completes.
                for _ in 0..2400 {
                    if calls.load(AtomicOrdering::SeqCst) >= 2 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            })
            .flat_map(|()| {
                stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "grandchild late report".to_string(),
                    }),
                    Ok(done_with(StopReason::EndTurn, 7, 3)),
                ])
            });
            return Ok(Box::pin(s));
        }
        let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
        match call {
            0 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "tc-linger-grandchild".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "linger-grandchild-task",
                        "model": CATALOG_MODEL,
                        "role": "leaf",
                        // Per-spawn clearing of the inherited linger:
                        // the leaf grandchild must not linger itself,
                        // or its own (empty) boundary wait would
                        // outlast the child's deadline.
                        "child_policy": {
                            "messaging": "siblings_and_parent",
                            "delegation": {
                                "remaining_depth": 0,
                                "max_concurrent_children": 32,
                            },
                            "inbound_capacity": 32,
                        },
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_with(StopReason::ToolUse, 100, 50)),
            ]))),
            1 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "child would stop here".to_string(),
                }),
                Ok(done_with(StopReason::EndTurn, 200, 60)),
            ]))),
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "child done with grandchild result".to_string(),
                }),
                Ok(done_with(StopReason::EndTurn, 300, 70)),
            ]))),
        }
    }
}

/// R5 end-to-end, the §"Messaging × recursion" item 5 scenario that
/// was unachievable before per-child loop config: a depth-2 tree
/// where the **child** (not the root) is granted linger via the
/// per-spawn `child_policy.loop_config`, its model stops before the
/// grandchild finishes, and the lingering child drains the late
/// grandchild result, runs another turn, and delivers a complete
/// subtree to the root — with the grandchild's `subtree_usage` rolled
/// up through the lingering child (W3.6).
#[tokio::test]
async fn mid_tree_child_granted_linger_drains_late_grandchild_result() -> TestResult {
    use crate::agent::child_policy::ChildLoopConfig;

    let agent_registry = AgentRegistry::shared();
    let provider: Arc<dyn Provider> = Arc::new(LingerTreeProvider {
        child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    let root_id = Uuid::new_v4();
    // Persistent parent so the child's conversation is readable from
    // its on-disk timeline for the injected-frame assertion.
    let tmp = tempfile::tempdir()?;
    let (ctx, manager, root_session_id) = persistent_parent_ctx(
        tmp.path(),
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
    )?;
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    // The mid-tree child is granted a linger through the per-spawn
    // child_policy argument — the exact surface R5 adds.
    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "task": "child-task", "model": CATALOG_MODEL, "role": "lead",
                "child_policy": {
                    "messaging": "siblings_and_parent",
                    "delegation": {
                        "remaining_depth": 1,
                        "max_concurrent_children": 32,
                    },
                    "inbound_capacity": 32,
                    "loop_config": { "linger_secs": 2 },
                },
            })),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    // The granted loop_config is registry ground truth on the
    // child's entry (the `agents` tool renders this policy).
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .policy
            .loop_config,
        Some(ChildLoopConfig {
            step_timeout_secs: None,
            linger_secs: Some(2),
            context_window: None,
        }),
    );

    // The complete subtree reached the root as exactly one result:
    // the child's final answer, produced *after* the late grandchild
    // result was drained at the lingering stop boundary.
    let child_result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(child_result.agent_id, child_id);
    assert!(child_result.succeeded, "{:?}", child_result.error);
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    assert!(
        child_result
            .formatted_message
            .contains("child done with grandchild result"),
        "the child's post-linger turn is the delivered result: {}",
        child_result.formatted_message,
    );
    assert!(
        rx.try_recv().is_err(),
        "the grandchild's result must never reach the root directly",
    );

    // W3.6 rollup through the lingering child: own usage is the
    // child's three provider calls; subtree adds the grandchild's.
    assert_eq!(child_result.usage.input_tokens, 600, "100+200+300 own");
    assert_eq!(child_result.usage.output_tokens, 180, "50+60+70 own");
    assert_eq!(
        child_result.subtree_usage.input_tokens, 607,
        "own + the lingered-for grandchild subtree (7)",
    );
    assert_eq!(child_result.subtree_usage.output_tokens, 183);

    // The grandchild's framed result was injected into the *child's*
    // conversation through the normal drain path — read back from the
    // child's ON-DISK timeline.
    let rows = crate::session::persistence::index::read_index(manager.data_dir())?;
    let child_row = rows
        .iter()
        .find(|r| r.parent_id.as_deref() == Some(root_session_id.as_str()))
        .ok_or("required test value")?;
    let child_events = events_on_disk(&manager, &child_row.id);
    let injected = child_events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content.contains("<agent_result")
                    && content.contains("grandchild late report")
        )
    });
    assert!(
        injected,
        "the late grandchild result must be injected into the lingering child's conversation",
    );
    Ok(())
}

/// DECISIONS §0.6(c): the model-suppliable `loop_config.max_iterations`
/// grant is removed. It is absent from the schema and, because
/// `loop_config` is `deny_unknown_fields`, a spawn that still passes it
/// is rejected loudly at the argument boundary — never silently dropped
/// (a silent failure), and nothing is reserved.
#[tokio::test]
async fn spawn_rejects_removed_max_iterations_grant() -> TestResult {
    // The schema no longer advertises the knob under loop_config.
    let tool = SpawnAgentTool::new();
    let loop_config = &tool.input_schema()["properties"]["child_policy"]["properties"]["loop_config"]
        ["properties"];
    assert!(
        loop_config.get("max_iterations").is_none(),
        "max_iterations must be absent from the loop_config schema: {loop_config:?}",
    );
    assert!(
        loop_config.get("step_timeout_secs").is_some() && loop_config.get("linger_secs").is_some(),
        "the surviving loop-shaping knobs stay advertised: {loop_config:?}",
    );

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let result = tool
        .execute(
            &envelope_for(json!({
                "task": "capped", "model": CATALOG_MODEL, "role": "worker",
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
    let Err(ToolError::ExecutionFailed { reason }) = result else {
        return Err(format!("expected ExecutionFailed, got {result:?}").into());
    };
    assert!(
        reason.contains("max_iterations"),
        "the failure names the removed field: {reason}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "a refused spawn reserves nothing",
    );
    Ok(())
}
