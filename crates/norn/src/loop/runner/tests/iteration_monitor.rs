use super::*;

// -- R2 (N-011): iteration monitor wiring ----------------------------

fn iteration_monitor(handoff_pct: f64, warn_pct: f64) -> crate::r#loop::IterationMonitorConfig {
    crate::r#loop::IterationMonitorConfig {
        context_window_tokens: 20,
        warn_threshold_pct: warn_pct,
        handoff_threshold_pct: handoff_pct,
        handoff_guidance: "Wrap up cleanly.".to_string(),
        failure_repeat_window: 0,
        hedging_patterns: Vec::new(),
    }
}

/// R2 acceptance: `evaluate_iteration` fires once per loop iteration and
/// a `TokenWarning` is recorded as a `Custom` event in the store. The
/// `MockProvider` emits 10 input + 5 output = 15 tokens per turn, so a
/// 20-token window with warn=0.5 / handoff=0.99 puts the first iteration
/// at 75% utilisation — squarely in the warn band.
#[tokio::test]
async fn token_warning_appends_custom_event() {
    let events = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"warned"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(iteration_monitor(0.99, 0.5));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "warned");

    let token_warnings: Vec<SessionEvent> = store
        .events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == "iteration.token_warning"
            )
        })
        .collect();
    assert_eq!(
        token_warnings.len(),
        1,
        "exactly one iteration.token_warning event expected, got {token_warnings:?}",
    );
    if let SessionEvent::Custom { data, .. } = &token_warnings[0] {
        assert_eq!(data["used"], 15);
        assert_eq!(data["limit"], 20);
        assert!(data["pct"].as_f64().is_some(), "pct must be numeric");
    }
}

/// R2 acceptance: `HandoffTriggered` injects a wrap-up `UserMessage`
/// that the next provider call sees. Turn 1 makes a tool call so the
/// loop's `ToolsOnly` branch keeps the loop running; the handoff message
/// is then visible to turn 2's provider call.
#[tokio::test]
async fn handoff_triggered_injects_user_message() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![text_delta("wrapping up"), done_event(StopReason::EndTurn)];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    // Handoff at 50% — first iteration's 15/20 = 75% triggers it.
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(iteration_monitor(0.5, 0.5));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("wrapping up".to_string()));

    // A handoff-shaped UserMessage must be present in the audit trail.
    let handoff_text = store.events().into_iter().find_map(|e| {
        if let SessionEvent::UserMessage { content, .. } = e
            && content.contains("Wrap up cleanly.")
            && content.contains("75.0%")
            && content.contains("summarize")
        {
            return Some(content);
        }
        None
    });
    assert!(
        handoff_text.is_some(),
        "expected a wrap-up UserMessage with guidance + percentage + summarize",
    );

    // And the provider must have been called twice — turn 2 must see
    // the handoff guidance before producing its wrap-up text.
    assert_eq!(
        provider.call_count(),
        2,
        "handoff must NOT terminate the loop; turn 2 must still run",
    );
}

/// R2 supporting: the `LoopContext::default()` iteration monitor field
/// is `None`, so existing tests (none of which set it) run unchanged.
#[test]
fn default_loop_context_has_no_iteration_monitor() {
    let ctx = LoopContext::default();
    assert!(
        ctx.iteration_monitor.is_none(),
        "default must be None so existing tests run unchanged",
    );
}
