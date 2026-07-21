use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Register a `PreLlmHook` backed by an atomic counter that blocks on
/// the third call. Drive a mock provider whose first two turns make
/// tool calls so the loop keeps running; the third turn must return
/// `Err(NornError::HookBlocked)`.
#[tokio::test]
async fn pre_llm_hook_blocks_after_three_calls() -> TestResult {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::error::HookType;
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreLlmHook};
    use crate::provider::request::ProviderRequest;

    struct BlockOnThird {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PreLlmHook for BlockOnThird {
        async fn before_llm(&self, _req: &ProviderRequest) -> HookOutcome {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= 3 {
                HookOutcome::Block {
                    reason: "third strike".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"a"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("read_file"), r#"{"path":"b"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn3 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"never"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(BlockOnThird {
        calls: Arc::clone(&calls),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let tools = [read_file_tool_def()];
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &tools,
        output_schema: Some(&schema),
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await;

    let Err(NornError::HookBlocked { hook_type, reason }) = result else {
        return Err(
            std::io::Error::other(format!("expected PreLlm HookBlocked, got {result:?}")).into(),
        );
    };
    assert_eq!(hook_type, HookType::PreLlm);
    assert_eq!(reason, "third strike");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "hook must have observed exactly three calls",
    );
    Ok(())
}

/// Register a `SessionEventHook` that increments an atomic counter on
/// every event. After a two-turn loop with one tool call and a
/// structured-output finish, the counter must equal the number of
/// events visible from `store.events()`.
#[tokio::test]
async fn session_event_hook_counts_all_appends() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};

    struct CountAll {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl SessionEventHook for CountAll {
        async fn on_event(&self, _event: &SessionEvent) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let counter = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(CountAll {
        counter: Arc::clone(&counter),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let stored = store.len();
    assert!(stored >= 4, "expected at least 4 events, got {stored}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        stored,
        "session-event hook must fire once per stored event",
    );
}
