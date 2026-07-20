use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- REVIEW item 5: truncation must not masquerade as Completed --------

async fn run_truncation_step(
    provider: &MockProvider,
    store: &EventStore,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
}

/// REVIEW item 5 (Phase 2 shape): a `max_tokens` stop with no tool
/// calls in no-schema mode is a *stopped run*, not a `Completed` one
/// and not an error. It returns the typed `Truncated` outcome carrying
/// the partial text, iteration count, and accumulated usage — making
/// the truncation impossible to mistake for success while keeping the
/// partial output on the return value. (Replaces the Phase 1 stopgap
/// that returned `ProviderError::Truncated`; truncation can no longer
/// reach the retry classifier at all, so the never-retry property is
/// structural.)
#[tokio::test]
async fn max_tokens_truncation_is_a_typed_stop_not_completed() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("partial answ"),
        done_event(StopReason::MaxTokens),
    ]]);
    let store = EventStore::new();

    let result = run_truncation_step(&provider, &store).await?;

    let (kind, partial_text, iterations, usage) = match result {
        AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            ..
        } => (kind, partial_text, iterations, usage),
        other => {
            return Err(std::io::Error::other(format!(
                "expected AgentStepResult::Truncated, got {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(kind, TruncationKind::MaxTokens);
    assert_eq!(partial_text.as_deref(), Some("partial answ"));
    assert_eq!(iterations, 1);
    assert!(
        usage.input_tokens > 0 || usage.output_tokens > 0,
        "accumulated usage must ride the truncated outcome: {usage:?}"
    );

    // Partial text + stop reason persisted for recovery.
    let assistant = store.events().into_iter().find_map(|e| match e {
        SessionEvent::AssistantMessage {
            content,
            stop_reason,
            ..
        } => Some((content, stop_reason)),
        _ => None,
    });
    let (content, stop_reason) =
        assistant.ok_or_else(|| std::io::Error::other("assistant message was not persisted"))?;
    assert_eq!(content, "partial answ");
    assert_eq!(stop_reason, "max_tokens");

    let truncated_event = store.events().into_iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == "loop.truncated"
        )
    });
    assert!(truncated_event, "loop.truncated event must be persisted");
    Ok(())
}

#[tokio::test]
async fn content_filter_truncation_is_a_typed_stop_not_completed() -> TestResult {
    let provider = MockProvider::new(vec![vec![done_event(StopReason::ContentFilter)]]);
    let store = EventStore::new();

    let result = run_truncation_step(&provider, &store).await?;

    let (kind, partial_text) = match result {
        AgentStepResult::Truncated {
            kind, partial_text, ..
        } => (kind, partial_text),
        other => {
            return Err(std::io::Error::other(format!(
                "expected AgentStepResult::Truncated, got {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(kind, TruncationKind::ContentFilter);
    assert!(
        partial_text.is_none(),
        "no text was produced, so no partial text: {partial_text:?}"
    );
    Ok(())
}

/// With a schema present, truncation funnels into the existing nudge
/// path: budget is consumed and the step terminates `SchemaUnreachable` —
/// never a silent Completed.
#[tokio::test]
async fn truncation_with_schema_consumes_budget() {
    let provider = MockProvider::new(vec![vec![
        text_delta("partial"),
        done_event(StopReason::MaxTokens),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(1),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 1);
}
