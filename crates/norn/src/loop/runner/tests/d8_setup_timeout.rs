use super::*;
use crate::profile::PromptCommand;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn setup_finishes_durably_before_an_exhausted_step_budget_returns() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let finished_marker = working_dir.path().join("command-finished");
    let mut loop_context = LoopContext::with_working_dir(
        "system",
        crate::tool::context::SharedWorkingDir::new(working_dir.path().to_path_buf()),
    );
    loop_context.prompt_commands.push(PromptCommand {
        name: "slow-setup".to_owned(),
        command: "sleep 0.2; printf finished > command-finished".to_owned(),
        cache_ttl: None,
    });
    let provider = MockProvider::new(Vec::new());
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(100)),
        prompt_command_timeout: Some(Duration::from_secs(1)),
        ..AgentLoopConfig::default()
    };

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "D8-DURABLE-BEFORE-TIMEOUT",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;

    assert!(matches!(result, AgentStepResult::TimedOut { .. }));
    assert_eq!(
        provider.call_count(),
        0,
        "an exhausted budget cannot call the provider"
    );
    assert!(
        !finished_marker.exists(),
        "the command timeout must be clamped to the remaining step budget",
    );
    let durable_prompts = store
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content == "D8-DURABLE-BEFORE-TIMEOUT"
            )
        })
        .count();
    assert_eq!(
        durable_prompts, 1,
        "the accepted prompt must remain durable exactly once"
    );
    Ok(())
}
