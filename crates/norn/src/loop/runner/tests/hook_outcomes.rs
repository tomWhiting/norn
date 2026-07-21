use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// NH-006 R3 / C54: a UserPromptHook returning Block must short-
// circuit the loop entry. The agent step returns
// `NornError::HookBlocked { hook_type: UserPrompt, .. }` and no
// provider call is dispatched.
#[tokio::test]
async fn user_prompt_hook_block_returns_hook_blocked_error() -> TestResult {
    use crate::error::HookType;
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, UserPromptHook};

    struct AlwaysBlock;
    #[async_trait::async_trait]
    impl UserPromptHook for AlwaysBlock {
        async fn on_user_prompt(&self, _prompt: &str, _session_id: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "not allowed".to_owned(),
            }
        }
    }

    let provider = MockProvider::new(Vec::new());
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::UserPrompt(Box::new(AlwaysBlock)));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let tools = [read_file_tool_def()];
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hello",
        tools: &tools,
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await;

    let Err(NornError::HookBlocked { hook_type, reason }) = result else {
        return Err(std::io::Error::other(format!(
            "expected UserPrompt HookBlocked, got {result:?}"
        ))
        .into());
    };
    assert_eq!(hook_type, HookType::UserPrompt);
    assert_eq!(reason, "not allowed");
    Ok(())
}

// NH-006 R7 / C59: PostToolFailureHook fires (additively to the
// existing PostToolHook) when a tool returns an error output. The
// counter increments on the erroring tool only; successful tool
// calls in the same turn do not fire it.
#[tokio::test]
async fn post_tool_failure_hook_fires_only_on_error_output() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookRegistry, PostToolFailureHook, PostToolHook};

    struct CountFailure {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PostToolFailureHook for CountFailure {
        async fn after_tool_failure(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _output: &crate::tool::traits::ToolOutput,
            _ctx: &crate::tool::context::ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct CountSuccess {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PostToolHook for CountSuccess {
        async fn after_tool(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _output: &crate::tool::traits::ToolOutput,
            _ctx: &crate::tool::context::ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "always_fails".to_string(),
        Box::new(|_| {
            Err(crate::error::ToolError::ExecutionFailed {
                reason: "boom".to_owned(),
            })
        }),
    );

    let turn1 = vec![
        tool_call_delta("tc_fail", Some("always_fails"), r"{}"),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_done",
            Some("structured_output"),
            r#"{"answer":"finished"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let failure_count = Arc::new(AtomicUsize::new(0));
    let success_count = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PostToolFailure(Box::new(CountFailure {
        counter: Arc::clone(&failure_count),
    })));
    hooks.register(Hook::PostTool(Box::new(CountSuccess {
        counter: Arc::clone(&success_count),
    })));

    let tool_def = ToolDefinition {
        name: "always_fails".to_string(),
        description: "Always fails".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[tool_def],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let _ = assert_completed(result);
    assert_eq!(
        failure_count.load(Ordering::SeqCst),
        1,
        "PostToolFailureHook fires once for the erroring tool call",
    );
    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "PostToolHook fires once for the erroring tool call in this path",
    );
}

// NH-006 R4 / C55: a StopHook returning Block once then Proceed
// forces the loop to take one extra iteration with the block reason
// injected as a user message, then complete normally on the second
// round.
#[tokio::test]
async fn stop_hook_block_forces_extra_iteration() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, StopHook};

    struct BlockOnce {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl StopHook for BlockOnce {
        async fn on_stop(&self, _final_text: &str) -> HookOutcome {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                HookOutcome::Block {
                    reason: "keep going".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        ProviderEvent::TextDelta {
            text: "round one".to_owned(),
        },
        done_event(StopReason::EndTurn),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "round two".to_owned(),
        },
        done_event(StopReason::EndTurn),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Stop(Box::new(BlockOnce {
        calls: Arc::clone(&calls),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hi",
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

    let AgentStepResult::Completed { output, .. } = result else {
        return Err(std::io::Error::other(format!("expected Completed, got {result:?}")).into());
    };
    assert_eq!(output, Value::String("round two".to_owned()));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "StopHook must observe both terminal classifications",
    );
    Ok(())
}
