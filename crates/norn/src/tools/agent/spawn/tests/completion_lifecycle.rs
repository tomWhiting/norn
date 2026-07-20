use super::*;

/// R7: the hard-error path still marks the registry `Failed` and still
/// sends a result through the child result channel with
/// `succeeded: false`.
#[tokio::test]
async fn child_failure_marks_failed_and_sends_result() -> TestResult {
    // Empty MockProvider — the first `stream()` call errors, so the
    // child's `run_agent_step` returns Err.
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let ctx = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::clone(&router),
    );
    let sender = ChildResultSender(Arc::new(tx));
    ctx.insert_extension(Arc::new(sender));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "will fail", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    // Terminal transition retains the entry with Failed status; the
    // result channel carries the failure.
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Failed,
    );
    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert!(!result.succeeded, "child must report failure");
    assert!(result.error.is_some(), "error message present on failure");
    Ok(())
}

/// Typed lifecycle: spawn emits `SubagentLifecycle::Started` then
/// `Completed` on the shared broadcast channel — child-tagged, with
/// parent/child ids, the spawn descriptor, ordered wall-clock
/// timestamps, and the child's accumulated usage — and appends the
/// matching `subagent.started` / `subagent.completed` Custom audit
/// events to the parent's session store. The result channel carries
/// the same per-child usage.
#[tokio::test]
async fn spawn_emits_typed_lifecycle_events_on_channel_and_parent_store() -> TestResult {
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
        SharedAgentEventChannel, SubagentKind, SubagentLifecycle,
    };

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child done".to_string(),
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
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let before = Utc::now();
    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    // Collect every broadcast event; lifecycle events are child-tagged
    // and `Started` must precede the child's own provider events.
    let mut subagent_events = Vec::new();
    let mut first_child_event_is_started = None;
    while let Ok(ev) = brx.try_recv() {
        if ev.agent_id == child_id && first_child_event_is_started.is_none() {
            first_child_event_is_started = Some(matches!(
                ev.event,
                AgentEventKind::Subagent(SubagentLifecycle::Started { .. })
            ));
        }
        if let AgentEventKind::Subagent(lifecycle) = ev.event {
            assert_eq!(ev.agent_id, child_id, "lifecycle events are child-tagged");
            assert_eq!(*ev.agent_role, format!("spawn/{CATALOG_MODEL}"));
            subagent_events.push(lifecycle);
        }
    }
    assert_eq!(
        first_child_event_is_started,
        Some(true),
        "Started must precede the child's own provider events",
    );
    assert_eq!(subagent_events.len(), 2, "exactly Started then Completed");
    let SubagentLifecycle::Started {
        parent_id,
        child_id: c,
        descriptor,
        started_at,
    } = &subagent_events[0]
    else {
        return Err(format!("expected Started, got {:?}", subagent_events[0]).into());
    };
    assert_eq!(*parent_id, parent);
    assert_eq!(*c, child_id);
    assert_eq!(descriptor.kind, SubagentKind::Spawn);
    assert_eq!(descriptor.role, "worker");
    assert_eq!(descriptor.model, CATALOG_MODEL);
    assert!(descriptor.profile.is_none());
    assert!(
        *started_at >= before,
        "started_at is wall-clock launch time"
    );

    let SubagentLifecycle::Completed {
        parent_id,
        child_id: c,
        descriptor,
        started_at,
        completed_at,
        usage,
        subtree_usage,
        succeeded,
        error,
        stop,
    } = &subagent_events[1]
    else {
        return Err(format!("expected Completed, got {:?}", subagent_events[1]).into());
    };
    assert_eq!(*parent_id, parent);
    assert_eq!(*c, child_id);
    assert_eq!(descriptor.kind, SubagentKind::Spawn);
    assert!(*completed_at >= *started_at, "timestamps must be ordered");
    assert!(*succeeded);
    assert!(error.is_none());
    assert!(stop.is_none());
    assert_eq!(usage.input_tokens, 10, "per-child usage must surface");
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(
        subtree_usage.input_tokens, 10,
        "a childless child's subtree usage equals its own usage",
    );
    assert_eq!(subtree_usage.output_tokens, 5);

    // Audit carrier: the parent store got both Custom events.
    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;
    let custom: Vec<(String, serde_json::Value)> = infra
        .event_store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } => Some((event_type, data)),
            _ => None,
        })
        .collect();
    assert_eq!(custom.len(), 2, "started + completed audit events");
    assert_eq!(custom[0].0, SUBAGENT_STARTED_EVENT_TYPE);
    assert_eq!(custom[0].1["phase"], "started");
    assert_eq!(custom[0].1["child_id"], child_id.to_string());
    assert_eq!(custom[0].1["descriptor"]["kind"], "spawn");
    assert_eq!(custom[1].0, SUBAGENT_COMPLETED_EVENT_TYPE);
    assert_eq!(custom[1].1["phase"], "completed");
    assert_eq!(custom[1].1["succeeded"], true);
    assert_eq!(custom[1].1["usage"]["input_tokens"], 10);

    // The result channel carries the same per-child usage.
    let result = rx.try_recv()?;
    assert_eq!(result.usage.input_tokens, 10);
    assert_eq!(result.usage.output_tokens, 5);
    Ok(())
}

