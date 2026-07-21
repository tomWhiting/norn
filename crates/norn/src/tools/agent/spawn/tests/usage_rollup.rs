use super::*;

/// `Done` event with explicit usage so each tree level reports
/// distinct token counts (W3.6 rollup tests).
pub(super) fn done_with(stop_reason: StopReason, input: u64, output: u64) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
        response_id: None,
    }
}

/// W3.6 provider: routes like [`TreeProvider`] but stamps distinct
/// usage on every level and call — grandchild (7, 3); child calls
/// (100, 50), (200, 60), (300, 70) — so any double count or dropped
/// level changes the totals. The child's would-stop turn is held
/// until the registry shows the grandchild parked idle, guaranteeing
/// its result (and its `subtree_usage`) is already in the child's
/// channel when the boundary sweep folds it.
struct UsageTreeProvider {
    registry: Arc<RwLock<AgentRegistry>>,
    child_calls: Arc<std::sync::atomic::AtomicUsize>,
    /// Tool call the third child turn emits: `None` stops with text
    /// (rollup test); `Some(name)` calls that tool (panic test).
    third_turn_tool: Option<&'static str>,
}

impl Provider for UsageTreeProvider {
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
        if last == "usage-grandchild-task" {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "grandchild usage report".to_string(),
                }),
                Ok(done_with(StopReason::EndTurn, 7, 3)),
            ])));
        }
        let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
        match call {
            0 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "tc-usage-grandchild".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "usage-grandchild-task",
                        "model": CATALOG_MODEL,
                        "role": "leaf",
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_with(StopReason::ToolUse, 100, 50)),
            ]))),
            1 => {
                let registry = Arc::clone(&self.registry);
                let s = stream::once(async move {
                    for _ in 0..2400 {
                        if idle_grandchild_entry(&registry).is_some() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    assert!(
                        idle_grandchild_entry(&registry).is_some(),
                        "grandchild never parked idle - the test cannot proceed",
                    );
                })
                .flat_map(|()| {
                    stream::iter(vec![
                        Ok(ProviderEvent::TextDelta {
                            text: "waited for grandchild".to_string(),
                        }),
                        Ok(done_with(StopReason::EndTurn, 200, 60)),
                    ])
                });
                Ok(Box::pin(s))
            }
            _ => match self.third_turn_tool {
                Some(tool_name) => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCallDelta {
                        item_id: "tc-third-turn".to_string(),
                        call_id: None,
                        name: Some(tool_name.to_string()),
                        arguments_delta: "{}".to_string(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    }),
                    Ok(done_with(StopReason::ToolUse, 300, 70)),
                ]))),
                None => Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::TextDelta {
                        text: "child done after grandchild".to_string(),
                    }),
                    Ok(done_with(StopReason::EndTurn, 300, 70)),
                ]))),
            },
        }
    }
}

