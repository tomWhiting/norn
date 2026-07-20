use super::*;

/// Provider whose stream never yields: the child's run parks inside
/// the in-flight provider call until cancelled. Counts `stream()`
/// calls and notifies `entered` on each, so a test can close the
/// child deterministically mid-call and prove the run never reached
/// another iteration.
pub(super) struct ParkedProvider {
    pub(super) entered: Arc<tokio::sync::Notify>,
    pub(super) calls: Arc<std::sync::atomic::AtomicUsize>,
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

/// Mid-run close terminates the inner run (HIGH-fix regression): a
/// child parked inside an in-flight provider call is closed. The
/// handle's cancellation token must terminate the run itself — not
/// just the wrapper task — so the run never continues toward natural
/// completion: the loop's biased select resolves the cancel arm, the
/// wrapper records the run's REAL outcome (registry `Failed`, typed
/// `AgentStopReason::Cancelled` on the result channel), and the
/// closer's job reduces to reclaiming the terminal entry.
#[tokio::test]
async fn close_mid_run_cancels_inner_run_and_records_cancelled_outcome() -> TestResult {
    use crate::agent::output::AgentStopReason;
    use crate::tools::agent::coord::CloseAgentTool;

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

    // Deterministic hook: the child is now inside its first in-flight
    // provider call (`notify_one` stores a permit, so this is
    // race-free regardless of scheduling).
    entered.notified().await;

    let close_out = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-1".to_string(),
                tool_name: "close_agent".to_string(),
                model_args: json!({
                    "agent_id": child_id.to_string(),
                    "reason": "stand down",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;

    // The wrapper recorded the run's real outcome and the closer
    // reclaimed the (already terminal) entry — it never had to force
    // a mark of its own.
    assert_eq!(
        close_out.content["shut_down"][0]["status"], "reclaimed",
        "cancellation lets the wrapper finish its own terminal sequence: {:?}",
        close_out.content,
    );
    let reg = agent_registry.read();
    assert!(reg.get(child_id).is_none(), "entry reclaimed by the close");
    let tombstone = reg.tombstone(child_id).ok_or("required test value")?;
    assert_eq!(
        tombstone.status,
        AgentStatus::Failed,
        "a cancelled run records Failed — never Completed",
    );
    drop(reg);

    // The run terminated with the cancellation outcome, delivered by
    // the wrapper before the close's join returned.
    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert!(!result.succeeded, "a cancelled run is not a success");
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
    assert!(
        result.error.unwrap_or_default().contains("cancelled"),
        "the failure must name the cancellation",
    );

    // And the inner run did NOT keep executing after the close:
    // exactly one provider call ever started, and the handle is gone.
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the inner run must stop at the cancelled provider call, not \
             continue to further iterations",
    );
    assert!(
        !ctx.get_extension::<AgentHandles>()
            .ok_or("required test value")?
            .contains(child_id),
        "the closer takes ownership of the handle",
    );
    Ok(())
}

/// Production regression (action-log tree): a spawned child inherits
/// the `action_log` TOOL through the shared registry but previously
/// received no `ActionLog` extension — every call inside the child
/// failed with `MissingExtension`. The child now carries its own
/// per-agent log, so the call succeeds end-to-end, and the parent can
/// federate over the child's entries with `scope: "all"`.
#[tokio::test]
async fn spawned_child_action_log_query_works_and_parent_federates() -> TestResult {
    use crate::session::action_log::ActionLog;
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
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ActionLogTool::new()));
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
    // The parent has its own action log (as every builder-assembled
    // agent does) so the lazily-installed tree can register its root.
    let parent_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
    ctx.insert_extension(Arc::clone(&parent_log));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "task": "inspect your log",
                "model": CATALOG_MODEL,
                "role": "worker",
                "path": "/smoke/child",
            })),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    let child_store = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?
        .event_store(child_id)
        .ok_or("required test value")?;

    // The child's action_log call succeeded — the MissingExtension
    // regression is pinned here.
    let result = child_store
        .events()
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } if tool_name == "action_log" => Some(output),
            _ => None,
        })
        .ok_or("required test value")?;
    assert!(
        result.get("error").is_none(),
        "the child's action_log query must succeed: {result}",
    );
    assert_eq!(result["query"], "list");
    assert_eq!(
        result["count"], 0,
        "the child's log is its own and starts empty: {result}",
    );

    // Federation: the parent's scope=all sees the child's recorded
    // call, labeled with the child's registry path.
    let federated = ActionLogTool::new()
        .execute(
            &crate::tool::envelope::ToolEnvelope {
                tool_call_id: "parent-query".to_string(),
                tool_name: "action_log".to_string(),
                model_args: json!({ "query": "list", "scope": "all" }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!federated.is_error(), "{:?}", federated.content);
    let entries = federated.content["entries"]
        .as_array()
        .ok_or("required test value")?;
    let child_entry = entries
        .iter()
        .find(|e| e["tool"] == "action_log")
        .ok_or("required test value")?;
    assert_eq!(child_entry["agent"], "/smoke/child");
    Ok(())
}

/// Route ownership (W3.2): the launch path registers the child's
/// inbound route at launch and the step wrapper deregisters when the
/// child parks idle — `signal_agent` reaches a running child without
/// any tool-side registration, while an idle child queues into its
/// durable mailbox for `wake_agent`.
#[tokio::test]
async fn spawn_registers_route_at_launch_and_deregisters_at_idle() -> TestResult {
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
    let agent_registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::clone(&router),
    );

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "wait", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    assert!(
        router.is_routed(child_id),
        "the launch path must register the child's inbound route",
    );

    gate.notify_one();
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    assert!(
        !router.is_routed(child_id),
        "an idle child must not keep a live inbound route",
    );
    Ok(())
}

