use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- Post-batch steer drains in the schema arms ------------------------

fn request_carries_frame(request: &ProviderRequest, needle: &str) -> bool {
    request.messages.iter().any(|m| {
        m.content
            .as_deref()
            .is_some_and(|c| c.contains("<agent_message") && c.contains(needle))
    })
}

/// Regression (steer drain missing from the `SchemaInvalid` arm): a
/// steer arriving during the pre-schema tool batch of a failed
/// validation attempt must be injected before the retry's provider
/// request — "immediately after the current tool batch" — not parked
/// until some later boundary.
#[tokio::test]
async fn steer_during_schema_invalid_batch_reaches_the_retry_request() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"wrong":1}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema2",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let schema = simple_schema();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "steer during invalid batch",
        crate::r#loop::inbound::MessageKind::Steer,
    ));

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let retry = requests
        .get(1)
        .ok_or_else(|| std::io::Error::other("provider recorded no schema retry request"))?;
    assert!(
        request_carries_frame(retry, "steer during invalid batch"),
        "the schema-retry request must already carry the framed steer",
    );
    Ok(())
}

/// Contract pin for the `ToolsAndSchemaValid` arm: a steer arriving
/// during the pre-schema batch is injected once every call has its
/// result, and the loop continues so the next provider request sees
/// it (the model must never stop past an undelivered steer).
#[tokio::test]
async fn steer_during_tools_and_schema_valid_batch_reaches_the_model() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"early"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema2",
            Some("structured_output"),
            r#"{"answer":"final"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let schema = simple_schema();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "steer during valid batch",
        crate::r#loop::inbound::MessageKind::Steer,
    ));

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(
        output["answer"], "final",
        "the loop must continue past the steer instead of returning the pre-steer output",
    );

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let continuation = requests
        .get(1)
        .ok_or_else(|| std::io::Error::other("provider recorded no continuation request"))?;
    assert!(
        request_carries_frame(continuation, "steer during valid batch"),
        "the continuation request must carry the framed steer",
    );
    Ok(())
}