/// W3.6 acceptance (rollup): a depth-2 tree with distinct synthetic
/// usage at each level sums **exactly once** at the root. The
/// grandchild's lifecycle reports `subtree_usage == own (7, 3)`; the
/// child's own `usage` stays own-calls-only (600, 180) — proving the
/// drained grandchild subtree was never folded into it — and the
/// root receives `subtree_usage == (607, 183) == Σ` both levels,
/// each counted once.
#[tokio::test]
async fn depth2_subtree_usage_sums_each_level_exactly_once_at_the_root() -> TestResult {
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
    };

    let agent_registry = AgentRegistry::shared();
    let provider: Arc<dyn Provider> = Arc::new(UsageTreeProvider {
        registry: Arc::clone(&agent_registry),
        child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        third_turn_tool: None,
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    let root_id = Uuid::new_v4();
    let ctx = parent_ctx(
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(child_result.agent_id, child_id);
    assert!(child_result.succeeded, "{:?}", child_result.error);

    // Own usage stays own-calls-only: 100+200+300 / 50+60+70.
    assert_eq!(
        child_result.usage.input_tokens, 600,
        "the child's own usage must never absorb the grandchild's",
    );
    assert_eq!(child_result.usage.output_tokens, 180);
    // Subtree = child own + grandchild own, each exactly once.
    assert_eq!(
        child_result.subtree_usage.input_tokens, 607,
        "root must receive Σ of both levels, each counted once",
    );
    assert_eq!(child_result.subtree_usage.output_tokens, 183);

    // Lifecycle carrier agrees at every level: the grandchild (a
    // leaf) reports subtree == own (7, 3); the child reports its own
    // usage and the folded subtree total.
    let mut completed: Vec<(Uuid, Usage, Usage)> = Vec::new();
    while let Ok(ev) = brx.try_recv() {
        if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
            child_id: c,
            usage,
            subtree_usage,
            ..
        }) = ev.event
        {
            completed.push((c, usage, subtree_usage));
        }
    }
    assert_eq!(completed.len(), 2, "grandchild + child lifecycles");
    let (_, grandchild_usage, grandchild_subtree) = completed
        .iter()
        .find(|(c, _, _)| *c != child_id)
        .ok_or("required test value")?;
    assert_eq!(grandchild_usage.input_tokens, 7);
    assert_eq!(grandchild_usage.output_tokens, 3);
    assert_eq!(
        grandchild_subtree.input_tokens, 7,
        "a leaf's subtree usage equals its own usage",
    );
    assert_eq!(grandchild_subtree.output_tokens, 3);
    let (_, child_usage, child_subtree) = completed
        .iter()
        .find(|(c, _, _)| *c == child_id)
        .ok_or("required test value")?;
    assert_eq!(child_usage.input_tokens, 600);
    assert_eq!(child_usage.output_tokens, 180);
    assert_eq!(child_subtree.input_tokens, 607);
    assert_eq!(child_subtree.output_tokens, 183);
    Ok(())
}

/// W3.6 acceptance (honest zeros): a mid-tree child that panics
/// AFTER its loop delivered the grandchild's result reports its own
/// usage as unknown-zeros while the grandchild's `subtree_usage`
/// still rolls up — partial truth beats silent loss. The shared
/// accumulator survives the unwound loop task; nothing is invented
/// for the child's own spend.
#[tokio::test]
async fn panicked_mid_tree_child_still_rolls_up_delivered_grandchild_usage() -> TestResult {
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
    };

    /// Stands in for a panicking dependency inside the child's run.
    struct ExplodingTool;

    #[async_trait]
    impl TestTool for ExplodingTool {
        fn name(&self) -> &'static str {
            "explode"
        }
        fn description(&self) -> &'static str {
            "panics on execute (test stand-in for a panicking dependency)"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            assert!(
                envelope.tool_name.is_empty(),
                "dependency panic after the grandchild result was delivered",
            );
            Ok(TestToolOutput::success(json!({})))
        }
    }

    let agent_registry = AgentRegistry::shared();
    let provider: Arc<dyn Provider> = Arc::new(UsageTreeProvider {
        registry: Arc::clone(&agent_registry),
        child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        third_turn_tool: Some("explode"),
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    tool_registry.register(Box::new(ExplodingTool));
    let root_id = Uuid::new_v4();
    let ctx = parent_ctx(
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    let child_result = tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(child_result.agent_id, child_id);
    assert!(!child_result.succeeded, "the panicked child must fail");
    assert!(
        child_result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("panicked before completing"),
        "the panic must surface honestly: {:?}",
        child_result.error,
    );
    // Own usage: unknown-zeros — the panicked task took it along.
    assert_eq!(
        child_result.usage.input_tokens, 0,
        "own usage is unknown after a panic — zeros, never invented",
    );
    assert_eq!(child_result.usage.output_tokens, 0);
    // Delivered grandchild subtree still present in the rollup.
    assert_eq!(
        child_result.subtree_usage.input_tokens, 7,
        "the grandchild's delivered subtree must survive the panic",
    );
    assert_eq!(child_result.subtree_usage.output_tokens, 3);

    // The lifecycle carrier agrees: the child's Completed reports
    // zeros for its own usage with the grandchild subtree intact.
    let mut child_completed = None;
    while let Ok(ev) = brx.try_recv() {
        if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
            child_id: c,
            usage,
            subtree_usage,
            succeeded,
            ..
        }) = ev.event
            && c == child_id
        {
            child_completed = Some((usage, subtree_usage, succeeded));
        }
    }
    let (usage, subtree_usage, succeeded) = child_completed.ok_or("required test value")?;
    assert!(!succeeded);
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(subtree_usage.input_tokens, 7);
    assert_eq!(subtree_usage.output_tokens, 3);
    Ok(())
}
