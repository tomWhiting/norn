use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Provider that pushes a message into the recipient's own inbound
/// channel the moment it is called, then fails the turn with a typed
/// in-band error — modelling a send the channel accepted (and whose
/// sender was told `delivered: true`) after the loop's final inbound
/// drain: the deregistration message-loss window.
struct SendThenFailProvider {
    tx: crate::r#loop::inbound::InboundSender,
    content: &'static str,
    kind: crate::r#loop::inbound::MessageKind,
}

/// Provider that places a message in the live inbound channel and then never
/// yields an event. The outer step-timeout arm must still sweep that undrained
/// channel into durable pending storage.
struct SendThenStallProvider {
    tx: crate::r#loop::inbound::InboundSender,
    content: &'static str,
}

impl Provider for SendThenStallProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        if let Err(error) = self.tx.try_send(make_channel_message(
            "timeout-sender",
            self.content,
            crate::r#loop::inbound::MessageKind::Update,
            0,
        )) {
            return Err(ProviderError::StreamError {
                reason: format!("timeout fixture could not enqueue its message: {error}"),
                transient: None,
            });
        }
        Ok(Box::pin(stream::pending()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

impl Provider for SendThenFailProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.tx
            .try_send(make_channel_message(
                "late-sender",
                self.content,
                self.kind,
                0,
            ))
            .map_err(|error| ProviderError::StreamError {
                reason: format!("late-send fixture could not enqueue its message: {error}"),
                transient: None,
            })?;
        Ok(Box::pin(stream::iter(vec![Ok(ProviderEvent::Error {
            error: ProviderError::QuotaExceeded,
        })])))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// Regression (deregistration message-loss window): a message the
/// inbound channel accepted after the loop's final drain — pushed
/// here during the failing provider call, so no sweep ever ran after
/// it — must be re-queued into the durable pending store at step
/// exit, where the next step's flush and `wake_agent` eligibility
/// both see it. The message kind survives the round trip.
#[tokio::test]
async fn exit_sweeps_undrained_channel_messages_into_pending_store() -> TestResult {
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let provider = SendThenFailProvider {
        tx,
        content: "steer accepted mid-call",
        kind: crate::r#loop::inbound::MessageKind::Steer,
    };
    let store = Arc::new(EventStore::new());
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;

    let err = match run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
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
                "expected the in-band provider error, received {result:?}"
            ))
            .into());
        }
    };
    assert!(
        matches!(err, NornError::Provider(ProviderError::QuotaExceeded)),
        "the step surfaces the typed provider error, got {err:?}",
    );

    let queued = pending.messages_for_delivery(agent_id);
    assert_eq!(
        queued.len(),
        1,
        "the undrained channel message must be re-queued durably",
    );
    assert_eq!(queued[0].content, "steer accepted mid-call");
    assert_eq!(queued[0].kind, crate::r#loop::inbound::MessageKind::Steer);
    assert_eq!(
        queued[0].to_id, agent_id,
        "redelivery is re-targeted to this loop's agent",
    );
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(
        audited,
        "the sweep must append an agent_message.queued audit event",
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn timeout_arm_sweeps_undrained_channel_messages_into_pending_store() -> Result<(), NornError>
{
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(1);
    let provider = SendThenStallProvider {
        tx,
        content: "accepted before timeout",
    };
    let store = Arc::new(EventStore::new());
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(100)),
        ..AgentLoopConfig::default()
    };

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
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));
    assert_update_requeued(&pending, agent_id, &store, "accepted before timeout");
    Ok(())
}

#[tokio::test]
async fn full_try_send_hands_retained_message_to_awaited_send_losslessly()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::r#loop::inbound::InboundTrySendError;

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(1);
    let first = make_channel_message(
        "capacity-sender",
        "first buffered update",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    );
    let second = make_channel_message(
        "capacity-sender",
        "second awaited update",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    );
    tx.try_send(first)?;
    assert_eq!(
        tx.try_send(second.clone()),
        Err(InboundTrySendError::Full),
        "the non-blocking attempt must report Full without claiming delivery",
    );

    let provider = DelayedProvider::new(
        vec![
            vec![text_delta("turn one"), done_event(StopReason::EndTurn)],
            vec![text_delta("turn two"), done_event(StopReason::EndTurn)],
        ],
        Duration::from_millis(10),
    );
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    let config = default_config();
    let run = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    });
    let handoff = tx.send(second);
    let (result, delivery) = tokio::join!(run, handoff);
    delivery?;
    result?;

    let user_text = store
        .events()
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(user_text.contains("first buffered update"));
    assert!(user_text.contains("second awaited update"));
    Ok(())
}

