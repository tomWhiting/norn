use super::*;

/// Close-with-handle determinism: the closer triggers the fork's
/// cooperative cancellation token and JOINS the wrapper before
/// touching the registry, so exactly one owner performs the terminal
/// transition — the wrapper itself, with the run's REAL outcome. With
/// the wrapper parked pre-mark (gated stop hook) after its run
/// already completed, the close waits for the wrapper (gate released
/// concurrently), the wrapper's own mark lands exactly once
/// (`Completed` — the run genuinely finished), and the closer's job
/// reduces to reclaim: the entry is gone, a `Completed` tombstone
/// preserves the real outcome, the handle is owned by the closer,
/// and no "agent not found" race is possible. The closer never
/// aborts the wrapper and never rewrites the recorded outcome.
#[tokio::test]
async fn close_with_handle_joins_wrapper_then_owns_terminal_transition() -> TestResult {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};
    use crate::tools::agent::coord::CloseAgentTool;

    struct GateStopHook {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl SubagentHook for GateStopHook {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            self.entered.notify_one();
            self.release.notified().await;
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

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::Subagent(Box::new(GateStopHook {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    })));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "race", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    entered.notified().await;

    // The parent itself closes the fork: it holds the handle, so the
    // close cancels the run (already finished here), then JOINS the
    // parked wrapper. The join waits for the wrapper's own terminal
    // sequence, so the gate must be released concurrently —
    // `notify_one` stores a permit, making the join/release ordering
    // race-free.
    let close_tool = CloseAgentTool::new();
    let close_envelope = ToolEnvelope {
        tool_call_id: "close-1".to_string(),
        tool_name: "close_agent".to_string(),
        model_args: json!({"agent_id": fork_id.to_string(), "reason": "wrap up"}),
        metadata: serde_json::Value::Null,
    };
    let close_fut = close_tool.execute(&close_envelope, &ctx);
    let release_fut = async {
        release.notify_one();
    };
    let (shutdown_result, ()) = tokio::join!(close_fut, release_fut);
    let shutdown_output = shutdown_result?;
    assert_eq!(
        shutdown_output.content["shut_down"][0]["status"], "reclaimed",
        "the joined wrapper records the run's real outcome itself; the \
         closer's job is reclaim-only: {:?}",
        shutdown_output.content,
    );

    let reg = agent_registry.read();
    assert!(reg.get(fork_id).is_none(), "the closer reclaims the entry");
    let tombstone = required(
        reg.tombstone(fork_id),
        "the recorded outcome must stay reportable via its tombstone",
    )?;
    assert_eq!(
        tombstone.status,
        AgentStatus::Completed,
        "the run genuinely completed before the close — the wrapper's \
         real outcome is preserved, never rewritten by the closer",
    );
    drop(reg);
    let handles = required(ctx.get_extension::<AgentHandles>(), "AgentHandles")?;
    assert!(
        !handles.contains(fork_id),
        "the closer takes ownership of the handle",
    );
    Ok(())
}

/// Mid-run close terminates the fork's inner run (HIGH-fix
/// regression, fork path — mirrors the spawn-side test): a fork
/// parked inside an in-flight provider call is closed. The handle's
/// cancellation token terminates the run itself, the wrapper records
/// the real outcome (registry `Failed`, `AgentStopReason::Cancelled`
/// on the result channel), and the run never reaches another
/// provider iteration.
#[tokio::test]
async fn close_mid_run_cancels_fork_inner_run_and_records_cancelled_outcome() -> TestResult {
    use crate::agent::output::AgentStopReason;
    use crate::agent::result_channel::ChildResultSender;
    use crate::tools::agent::coord::CloseAgentTool;

    /// Provider whose stream never yields: the fork parks inside the
    /// in-flight provider call until cancelled. Counts `stream()`
    /// calls and notifies `entered` on each.
    struct ParkedProvider {
        entered: Arc<tokio::sync::Notify>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for ParkedProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.notify_one();
            Ok(Box::pin(stream::pending::<
                Result<ProviderEvent, ProviderError>,
            >()))
        }
    }

    let entered = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ParkedProvider {
        entered: Arc::clone(&entered),
        calls: Arc::clone(&calls),
    });
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "long haul", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;

    // Deterministic hook: the fork is inside its first in-flight
    // provider call (`notify_one` stores a permit — race-free).
    entered.notified().await;

    let close_out = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-1".to_string(),
                tool_name: "close_agent".to_string(),
                model_args: json!({
                    "agent_id": fork_id.to_string(),
                    "reason": "stand down",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;

    assert_eq!(
        close_out.content["shut_down"][0]["status"], "reclaimed",
        "cancellation lets the fork wrapper finish its own terminal sequence: {:?}",
        close_out.content,
    );
    let reg = agent_registry.read();
    assert!(reg.get(fork_id).is_none(), "entry reclaimed by the close");
    let tombstone = required(reg.tombstone(fork_id), "tombstone must be retained")?;
    assert_eq!(
        tombstone.status,
        AgentStatus::Failed,
        "a cancelled fork records Failed — never Completed",
    );
    drop(reg);

    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, fork_id);
    assert!(!result.succeeded, "a cancelled fork is not a success");
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the fork's inner run must stop at the cancelled provider call, \
         not continue to further iterations",
    );
    let handles = required(ctx.get_extension::<AgentHandles>(), "AgentHandles")?;
    assert!(
        !handles.contains(fork_id),
        "the closer takes ownership of the handle",
    );
    Ok(())
}

