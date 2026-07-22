use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- Undelivered follow-up (Update) re-queue on abnormal exits ---------

/// Regression (follow-up buffer dropped on abnormal exits): an Update
/// drained mid-step buffers for the next stop boundary; a
/// `MaxIterations` exit never reaches one, so the message must land in
/// the durable pending store instead of vanishing.
#[tokio::test]
async fn max_iterations_exit_requeues_buffered_updates() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1]);
    let store = Arc::new(EventStore::new());
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi from mid-batch",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;
    let config = AgentLoopConfig {
        max_iterations: Some(1),
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    assert!(matches!(
        result,
        AgentStepResult::MaxIterationsReached { .. }
    ));
    assert_update_requeued(&pending, agent_id, &store, "fyi from mid-batch");
    Ok(())
}

/// Regression: a hard provider error (here an in-band typed Error
/// event ending the second turn) propagates as the step's `Err` —
/// and the Update buffered during turn 1's batch must still be
/// re-queued durably, not dropped with the failed future.
#[tokio::test]
async fn provider_error_exit_requeues_buffered_updates() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![ProviderEvent::Error {
        error: ProviderError::QuotaExceeded,
    }];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = Arc::new(EventStore::new());
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi before the crash",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;

    let err = match run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    {
        Err(error) => error,
        Ok(result) => {
            return Err(std::io::Error::other(format!(
                "expected the typed provider error, received {result:?}"
            ))
            .into());
        }
    };
    assert!(
        matches!(err, NornError::Provider(ProviderError::QuotaExceeded)),
        "the step must surface the provider's typed error, got {err:?}",
    );
    assert_update_requeued(&pending, agent_id, &store, "fyi before the crash");
    Ok(())
}

/// Regression: a `step_timeout` cut drops the inner future wherever
/// it is suspended; the Update buffered before the cut must survive
/// into the durable pending store.
#[tokio::test(start_paused = true)]
async fn step_timeout_exit_requeues_buffered_updates() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        text_delta("never observed"),
        done_event(StopReason::EndTurn),
    ];
    // 100ms before each turn's first event: turn 1 completes inside
    // the 150ms budget, turn 2 cannot.
    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(100));
    let store = Arc::new(EventStore::new());
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi before the timeout",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(150)),
        ..AgentLoopConfig::default()
    };

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
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));
    assert_update_requeued(&pending, agent_id, &store, "fyi before the timeout");
    Ok(())
}

/// Gap 10 (session-fidelity inventory): the `agent_message.queued` audit
/// is the only durable copy of a re-queued message — when its sink append
/// fails on a step that would otherwise report success, the step fails
/// typed instead, and the in-memory pending record is retained
/// (redeliverable while the process lives).
#[tokio::test]
async fn queued_audit_sink_failure_fails_an_otherwise_successful_step() -> TestResult {
    /// Sink that fails exactly the queued-audit append, so every primary
    /// write of the step succeeds and only the secondary contract is
    /// exercised.
    struct QueuedAuditFailingSink;
    impl crate::session::store::PersistenceSink for QueuedAuditFailingSink {
        fn persist(
            &mut self,
            event: &SessionEvent,
        ) -> Result<(), crate::session::persistence::SessionPersistError> {
            if matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE
            ) {
                return Err(crate::session::persistence::SessionPersistError::Io(
                    std::io::Error::other("disk full"),
                ));
            }
            Ok(())
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1]);
    let store = Arc::new(EventStore::with_sink(Box::new(QueuedAuditFailingSink)));
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi from mid-batch",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;
    let config = AgentLoopConfig {
        max_iterations: Some(1),
        ..AgentLoopConfig::default()
    };

    let err = match run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    {
        Err(error) => error,
        Ok(result) => {
            return Err(std::io::Error::other(format!(
                "expected the queued-audit persistence error, received {result:?}"
            ))
            .into());
        }
    };
    assert!(
        matches!(err, NornError::Session(_)),
        "expected the typed session error, got {err:?}",
    );
    // The message did not vanish: the in-memory pending record survives
    // the failed audit and stays redeliverable while the process lives.
    let queued = pending.messages_for_delivery(agent_id);
    assert_eq!(queued.len(), 1, "pending record retained on audit failure");
    assert_eq!(queued[0].content, "fyi from mid-batch");
    // And no durable queued-audit line claims otherwise.
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(!audited, "the failed audit must not reach the store");
    Ok(())
}
