use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Provider whose event stream never yields anything, so
/// `call_provider`'s `next().await` hangs forever. Lets C10 exercise
/// the `tokio::select!` cancel arm against an in-flight provider
/// call without depending on real I/O.
struct HangingProvider;

impl Provider for HangingProvider {
    fn stream(
        &self,
        _request: ProviderRequest,
    ) -> Result<crate::provider::traits::ProviderStream, ProviderError> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
}

#[tokio::test]
async fn cancellation_before_first_iteration_returns_cancelled() -> TestResult {
    // C9: token is already cancelled when the loop starts, so the
    // top-of-iteration check fires before the provider is ever
    // invoked. A `HangingProvider` proves it: if the gate did not
    // catch the cancel, the test would hang.
    let provider = HangingProvider;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let token = CancellationToken::new();
    token.cancel();

    let mut loop_ctx = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(token),
    })
    .await?;

    assert!(
        matches!(result, AgentStepResult::Cancelled { .. }),
        "expected Cancelled, got {result:?}",
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_mid_iteration_returns_cancelled() -> TestResult {
    // C10: token fires while the provider call is in flight. The
    // tokio::select! race in the loop body resolves the cancel arm
    // and returns Cancelled. Usage stays zero because the provider
    // never produced a Done event (and so no `total_usage += ...`
    // ever ran), which matches the R3 acceptance: partial usage is
    // captured if available, not synthesised.
    let provider = HangingProvider;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let token = CancellationToken::new();
    let config = default_config();

    let mut loop_ctx = LoopContext::new("system");
    let step = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(token.clone()),
    });
    let cancel_after_delay = async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();
    };

    let (result, ()) = tokio::join!(step, cancel_after_delay);
    let result = result?;
    assert!(
        matches!(result, AgentStepResult::Cancelled { .. }),
        "expected Cancelled, got {result:?}",
    );
    Ok(())
}

#[tokio::test]
async fn no_cancellation_token_runs_to_completion_unchanged() -> TestResult {
    // C11: regression baseline: passing `None` for `cancel`
    // bypasses the select! and direct-awaits the provider, so the
    // loop produces the same Completed result it did before NB-P2.
    let events = vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(StopReason::EndTurn),
    ];
    let provider = MockProvider::new(vec![events]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();

    let mut loop_ctx = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hello",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;

    assert!(
        matches!(result, AgentStepResult::Completed { .. }),
        "expected Completed with None cancel, got {result:?}",
    );
    Ok(())
}
