use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// -- Session-fidelity Gaps 6 & 7: abnormal-stop and hard-cut-partial ----
// records on the agent's own timeline.

/// Store index and data of every Custom event of `event_type`.
fn customs_of_type(store: &EventStore, event_type: &str) -> Vec<(usize, Value)> {
    store
        .events()
        .iter()
        .enumerate()
        .filter_map(|(idx, event)| match event {
            SessionEvent::Custom {
                event_type: ty,
                data,
                ..
            } if ty == event_type => Some((idx, data.clone())),
            _ => None,
        })
        .collect()
}

/// Provider whose only call streams thinking and text deltas, then hangs
/// with no `Done` — the shape of a call a step timeout or cancellation
/// hard-cuts mid-stream.
struct PartialThenHangs;

impl Provider for PartialThenHangs {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use futures_util::StreamExt as _;
        let events: Vec<Result<ProviderEvent, ProviderError>> = vec![
            Ok(thinking_delta("mulling it over ")),
            Ok(text_delta("the answer ")),
            Ok(text_delta("is 4")),
            Ok(refusal_delta("I cannot ")),
            Ok(refusal_delta("help")),
        ];
        Ok(Box::pin(stream::iter(events).chain(stream::pending())))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

struct RefusalThenHangs;

impl Provider for RefusalThenHangs {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use futures_util::StreamExt as _;
        let events = vec![Ok(refusal_delta("I cannot complete this request"))];
        Ok(Box::pin(stream::iter(events).chain(stream::pending())))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// Gap 6 + 7 (timeout): a step cut by `step_timeout` mid-provider-stream
/// must leave BOTH durable records — the aborted call's partial content
/// (`loop.partial_output`, marked hard-cut) and the typed stop reason
/// (`loop.step_stopped`) — in that order, on the agent's own store.
#[tokio::test(start_paused = true)]
async fn step_timeout_persists_partial_output_and_stop_record() -> TestResult {
    let provider = PartialThenHangs;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_agent_step(AgentStepRequest {
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
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));

    let partials = customs_of_type(&store, "loop.partial_output");
    assert_eq!(partials.len(), 1, "exactly one partial-output record");
    let (partial_idx, partial) = &partials[0];
    assert_eq!(partial["stop_reason"].as_str(), Some("timeout"));
    assert_eq!(partial["hard_cut"].as_bool(), Some(true));
    assert_eq!(partial["text"].as_str(), Some("the answer is 4"));
    assert_eq!(partial["thinking"].as_str(), Some("mulling it over "));
    assert_eq!(partial["refusal"].as_str(), Some("I cannot help"));
    assert_eq!(partial["text_chars"].as_u64(), Some(15));
    assert_eq!(partial["thinking_chars"].as_u64(), Some(16));
    assert_eq!(partial["refusal_chars"].as_u64(), Some(13));

    let stops = customs_of_type(&store, "loop.step_stopped");
    assert_eq!(stops.len(), 1, "exactly one stop record");
    let (stop_idx, stop) = &stops[0];
    assert_eq!(stop["stop_reason"].as_str(), Some("timeout"));
    assert_eq!(stop["iterations"].as_u64(), Some(1));
    assert_eq!(stop["budget_ms"].as_u64(), Some(5_000));
    assert!(stop["elapsed_ms"].is_u64(), "elapsed must be recorded");
    assert!(
        partial_idx < stop_idx,
        "the partial content precedes the stop that cut it",
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn step_timeout_returns_and_persists_partial_refusal() -> TestResult {
    let provider = RefusalThenHangs;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_agent_step(AgentStepRequest {
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
        cancel: None,
    })
    .await?;
    let AgentStepResult::TimedOut { partial_output, .. } = result else {
        return Err(std::io::Error::other("refusal stream did not time out").into());
    };
    assert_eq!(
        partial_output,
        Some(Value::String("I cannot complete this request".to_owned()))
    );

    let partials = customs_of_type(&store, "loop.partial_output");
    assert_eq!(partials.len(), 1);
    assert_eq!(
        partials[0].1["refusal"].as_str(),
        Some("I cannot complete this request")
    );
    Ok(())
}

/// Gap 7 (cancellation): a cancel that wins the provider-call race is
/// the other hard cut — the aborted call's partial content must persist,
/// marked with the cancelled stop reason.
#[tokio::test(start_paused = true)]
async fn cancellation_mid_stream_persists_partial_output_and_stop_record() -> TestResult {
    let provider = PartialThenHangs;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = default_config();
    let mut loop_ctx = LoopContext::new("system");
    let token = CancellationToken::new();
    let trigger = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });

    let result = run_agent_step(AgentStepRequest {
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
        cancel: Some(token),
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Cancelled { .. }));

    let partials = customs_of_type(&store, "loop.partial_output");
    assert_eq!(partials.len(), 1, "the aborted call's content must persist");
    let (_, partial) = &partials[0];
    assert_eq!(partial["stop_reason"].as_str(), Some("cancelled"));
    assert_eq!(partial["hard_cut"].as_bool(), Some(true));
    assert_eq!(partial["text"].as_str(), Some("the answer is 4"));
    assert_eq!(partial["refusal"].as_str(), Some("I cannot help"));

    let stops = customs_of_type(&store, "loop.step_stopped");
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].1["stop_reason"].as_str(), Some("cancelled"));
    Ok(())
}

