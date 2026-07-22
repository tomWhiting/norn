use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn half_wired_pending_mailbox_fails_before_step_side_effects() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("must not run"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let mut loop_context = LoopContext::new("system");
    loop_context.agent_id = Some(uuid::Uuid::new_v4());
    loop_context.pending_agent_messages = Some(Arc::new(crate::agent::PendingAgentMessages::new()));

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "must not persist",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await;

    assert!(matches!(
        result,
        Err(NornError::Session(
            crate::error::SessionError::StorageError { .. }
        ))
    ));
    assert!(
        store.events().is_empty(),
        "mailbox validation must precede prompt persistence",
    );
    assert!(
        provider.requests()?.is_empty(),
        "mailbox validation must precede provider dispatch",
    );
    Ok(())
}

#[tokio::test]
async fn public_pending_mailbox_installer_binds_the_exact_step_store() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("complete"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = Arc::new(EventStore::new());
    let executor = MockToolExecutor::empty();
    let binding = crate::session::SessionBinding::ephemeral_root();
    let agent_id = uuid::Uuid::new_v4();
    let mut loop_context = LoopContext::new("system");
    let pending = loop_context.install_pending_mailbox(agent_id, &binding, &store)?;

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: store.as_ref(),
        user_prompt: "run",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("complete".to_owned()));
    assert!(pending.is_empty());
    assert_eq!(provider.requests()?.len(), 1);
    Ok(())
}
