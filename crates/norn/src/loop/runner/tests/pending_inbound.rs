use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- R7 regression: idle-root inbound reaches first request ----------

/// A message delivered while the root loop is idle between user turns
/// must be injected before the next provider request. Without the
/// pre-request drain this message would only surface at the stop
/// boundary of the second step, wasting a provider turn and delaying
/// the push.
#[tokio::test]
async fn inbound_message_queued_between_steps_reaches_next_request() -> TestResult {
    let provider = MockProvider::new(vec![
        vec![text_delta("first"), done_event(StopReason::EndTurn)],
        vec![text_delta("second"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let mut loop_ctx = LoopContext::new("system");

    let first = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (first_output, _) = assert_completed(first);
    assert_eq!(first_output, Value::String("first".to_string()));

    tx.send(make_channel_message(
        "spawn/worker",
        "idle push",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    ))
    .await?;

    let second = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (second_output, _) = assert_completed(second);
    assert_eq!(second_output, Value::String("second".to_string()));

    let requests = provider.requests()?;
    assert_eq!(
        requests.len(),
        2,
        "idle inbound should not force an extra provider turn",
    );
    let second_request_text = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("<agent_message from=\"spawn/worker\"")
            && second_request_text.contains("kind=\"update\"")
            && second_request_text.contains("\nidle push\n"),
        "second request must include idle inbound message: {second_request_text}",
    );
    Ok(())
}

#[tokio::test]
async fn message_seeded_step_records_delivery_without_empty_prompt() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("handled"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    let mut message = make_channel_message(
        "spawn/worker",
        "wake root",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    );
    message.seq = Some(1);
    let message_id = message.id;

    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        initial_messages: vec![message],
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("handled".to_string()));

    let events = store.events();
    let user_messages = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1, "no synthetic empty prompt");
    assert!(user_messages[0].contains("<agent_message from=\"spawn/worker\""));
    assert!(user_messages[0].contains("\nwake root\n"));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Custom {
                event_type,
                data,
                ..
            } if event_type == crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE
                && data["message_id"] == message_id.to_string()
        )
    }));

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 1);
    let request_text = requests[0]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("<agent_message from=\"spawn/worker\"")
            && request_text.contains("kind=\"steer\"")
            && request_text.contains("\nwake root\n"),
        "request must include delivered wake message: {request_text}",
    );
    Ok(())
}

#[tokio::test]
async fn pending_agent_message_reaches_next_request() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("handled"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let pending = std::sync::Arc::new(crate::agent::PendingAgentMessages::new());
    let queued = make_channel_message(
        "spawn/worker",
        "durable push",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    );
    let message_id = queued.id;
    let message_id_string = message_id.to_string();
    let queued_at = queued.timestamp;
    let to_label = "/root".to_owned();
    let mut queued = queued;
    queued.to_id = agent_id;
    pending
        .queue(crate::agent::PendingAgentMessage::new(
            queued, to_label, queued_at,
        ))
        .ok_or_else(|| std::io::Error::other("pending message was not queued"))?;

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.agent_id = Some(agent_id);
    loop_ctx.pending_agent_messages = Some(std::sync::Arc::clone(&pending));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("handled".to_string()));
    assert!(pending.is_empty(), "pending message must be drained once");

    let requests = provider.requests()?;
    let request_text = requests[0]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("<agent_message from=\"spawn/worker\"")
            && request_text.contains("kind=\"update\"")
            && request_text.contains("\ndurable push\n"),
        "first request must include queued agent message: {request_text}",
    );
    assert!(
        store.events().iter().any(|event| matches!(
            event,
            SessionEvent::Custom { event_type, data, .. }
                if event_type == crate::agent::AGENT_MESSAGE_DEQUEUED_EVENT_TYPE
                    && data.get("message_id").and_then(serde_json::Value::as_str)
                        == Some(message_id_string.as_str())
        )),
        "draining the pending message must append a dequeued audit event",
    );
    Ok(())
}

#[tokio::test]
async fn pending_message_seeded_step_resumes_without_empty_prompt() -> TestResult {
    let provider = MockProvider::new(vec![vec![
        text_delta("resumed"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let pending = std::sync::Arc::new(crate::agent::PendingAgentMessages::new());
    let mut queued = make_channel_message(
        "spawn/worker",
        "durable resume",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    );
    queued.to_id = agent_id;
    let message_id = queued.id;
    let queued_at = queued.timestamp;
    pending
        .queue(crate::agent::PendingAgentMessage::new(
            queued,
            "/root".to_owned(),
            queued_at,
        ))
        .ok_or_else(|| std::io::Error::other("pending resume message was not queued"))?;

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.agent_id = Some(agent_id);
    loop_ctx.pending_agent_messages = Some(std::sync::Arc::clone(&pending));

    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        initial_messages: Vec::new(),
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("resumed".to_string()));

    let events = store.events();
    let user_messages = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1, "no synthetic empty prompt");
    assert!(user_messages[0].contains("\ndurable resume\n"));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Custom {
                event_type,
                data,
                ..
            } if event_type == crate::agent::AGENT_MESSAGE_DEQUEUED_EVENT_TYPE
                && data["message_id"] == message_id.to_string()
        )
    }));
    assert!(pending.is_empty(), "resume step drains pending store");
    Ok(())
}
