use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Regression (fired Before-timing injection dropped on gate exit): a
/// Before-timing rule that fires in the final tool batch, when the step
/// then hits `max_iterations` before the next `build_request`, must still
/// leave its `SessionEvent::RuleInjection` audit event persisted — the
/// invariant that a fired rule is recorded regardless of delivery mode.
#[tokio::test]
async fn max_iterations_after_before_fire_persists_the_rule_injection() {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming};

    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    // Before-timing: fired at batch time, buffered for the next request
    // build that `max_iterations` prevents from ever running.
    let rule = Rule {
        id: RuleId::from("rs-before-rule"),
        name: "rs before".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::ContextInjection,
        timing: TriggerTiming::Before,
        body: "before-rule fired".to_string(),
        shell_source: None,
    };

    let provider = MockProvider::new(vec![vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        done_event(StopReason::ToolUse),
    ]]);
    let store = EventStore::new();
    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    let config = AgentLoopConfig {
        max_iterations: Some(1),
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[write_tool],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert!(
        matches!(result, AgentStepResult::MaxIterationsReached { .. }),
        "the step must hit the iteration cap right after the batch, got {result:?}",
    );

    let rule_events: Vec<(String, TriggerTiming)> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::RuleInjection {
                rule_id, timing, ..
            } => Some((rule_id, timing)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rule_events.len(),
        1,
        "the fired Before-timing rule must leave exactly one persisted \
         RuleInjection audit event, got {rule_events:?}",
    );
    assert_eq!(rule_events[0].0, "rs-before-rule");
    assert!(
        matches!(rule_events[0].1, TriggerTiming::Before),
        "the persisted injection keeps its Before timing",
    );
}

/// F2 regression (fired Before-timing injection dropped on the step-timeout
/// drop path): a Before-timing rule that fires in a tool batch reaching a
/// completion boundary, where the `step_timeout` then elapses during the
/// linger await before `StepMachine::run` can return, must still leave its
/// `SessionEvent::RuleInjection` audit event persisted. The timeout drops the
/// inner future — so the run-exit persist never runs — and only the buffer
/// hoisted into `run_agent_step_common` (persisted there) keeps the firing on
/// the record. Before the fix the buffer lived inside the dropped future and
/// the fired rule vanished without an audit event.
///
/// Determinism (paused clock): a `ToolsAndSchemaValid` response fires the
/// Before rule in its pre-schema batch and heads straight to the stop
/// boundary (no second `build_request` ever consumes the buffer). A long
/// linger deadline holds the step *inside* the inner future, so the short
/// `step_timeout` reliably cuts it post-fire.
#[tokio::test(start_paused = true)]
async fn step_timeout_after_before_fire_persists_the_rule_injection() -> TestResult {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming};

    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    // Before-timing: fired at batch time, buffered for a build_request the
    // step-timeout drop path prevents from ever running.
    let rule = Rule {
        id: RuleId::from("rs-before-rule"),
        name: "rs before".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::ContextInjection,
        timing: TriggerTiming::Before,
        body: "before-rule fired".to_string(),
        shell_source: None,
    };

    // Pre-schema write (fires the Before rule) + a valid schema call in one
    // response → ToolsAndSchemaValid → run the batch, accept the schema, head
    // to the stop boundary. The 10ms first-event delay lands inside the 100ms
    // budget; the boundary's 10s linger then holds the step until the budget
    // elapses.
    let turn = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = DelayedProvider::new(vec![turn], Duration::from_millis(10));
    let store = EventStore::new();
    let schema = simple_schema();
    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(100)),
        linger: Some(crate::r#loop::linger::LingerPolicy {
            deadline: Duration::from_secs(10),
        }),
        ..AgentLoopConfig::default()
    };

    // A never-triggered cancel token keeps the linger wake set non-empty so
    // it actually sleeps toward its deadline (an empty wake set short-circuits
    // to expire immediately). The 100ms budget then cuts the linger sleep,
    // dropping the inner future with the fired Before injection still buffered.
    let cancel = CancellationToken::new();
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[write_tool],
        output_schema: Some(&schema),
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(cancel),
    })
    .await?;
    assert!(
        matches!(result, AgentStepResult::TimedOut { .. }),
        "the linger await must be cut by the step timeout, got {result:?}",
    );

    let rule_events: Vec<(String, TriggerTiming)> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::RuleInjection {
                rule_id, timing, ..
            } => Some((rule_id, timing)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rule_events.len(),
        1,
        "the fired Before-timing rule must leave exactly one persisted \
         RuleInjection audit event even on the timeout drop path, got {rule_events:?}",
    );
    assert_eq!(rule_events[0].0, "rs-before-rule");
    assert!(
        matches!(rule_events[0].1, TriggerTiming::Before),
        "the persisted injection keeps its Before timing",
    );
    Ok(())
}