/// A step that ends through a stop boundary has already flushed its
/// follow-ups into the conversation; nothing may be re-queued.
#[tokio::test]
async fn boundary_exit_leaves_nothing_to_requeue() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    // The boundary flush injects the buffered update and continues.
    let turn2 = vec![text_delta("done"), done_event(StopReason::EndTurn)];
    let turn3 = vec![text_delta("final"), done_event(StopReason::EndTurn)];
    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = Arc::new(EventStore::new());
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi delivered at stop",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id, &store)?;

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("final".to_string()));
    assert!(
        pending.is_empty(),
        "a boundary-delivered follow-up must not be re-queued",
    );
    let delivered = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::UserMessage { content, .. }
                if content.contains("fyi delivered at stop"),
        )
    });
    assert!(
        delivered,
        "the boundary flush must have injected the update"
    );
    Ok(())
}

/// Regression (partial inbound-injection failure dropped acknowledged
/// messages): when the persistence sink fails midway through injecting a
/// drained steer batch, the failing message and every not-yet-injected
/// message after it must be re-queued into the durable pending store on
/// the step-exit sweep — not dropped inside the moved batch. The
/// successfully-injected prefix stays delivered and is never re-queued.
#[tokio::test]
async fn inbound_injection_sink_failure_requeues_undelivered_remainder() -> TestResult {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::error::SessionError;
    use crate::session::persistence::SessionPersistError;
    use crate::session::store::PersistenceSink;

    // Fails the append of any `UserMessage` whose framed content carries
    // the marker; every other event persists normally.
    struct FailOnMarkerSink {
        marker: &'static str,
        tripped: Arc<AtomicBool>,
    }
    impl PersistenceSink for FailOnMarkerSink {
        fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
            if let SessionEvent::UserMessage { content, .. } = event
                && content.contains(self.marker)
            {
                self.tripped.store(true, Ordering::SeqCst);
                return Err(SessionPersistError::Io(std::io::Error::other(
                    "sink refused the second steer",
                )));
            }
            Ok(())
        }
    }

    // read_file sends three steers mid-batch; sorted by (no-seq, timestamp)
    // they keep send order, so the sink lets steer-1 through and fails
    // steer-2, leaving steer-3 never attempted.
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(move |_| {
            for marker in ["steer-1", "steer-2", "steer-3"] {
                tx.try_send(make_channel_message(
                    "mid-batch-sender",
                    marker,
                    crate::r#loop::inbound::MessageKind::Steer,
                    0,
                ))
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("mid-batch fixture could not enqueue its message: {error}"),
                })?;
            }
            Ok(serde_json::json!({"content": "file data"}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let tripped = Arc::new(AtomicBool::new(false));
    let store = Arc::new(EventStore::with_sink(Box::new(FailOnMarkerSink {
        marker: "steer-2",
        tripped: Arc::clone(&tripped),
    })));

    let provider = MockProvider::new(vec![vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ]]);
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
                "expected the mid-injection sink failure, received {result:?}"
            ))
            .into());
        }
    };
    assert!(
        matches!(err, NornError::Session(SessionError::StorageError { .. })),
        "the step must surface the sink's storage error, got {err:?}",
    );
    assert!(
        tripped.load(Ordering::SeqCst),
        "the sink must actually have failed a steer append",
    );

    // steer-2 (the failed append) and steer-3 (never attempted) must both
    // be durably re-queued; steer-1 was delivered and must not re-appear.
    let queued = pending.messages_for_delivery(agent_id);
    let contents: std::collections::BTreeSet<String> =
        queued.iter().map(|m| m.content.clone()).collect();
    let expected: std::collections::BTreeSet<String> =
        ["steer-2".to_string(), "steer-3".to_string()]
            .into_iter()
            .collect();
    assert_eq!(
        contents, expected,
        "the failed and un-injected steers must be re-queued, got {contents:?}",
    );
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(
        audited,
        "the re-queue must append agent_message.queued audit events",
    );
    Ok(())
}
