use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- REVIEW item 4: RepeatedFailure monitor fires on real failures -----

#[tokio::test]
async fn repeated_tool_failures_fire_monitor() -> TestResult {
    let failing_call = |id: &str| {
        vec![
            tool_call_delta(id, Some("read_file"), r#"{"path":"f"}"#),
            done_event(StopReason::ToolUse),
        ]
    };
    let provider = MockProvider::new(vec![
        failing_call("tc1"),
        failing_call("tc2"),
        vec![text_delta("giving up"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| {
            Err(crate::error::ToolError::ExecutionFailed {
                reason: "permission denied at line 42".to_string(),
            })
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(crate::r#loop::IterationMonitorConfig {
        context_window_tokens: 0,
        warn_threshold_pct: 1.0,
        handoff_threshold_pct: 1.0,
        handoff_guidance: String::new(),
        failure_repeat_window: 2,
        hedging_patterns: Vec::new(),
    });

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
    assert_completed(result);

    let repeated_failure = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "iteration.repeated_failure" => Some(data),
        _ => None,
    });
    let data = repeated_failure.ok_or_else(|| {
        std::io::Error::other("RepeatedFailure signal did not fire after identical failures")
    })?;
    assert_eq!(data["consecutive_count"], 2);
    let signature = data["error_signature"].as_str().unwrap_or_default();
    assert!(
        signature.contains("permission denied"),
        "signature must reflect the repeated error: {signature}",
    );
    Ok(())
}

// -- Step timeout: accumulated usage rides the TimedOut outcome --------

/// Provider whose first call streams a complete tool-call turn (with
/// usage) and whose second call hangs forever, forcing the step
/// timeout to fire mid-run.
struct HangsOnSecondCall {
    calls: std::sync::atomic::AtomicUsize,
}

impl crate::provider::traits::Provider for HangsOnSecondCall {
    fn stream(
        &self,
        _request: ProviderRequest,
    ) -> Result<crate::provider::traits::ProviderStream, crate::error::ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            Ok(Box::pin(futures_util::stream::iter(
                vec![
                    tool_call_delta("tc1", Some("read_file"), "{}"),
                    done_event(StopReason::ToolUse),
                ]
                .into_iter()
                .map(Ok),
            )))
        } else {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }
}

/// The timed-out outcome must carry the usage accumulated by the
/// provider calls that completed before the budget elapsed — it was
/// previously zeroed because the outer timeout wrapper had no access
/// to the loop's running total.
#[tokio::test(start_paused = true)]
async fn timed_out_carries_accumulated_usage_and_partial_state() -> TestResult {
    let provider = HangsOnSecondCall {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let executor = MockToolExecutor::new(read_file_handlers());
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(std::time::Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;

    let (iterations, usage) = match result {
        AgentStepResult::TimedOut {
            iterations, usage, ..
        } => (iterations, usage),
        other => {
            return Err(std::io::Error::other(format!(
                "expected AgentStepResult::TimedOut, got {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(iterations, 2, "second iteration was in flight");
    // The first provider call completed and reported
    // input 10 / output 5 (see `done_event`).
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    Ok(())
}