/// Missing-envelope boundary: a context that can spawn but carries no
/// [`CoordinationEnvelope`] is a wiring error — spawn refuses with a
/// typed `MissingExtension` naming the envelope, never inventing a
/// child policy.
#[tokio::test]
async fn spawn_requires_coordination_envelope() -> TestResult {
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
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));

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
    assert!(
        extension.contains("CoordinationEnvelope"),
        "error must name the missing envelope: {extension}",
    );
    assert!(
        agent_registry.read().list().is_empty(),
        "no reservation may leak from the refused spawn",
    );
    Ok(())
}

/// `MessagingScope::None` removes `signal_agent` from the child's
/// surface: the tool definitions shown to the child model exclude it
/// (with or without an explicit allow-list) while every other tool
/// survives.
#[tokio::test]
async fn spawn_strips_signal_agent_from_child_surface_under_scope_none() -> TestResult {
    use crate::tools::agent::coord::SignalAgentTool;

    for explicit_tools in [None, Some(vec!["signal_agent", "read"])] {
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
        registry.register(Box::new(SignalAgentTool::new()));
        let registry = Arc::new(registry);

        let agent_registry = AgentRegistry::shared();
        let ctx = parent_ctx(
            provider,
            Uuid::new_v4(),
            &agent_registry,
            registry,
            Arc::new(MessageRouter::new()),
        );
        // Replace the standard test envelope with a muted one.
        let mut envelope = test_envelope();
        envelope.child_policy.messaging = MessagingScope::None;
        ctx.insert_extension(Arc::new(envelope));

        let mut args = json!({"task": "quiet work", "model": CATALOG_MODEL, "role": "worker"});
        if let Some(tools) = &explicit_tools {
            args["tools"] = json!(tools);
        }
        let tool = SpawnAgentTool::new();
        spawn_and_join(&tool, &ctx, args).await;

        let names: Vec<String> = captured
            .lock()
            .iter()
            .map(|definition| match definition {
                ProviderToolDefinition::Function(function) => Ok(function.name.clone()),
                other @ ProviderToolDefinition::Hosted(_) => {
                    Err(format!("unexpected tool definition: {other:?}").into())
                }
            })
            .collect::<TestResult<Vec<_>>>()?;
        assert!(
            !names.iter().any(|n| n == "signal_agent"),
            "scope none must remove signal_agent (explicit_tools: \
                 {explicit_tools:?}): {names:?}",
        );
        assert!(
            names.iter().any(|n| n == "read"),
            "other tools must survive the strip (explicit_tools: \
                 {explicit_tools:?}): {names:?}",
        );
    }
    Ok(())
}
