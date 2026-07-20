use super::*;
use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};
use crate::session::ProviderStateProvenance;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct HangAfterDurableAssistant;

#[async_trait::async_trait]
impl SessionEventHook for HangAfterDurableAssistant {
    async fn on_event(&self, event: &SessionEvent) {
        if matches!(event, SessionEvent::AssistantMessage { .. }) {
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_in_response_event_hook_never_duplicates_durable_output_as_partial() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("durable answer"),
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: Some("resp_durable-before-hook".to_owned()),
        },
    ]]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(HangAfterDurableAssistant)));
    let mut loop_context = LoopContext::new("system");
    loop_context.hooks = Some(Arc::new(hooks));

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
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));

    let events = store.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SessionEvent::AssistantMessage { .. }))
            .count(),
        1,
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(ProviderStateProvenance::from_event(event), Ok(Some(_))))
            .count(),
        1,
    );
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            SessionEvent::Custom { event_type, .. } if event_type == "loop.partial_output"
        )
    }));
    Ok(())
}