/// Typed lifecycle on the failure path: a child whose provider errors
/// reports `Completed` with `succeeded: false`, the error description,
/// and zero usage (no provider call completed).
#[tokio::test]
async fn failed_spawn_emits_completed_lifecycle_with_error() -> TestResult {
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
    };

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "will fail", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let mut completed = None;
    while let Ok(ev) = brx.try_recv() {
        if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
            child_id: c,
            succeeded,
            error,
            usage,
            ..
        }) = ev.event
        {
            completed = Some((c, succeeded, error, usage));
        }
    }
    let (c, succeeded, error, usage) = completed.ok_or("required test value")?;
    assert_eq!(c, child_id);
    assert!(!succeeded, "failed child must report succeeded: false");
    assert!(error.is_some(), "error description must be present");
    assert_eq!(usage.input_tokens, 0, "no provider call completed");
    Ok(())
}

/// Panic defense: a panic inside the child's run (here: a tool that
/// panics, standing in for a panicking dependency) must not leave
/// observers a dangling `Started`. The wrapper isolates the run on an
/// inner task, observes the `JoinError`, and still emits the
/// `Completed` lifecycle event with `succeeded: false` and an honest
/// error, delivers the failure through the result channel, and marks
/// the registry `Failed`.
#[tokio::test]
async fn panicking_child_task_still_completes_lifecycle_and_delivers_result() -> TestResult {
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SharedAgentEventChannel, SubagentLifecycle,
    };

    struct PanickingTool;

    #[async_trait]
    impl TestTool for PanickingTool {
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
                "dependency panic inside child tool",
            );
            Ok(TestToolOutput::success(json!({})))
        }
    }

    let turn = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc-panic".to_string(),
            call_id: None,
            name: Some("explode".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PanickingTool));

    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "boom", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    // Registry: the wrapper still applied the terminal transition.
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Failed,
    );

    // Result channel: the failure is delivered, naming the panic.
    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, child_id);
    assert!(!result.succeeded, "panicked child must report failure");
    let error = result.error.ok_or("required test value")?;
    assert!(
        error.contains("panicked before completing"),
        "error must be honest about the panic: {error}",
    );

    // Lifecycle: `Completed` is emitted — no dangling `Started`.
    let mut completed = None;
    while let Ok(ev) = brx.try_recv() {
        if let AgentEventKind::Subagent(SubagentLifecycle::Completed {
            child_id: c,
            succeeded,
            error,
            usage,
            ..
        }) = ev.event
        {
            completed = Some((c, succeeded, error, usage));
        }
    }
    let (c, succeeded, error, usage) = completed.ok_or("required test value")?;
    assert_eq!(c, child_id);
    assert!(!succeeded);
    assert!(
        error
            .unwrap_or_default()
            .contains("panicked before completing"),
        "lifecycle error must name the panic outcome",
    );
    assert_eq!(usage.input_tokens, 0, "usage is unknown after a panic");
    Ok(())
}