/// Production regression (action-log tree): a fork inherits the
/// `action_log` TOOL through the shared registry but previously
/// received no `ActionLog` extension — every call inside the fork
/// failed with `MissingExtension`. The fork now carries its own
/// per-agent log, which starts EMPTY at the fork point (its seeded
/// conversation is its memory; its action log records what it did) —
/// even when the parent's own log already has entries. The parent
/// federates over the fork's entries with `scope: "all"`.
#[tokio::test]
async fn fork_action_log_query_works_and_log_starts_at_fork_point() -> TestResult {
    use crate::session::action_log::{ActionLog, CompletionRecord, Outcome};
    use crate::tools::action_log::ActionLogTool;

    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc-log".to_string(),
            call_id: None,
            name: Some("action_log".to_string()),
            arguments_delta: json!({ "query": "list" }).to_string(),
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

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(ActionLogTool::new()));
    let tool_registry = Arc::new(tool_registry);

    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );
    // The parent's own log already holds an entry: the fork's log
    // must NOT inherit it.
    let parent_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
    parent_log.record_completion(CompletionRecord {
        tool_name: "read",
        tool_call_id: "parent-call",
        tool_use_description: "",
        outcome: Outcome::Success,
        output: &json!({ "path": "x", "lines": 1 }),
        args: json!({}),
        duration_ms: 1,
        follow_ups: Vec::new(),
        post_validate_outcome: None,
        level_1_only: false,
    });
    ctx.insert_extension(Arc::clone(&parent_log));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(
                json!({"request": "inspect your log", "model": "gpt-5.5", "requirements": []}),
            ),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    // The fork's action_log call succeeded — the MissingExtension
    // regression is pinned here — and saw an EMPTY log: the fork's
    // log starts at the fork point, not at the parent's history.
    let result = child_store.events().into_iter().find_map(|e| match e {
        SessionEvent::ToolResult {
            tool_name,
            tool_call_id,
            output,
            ..
        } if tool_name == "action_log" && tool_call_id == "tc-log" => Some(output),
        _ => None,
    });
    let result = required(result, "the fork's action_log call must produce a result")?;
    assert!(
        result.get("error").is_none(),
        "the fork's action_log query must succeed: {result}",
    );
    assert_eq!(
        result["count"], 0,
        "the fork's log starts empty at the fork point: {result}",
    );

    // Federation: the parent's scope=all sees the fork's recorded
    // call, labeled with the fork's registry path.
    let federated = ActionLogTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "parent-query".to_string(),
                tool_name: "action_log".to_string(),
                model_args: json!({ "query": "list", "scope": "all" }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!federated.is_error(), "{:?}", federated.content);
    let entries = required(
        federated.content["entries"].as_array(),
        "federated entries array",
    )?;
    let fork_entry = required(
        entries.iter().find(|e| e["tool"] == "action_log"),
        "the fork's call must surface in the parent's scope=all",
    )?;
    let fork_agent = required(fork_entry["agent"].as_str(), "fork action-log agent path")?;
    assert!(
        fork_agent.contains("/fork/"),
        "the fork's entry is labeled with its registry path: {fork_entry}",
    );
    assert!(
        entries.iter().any(|e| e["id"] == "parent-call"),
        "the parent's own entry interleaves into scope=all",
    );
    Ok(())
}
