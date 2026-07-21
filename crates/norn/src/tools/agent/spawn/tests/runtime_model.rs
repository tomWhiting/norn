use super::*;

/// F2 regression: `AgentModel` is refreshed at STEP START with the
/// model the step's provider requests actually use, and parent-model
/// inheritance prefers that live value over the registry row stamped
/// at build. Agent built (registered + assembly-stamped) on model A,
/// runtime step driven on model B, spawn with NO model from inside
/// that step → the child launches on B (asserted on the child's
/// actual provider request), the `subagent.started` descriptor
/// discloses B, and the parent's registry row still says A (proving
/// the flip in `resolve_parent_model` is what carries the switch).
#[tokio::test]
async fn runtime_model_switch_reaches_child_via_live_agent_model() -> TestResult {
    use crate::r#loop::runner::{AgentStepRequest, run_agent_step};

    // Two catalogued models (factual catalog ids, never invented):
    // A is the build-time model, B the runtime-switched step model.
    let model_a = "gpt-5.4-mini";
    let model_b = CATALOG_MODEL;
    assert_ne!(model_a, model_b, "test precondition");

    // The child's provider: captures the child's real request so the
    // launch model is asserted against ground truth.
    let child_captured = Arc::new(StdMutex::new(Vec::new()));
    let child_provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&child_captured),
        responses: StdMutex::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "child done".to_string(),
            },
            done_event(),
        ]]),
    });

    // The parent is registered at build time on model A — the row
    // that goes stale across a runtime `/model` switch.
    let agent_registry = AgentRegistry::shared();
    let parent_guard = AgentRegistry::reserve(
        &agent_registry,
        "/parent".to_owned(),
        "orchestrator".to_owned(),
        model_a.to_owned(),
        None,
        test_envelope().child_policy,
        None,
    )?;
    let parent_id = parent_guard.id();
    parent_guard.confirm()?;

    let ctx = parent_ctx(
        child_provider,
        parent_id,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    // The assembly-time stamp: model A, exactly what the builder
    // publishes at build.
    ctx.insert_extension(Arc::new(AgentModel {
        model: model_a.to_owned(),
        reasoning_effort: None,
    }));
    let ctx = Arc::new(ctx);

    // The parent step's tool surface carries the spawn tool; the
    // executor exposes the parent's context, so the step-start
    // refresh lands on it (the same seam every driver uses).
    let mut step_registry = ToolRegistry::new();
    step_registry.register(Box::new(SpawnAgentTool::new()));
    let executor = SubAgentExecutor::new(Arc::new(step_registry), None, Arc::clone(&ctx));

    // The parent's own step: one stream that calls spawn_agent with
    // NO model, then a closing stream.
    let parent_provider = MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-spawn".to_string(),
                call_id: None,
                name: Some("spawn_agent".to_string()),
                arguments_delta: json!({"task": "child work", "role": "worker"}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "spawned".to_string(),
            },
            done_event(),
        ],
    ]);

    let store = EventStore::new();
    let mut loop_context = LoopContext::new("base");
    let config = crate::r#loop::config::AgentLoopConfig::default();
    let _result = run_agent_step(AgentStepRequest {
        provider: &parent_provider,
        executor: &executor,
        store: &store,
        user_prompt: "delegate the work",
        tools: &[],
        output_schema: None,
        // The runtime-switched model for THIS step — exactly what
        // the CLI orchestrator passes after reading SlashState.model.
        model: model_b,
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;

    // The step-start refresh landed on the parent's context.
    let live = ctx
        .get_extension::<AgentModel>()
        .ok_or("required test value")?;
    assert_eq!(
        live.model, model_b,
        "the step-start refresh must re-publish the step's actual model",
    );
    // …while the registry row keeps its stale build-time stamp.
    assert_eq!(
        agent_registry
            .read()
            .get(parent_id)
            .ok_or("required test value")?
            .model,
        model_a,
        "the registry row is stamped at build and stays stale — the live \
             extension is what must carry the switch",
    );

    // Wait for the spawned child to finish its step.
    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;
    let child_id = {
        let ids = handles.list_children();
        assert_eq!(ids.len(), 1, "exactly one child spawned: {ids:?}");
        ids[0]
    };
    let mut status_rx = handles.status_rx(child_id).ok_or("required test value")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        status_rx
            .wait_for(|status| *status == AgentStatus::Idle || status.is_terminal())
            .await
    })
    .await??;

    // The child's actual provider request runs on B, not A.
    let child_requests = child_captured.lock().clone();
    assert!(!child_requests.is_empty(), "the child made a provider call");
    for request in &child_requests {
        assert_eq!(
            request.model, model_b,
            "the child must inherit the LIVE step model, not the stale \
                 build-time registry stamp",
        );
    }

    // The subagent.started descriptor discloses B durably.
    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;
    let started = infra
        .event_store
        .events()
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == crate::provider::agent_event::SUBAGENT_STARTED_EVENT_TYPE => {
                Some(data)
            }
            _ => None,
        })
        .ok_or("required test value")?;
    assert_eq!(
        started["descriptor"]["model"], model_b,
        "the started descriptor discloses the live model: {started}",
    );
    Ok(())
}
