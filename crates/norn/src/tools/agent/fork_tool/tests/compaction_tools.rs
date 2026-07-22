use super::*;

/// Hardening (owner ruling 2026-07-03), full trigger: a fork that
/// inherits a long parent history and then reports an oversized usage
/// must actually *compact* mid-run rather than die
/// `ContextWindowExceeded`. The parent is seeded with more than
/// `auto_compact_keep_recent_turns` (default 10) turns, the fork
/// inherits them, and the first turn's oversized usage pushes the
/// second preflight past the compaction threshold — proving the shared
/// arming makes the trigger genuinely fire for a child, not merely warn.
#[tokio::test]
async fn fork_runs_auto_compaction_when_history_exceeds_window() -> TestResult {
    let catalog_model = crate::model_catalog::default_selection().model;
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(IdentityStubTool {
        seen_agent: Arc::new(StdMutex::new(None)),
        seen_parent: Arc::new(StdMutex::new(None)),
    }));
    let tool_registry = Arc::new(tool_registry);

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        // Turn 1: tool call + oversized usage (sets the floor).
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-id".to_string(),
                call_id: None,
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100_000_000,
                    output_tokens: 0,
                    ..Usage::default()
                },
                response_id: None,
            },
        ],
        // The compaction summarization provider call (fired inside the
        // second preflight, before the second main turn).
        vec![
            ProviderEvent::TextDelta {
                text: "summary of earlier turns".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ],
        // Turn 2: structured output so the fork completes.
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
    ]));
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );

    // Seed 12 user/assistant turns so the fork inherits more than the
    // default keep_recent_turns (10) — giving the compaction plan
    // something to elide.
    for i in 0..12 {
        parent_store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("q{i}"),
        })?;
        parent_store.append(SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: format!("a{i}"),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        })?;
    }

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "run", "model": catalog_model, "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let fork_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let compacted = fork_store
        .events()
        .iter()
        .any(|e| matches!(e, SessionEvent::Compaction { .. }));
    assert!(
        compacted,
        "the fork must commit a Compaction event when its inherited \
         history and oversized usage cross the threshold",
    );
    Ok(())
}

/// Permission-escape regression (blocker), end to end: a tool denied
/// by the parent's policy must stay denied inside a fork — the fork
/// model calls it, dispatch blocks it, and the tool body never runs.
#[tokio::test]
async fn denied_tool_stays_denied_inside_fork() -> TestResult {
    struct CountingStubTool {
        executions: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl TestTool for CountingStubTool {
        fn name(&self) -> &'static str {
            "victim"
        }
        fn description(&self) -> &'static str {
            "counts executions"
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
            _ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            self.executions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

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
        ProviderEvent::ToolCallDelta {
            item_id: "structured-out".to_string(),
            call_id: None,
            name: Some("structured_output".to_string()),
            arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(CountingStubTool {
        executions: Arc::clone(&executions),
    }));
    let tool_registry = Arc::new(tool_registry);

    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(
        crate::config::permissions::PermissionPolicy::from_patterns(&["victim"], &[], &[]),
    ));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(
                json!({"request": "try the denied tool", "model": "gpt-5.5", "requirements": []}),
            ),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    assert_eq!(
        executions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a tool denied in the parent must never execute inside a fork",
    );
    Ok(())
}

/// R1 failure path: empty provider yields a run error — registry is
/// marked `Failed` and the parent receives a failure result through the
/// child result channel.
#[tokio::test]
async fn fork_failure_marks_failed_and_sends_result() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let sender = ChildResultSender(Arc::new(tx));
    ctx.insert_extension(Arc::new(sender));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "will-fail", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    // Terminal transition retains the entry with Failed status; the
    // result channel carries the failure.
    let failed_entry = required(
        agent_registry.read().get(fork_id),
        "failed fork entry must stay observable until reclaimed",
    )?;
    assert_eq!(failed_entry.status, AgentStatus::Failed);
    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, fork_id);
    assert!(!result.succeeded, "fork must report failure");
    assert!(result.error.is_some(), "error message present on failure");
    Ok(())
}

/// Builds a provider turn carrying a single `read` tool call.
fn read_call_turn(item_id: &str, path: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallDelta {
            item_id: item_id.to_string(),
            call_id: None,
            name: Some("read".to_string()),
            arguments_delta: json!({ "path": path }).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ]
}

/// Confinement-escape regression (blocker), end to end: a parent
/// confined to a workspace root forks a child; the fork's `read` of
/// an out-of-root file is REFUSED while an in-root read works.
#[tokio::test]
async fn forked_child_file_tools_respect_parent_confinement() -> TestResult {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let in_path = root.path().join("inside.txt");
    std::fs::write(&in_path, "inside-content")?;
    let out_path = outside.path().join("secret.txt");
    std::fs::write(&out_path, "secret-content")?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        read_call_turn("tc-out", &out_path.to_string_lossy()),
        read_call_turn("tc-in", &in_path.to_string_lossy()),
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
    ]));

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(crate::tools::read::ReadTool::new()));
    let tool_registry = Arc::new(tool_registry);

    let agent_registry = AgentRegistry::shared();
    let (mut ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );
    ctx.confine_to_workspace(root.path().to_path_buf());

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "read files", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let results: Vec<serde_json::Value> = child_store
        .events()
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } if tool_name == "read" => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "both reads produced results: {results:?}");
    assert_eq!(
        results[0]["kind"], "confinement_refused",
        "the out-of-root read must be refused inside the fork: {}",
        results[0],
    );
    assert_eq!(
        results[1]["kind"], "text",
        "the in-root read must succeed inside the fork: {}",
        results[1],
    );
    let content = required(results[1]["content"].as_str(), "read result text content")?;
    assert!(
        content.contains("inside-content"),
        "the in-root read must return the file content: {}",
        results[1],
    );
    Ok(())
}
