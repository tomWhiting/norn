use super::*;

// -- Usage-floor anchor (owner incident 2026-07) ------------------------

/// After a successful provider call the loop records the provider-reported
/// spend (`input + output`) as the usage floor on the `ContextEdits`
/// tracker, so the next preflight anchors its token warning and compaction
/// trigger on `max(estimate, floor)`.
#[tokio::test]
async fn provider_step_records_usage_floor_from_reported_usage() {
    let store = EventStore::new();
    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig::default();
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    // `done_event` reports input 10 + output 5.
    assert_eq!(
        loop_ctx
            .context_edits
            .as_ref()
            .and_then(crate::session::context_edit::ContextEdits::usage_floor),
        Some(15),
        "the step must record the provider-reported spend as the usage floor",
    );
}

/// The advisory `loop.token_warning` anchors on `max(estimate, floor)` and
/// its payload carries all three numbers (`estimated`, `usage_floor`,
/// `effective`) plus the limit, for observability. Here the estimate alone
/// is far below the limit — only the floor pushes the effective count over.
#[tokio::test]
async fn token_warning_fires_on_the_usage_floor_and_carries_all_numbers() {
    let store = EventStore::new();
    let provider = MockProvider::new(vec![vec![
        text_delta("ok"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    let mut edits = crate::session::context_edit::ContextEdits::new();
    edits.set_usage_floor(9_999);
    loop_ctx.context_edits = Some(edits);
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    // Small window; the reserve trigger is disabled so only the advisory
    // warning path is exercised.
    let config = AgentLoopConfig {
        context_window_limit: Some(5_000),
        auto_compact_reserve_tokens: None,
        ..AgentLoopConfig::default()
    };
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let warnings: Vec<Value> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "loop.token_warning" => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one loop.token_warning expected, got {warnings:?}",
    );
    let data = &warnings[0];
    let estimated = data["estimated"].as_u64();
    assert!(
        estimated.is_some_and(|value| value < 5_000),
        "the estimate alone must be present and under the limit (got {estimated:?}) — \
         the warning fired on the floor",
    );
    assert_eq!(data["usage_floor"].as_u64(), Some(9_999));
    assert_eq!(data["effective"].as_u64(), Some(9_999));
    assert_eq!(data["limit"].as_u64(), Some(5_000));
}