/// Gap 6 (cancellation at the gate): a token already cancelled before the
/// first iteration leaves the typed stop record — and no partial-output
/// record, because no provider call was in flight.
#[tokio::test]
async fn cancellation_at_gate_persists_stop_record_without_partial() -> TestResult {
    let provider = MockProvider::new(vec![]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = default_config();
    let mut loop_ctx = LoopContext::new("system");
    let token = CancellationToken::new();
    token.cancel();

    let result = run_agent_step(AgentStepRequest {
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
        cancel: Some(token),
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Cancelled { .. }));

    let stops = customs_of_type(&store, "loop.step_stopped");
    assert_eq!(stops.len(), 1);
    let (_, stop) = &stops[0];
    assert_eq!(stop["stop_reason"].as_str(), Some("cancelled"));
    assert_eq!(stop["iterations"].as_u64(), Some(0));
    assert!(
        customs_of_type(&store, "loop.partial_output").is_empty(),
        "no provider call was cut, so no partial may be fabricated",
    );
    Ok(())
}

/// Gap 6 (max-iterations): exhausting the iteration cap leaves the typed
/// stop record carrying the cap that fired.
#[tokio::test]
async fn max_iterations_persists_typed_stop_record() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        tool_call_delta("tc1", Some("read_file"), "{}"),
        done_event(StopReason::ToolUse),
    ]]);
    let executor = MockToolExecutor::new(read_file_handlers());
    let store = EventStore::new();
    let config = AgentLoopConfig {
        max_iterations: Some(1),
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
    assert!(matches!(
        result,
        AgentStepResult::MaxIterationsReached { .. }
    ));

    let stops = customs_of_type(&store, "loop.step_stopped");
    assert_eq!(stops.len(), 1);
    let (_, stop) = &stops[0];
    assert_eq!(stop["stop_reason"].as_str(), Some("max_iterations"));
    assert_eq!(stop["max_iterations"].as_u64(), Some(1));
    assert_eq!(stop["iterations"].as_u64(), Some(1));
    assert!(
        customs_of_type(&store, "loop.partial_output").is_empty(),
        "the last provider call assembled fully; nothing was cut",
    );
    Ok(())
}

/// A step that completes normally leaves neither abnormal-stop record.
#[tokio::test]
async fn completed_step_leaves_no_abnormal_stop_records() {
    let provider = MockProvider::new(vec![vec![
        text_delta("all done"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &default_config(),
        None,
    )
    .await;
    let _ = assert_completed(result);

    assert!(customs_of_type(&store, "loop.step_stopped").is_empty());
    assert!(customs_of_type(&store, "loop.partial_output").is_empty());
}

/// Gap 7, the post-assembly window: the provider call completes and
/// assembles, but a `PostLlm` hook (arbitrary user shell hooks run here)
/// hangs until the step timeout fires — BEFORE `persist_assistant_turn`
/// appends the `AssistantMessage`. The capture must stay armed through
/// that window so the complete response survives as the durable
/// `loop.partial_output` record instead of vanishing entirely.
#[tokio::test(start_paused = true)]
async fn timeout_during_post_llm_hook_persists_the_assembled_response_as_partial() -> TestResult {
    struct HangingPostLlm;
    #[async_trait::async_trait]
    impl crate::integration::hooks::PostLlmHook for HangingPostLlm {
        async fn after_llm(&self, _summary: &crate::integration::hooks::LlmCallSummary) {
            std::future::pending::<()>().await;
        }
    }

    let provider = MockProvider::new(vec![vec![
        thinking_delta("mulling it over "),
        text_delta("the answer "),
        text_delta("is 4"),
        refusal_delta("stale refusal preview"),
        completed_message_item(
            "msg_post_llm",
            &serde_json::json!([
                {
                    "type": "output_text",
                    "text": "the answer is 4",
                    "annotations": [],
                    "logprobs": [],
                },
                {"type": "refusal", "refusal": "canonical refusal"},
            ]),
        )?,
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut hooks = crate::integration::hooks::HookRegistry::new();
    hooks.register(crate::integration::hooks::Hook::PostLlm(Box::new(
        HangingPostLlm,
    )));
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_agent_step(AgentStepRequest {
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
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));

    // The cut landed after assembly but before the durable append: no
    // AssistantMessage may exist for the turn...
    assert!(
        !store
            .events()
            .iter()
            .any(|e| matches!(e, SessionEvent::AssistantMessage { .. })),
        "the turn never reached persist_assistant_turn",
    );
    // ...so the partial record is the ONLY durable copy of the complete
    // response, and it must carry everything the stream produced.
    let partials = customs_of_type(&store, "loop.partial_output");
    assert_eq!(partials.len(), 1, "exactly one partial-output record");
    let (_, partial) = &partials[0];
    assert_eq!(partial["stop_reason"].as_str(), Some("timeout"));
    assert_eq!(partial["hard_cut"].as_bool(), Some(true));
    assert_eq!(partial["text"].as_str(), Some("the answer is 4"));
    assert_eq!(partial["thinking"].as_str(), Some("mulling it over "));
    assert_eq!(partial["refusal"].as_str(), Some("canonical refusal"));

    let stops = customs_of_type(&store, "loop.step_stopped");
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].1["stop_reason"].as_str(), Some("timeout"));
    Ok(())
}
