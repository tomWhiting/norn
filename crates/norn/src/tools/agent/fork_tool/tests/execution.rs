use super::*;

/// R1: `fork.execute()` returns immediately while the child is still
/// blocked behind a gated provider.
#[tokio::test]
async fn fork_returns_immediately_then_child_runs_async() -> TestResult {
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
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let started = std::time::Instant::now();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "summarise", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "fork must return within 50ms while child is gated; took {elapsed:?}",
    );
    assert_eq!(out.content["status"], "active");
    let fork_id = fork_id_from(&out)?;

    let active_entry = required(agent_registry.read().get(fork_id), "active fork entry")?;
    assert_eq!(active_entry.status, AgentStatus::Active);

    gate.notify_one();
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let mut status_rx = handle.status_rx.clone();
    handle.join_handle.await?;
    // Terminal transition retains the entry (status displays hold it)
    // with terminal status; the watch channel carries it too.
    let completed_entry = required(
        agent_registry.read().get(fork_id),
        "completed fork entry must stay observable until reclaimed",
    )?;
    assert_eq!(completed_entry.status, AgentStatus::Completed);
    assert_eq!(*status_rx.borrow_and_update(), AgentStatus::Completed);
    Ok(())
}

/// NH-006 R5 parity with spawn: `SubagentHook::on_subagent_start`
/// fires before the fork launches and
/// `SubagentHook::on_subagent_stop` fires from the fork's wrapper
/// task once the run finishes — the pre-existing asymmetry (spawn
/// fired both, fork fired neither) is closed.
#[tokio::test]
async fn subagent_hook_start_and_stop_fire_around_fork() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

    struct CountingSubagentHook {
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
        seen_type: Arc<StdMutex<Option<String>>>,
    }

    #[async_trait]
    impl SubagentHook for CountingSubagentHook {
        async fn on_subagent_start(&self, _agent_id: &str, agent_type: &str) {
            self.start_count.fetch_add(1, AtomicOrdering::SeqCst);
            *self.seen_type.lock() = Some(agent_type.to_owned());
        }
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            self.stop_count.fetch_add(1, AtomicOrdering::SeqCst);
            HookOutcome::Proceed
        }
    }

    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let start_count = Arc::new(AtomicUsize::new(0));
    let stop_count = Arc::new(AtomicUsize::new(0));
    let seen_type = Arc::new(StdMutex::new(None));
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::Subagent(Box::new(CountingSubagentHook {
        start_count: Arc::clone(&start_count),
        stop_count: Arc::clone(&stop_count),
        seen_type: Arc::clone(&seen_type),
    })));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "summarise", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    assert_eq!(
        start_count.load(AtomicOrdering::SeqCst),
        1,
        "SubagentHook::on_subagent_start must fire exactly once per fork",
    );
    assert_eq!(
        stop_count.load(AtomicOrdering::SeqCst),
        1,
        "SubagentHook::on_subagent_stop must fire exactly once per fork",
    );
    assert_eq!(
        seen_type.lock().as_deref(),
        Some("fork"),
        "the hook matcher input for forks is the literal role 'fork'",
    );
    Ok(())
}

/// R2: fork running mid-turn — the parent's latest `AssistantMessage`
/// has multiple `tool_calls`. The child store carries a synthetic
/// `ToolResult` with `tool_name == "fork"` matching the fork's
/// `tool_call_id`, and every other `tool_call` has a matching result.
#[tokio::test]
async fn fork_injects_synthetic_tool_result_for_orphan_fork_call() -> TestResult {
    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    parent_store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "go".to_string(),
    })?;
    parent_store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "running batch".to_string(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![
            ToolCallEvent {
                call_id: "tc-read".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            },
            ToolCallEvent {
                call_id: "tc-search".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            },
            ToolCallEvent {
                call_id: "call-1".to_string(),
                name: "fork".to_string(),
                arguments: serde_json::json!({}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            },
        ],
        usage: EventUsage::default(),
        stop_reason: String::new(),
        response_id: None,
    })?;
    parent_store.append(SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "tc-read".to_string(),
        tool_name: "read".to_string(),
        output: serde_json::json!({"content": "x"}),
        spool_ref: None,
        duration_ms: 1,
    })?;
    parent_store.append(SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "tc-search".to_string(),
        tool_name: "search".to_string(),
        output: serde_json::json!({"hits": []}),
        spool_ref: None,
        duration_ms: 1,
    })?;

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "summarise", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;

    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let events = child_store.events();
    let synthetic = events.iter().find(|e| {
        matches!(
            e,
            SessionEvent::ToolResult {
                tool_call_id,
                tool_name,
                ..
            } if tool_call_id == "call-1" && tool_name == "fork"
        )
    });
    assert!(
        synthetic.is_some(),
        "synthetic ToolResult with tool_name == 'fork' must be present",
    );

    let seeded_assistant = events.iter().rposition(|e| {
        matches!(
            e,
            SessionEvent::AssistantMessage { tool_calls, .. }
                if tool_calls.iter().any(|tc| tc.call_id == "call-1")
        )
    });
    let index = required(seeded_assistant, "seeded assistant event")?;
    let SessionEvent::AssistantMessage { tool_calls, .. } = &events[index] else {
        return Err(test_error("seeded event must be an assistant message"));
    };
    for tc in tool_calls {
        let has_result = events.iter().any(|e| {
            matches!(
                e,
                SessionEvent::ToolResult { tool_call_id, .. } if tool_call_id == &tc.call_id
            )
        });
        assert!(
            has_result,
            "tool_call {} must have a matching ToolResult in child seed events",
            tc.call_id,
        );
    }
    Ok(())
}
