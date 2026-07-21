use super::*;

#[tokio::test]
async fn explicit_continue_turn_replays_text_then_completes()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = MockProvider::new(vec![
        vec![
            text_delta("first response"),
            done_event(StopReason::ContinueTurn),
        ],
        vec![
            text_delta("final response"),
            done_event(StopReason::EndTurn),
        ],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

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

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("final response".to_owned()));
    assert_eq!(provider.call_count(), 2);

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.content.as_deref() == Some("first response")
    }));

    let stop_reasons: Vec<_> = store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { stop_reason, .. } => Some(stop_reason),
            _ => None,
        })
        .collect();
    assert_eq!(stop_reasons, ["continue_turn", "end_turn"]);
    Ok(())
}

#[tokio::test]
async fn explicit_continue_turn_accepts_empty_output() {
    let provider = MockProvider::new(vec![
        vec![done_event(StopReason::ContinueTurn)],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

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

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("done".to_owned()));
    assert_eq!(provider.call_count(), 2);
}

#[tokio::test]
async fn explicit_continue_turn_with_schema_does_not_nudge_or_consume_budget() {
    let provider = MockProvider::new(vec![
        vec![
            text_delta("intermediate"),
            done_event(StopReason::ContinueTurn),
        ],
        vec![
            tool_call_delta(
                "tc_schema",
                Some("structured_output"),
                r#"{"answer":"accepted"}"#,
            ),
            done_event(StopReason::ToolUse),
        ],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(1),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "accepted");
    assert_eq!(provider.call_count(), 2);
    assert!(!store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content.contains("Call the structured_output tool")
        )
    }));
}

#[tokio::test]
async fn schema_call_precedes_an_explicit_continue_directive() {
    let provider = MockProvider::new(vec![vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"accepted"}"#,
        ),
        done_event(StopReason::ContinueTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "accepted");
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn custom_tool_call_precedes_an_explicit_continue_directive() {
    let provider = MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "ctc_continue".to_owned(),
                call_id: None,
                name: Some("apply_patch".to_owned()),
                arguments_delta: "patch content".to_owned(),
                kind: crate::provider::request::ToolCallKind::Custom,
            },
            ProviderEvent::ToolCallComplete {
                call_id: "call_continue".to_owned(),
                name: "apply_patch".to_owned(),
                arguments: "patch content".to_owned(),
                kind: crate::provider::request::ToolCallKind::Custom,
            },
            done_event(StopReason::ContinueTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler_executions = Arc::clone(&executions);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "apply_patch".to_owned(),
        Box::new(move |_| {
            handler_executions.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"applied": true}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "apply_patch".to_owned(),
        description: "Apply a patch".to_owned(),
        parameters: serde_json::json!({}),
    }];

    let result = run_step(
        &provider,
        &executor,
        &store,
        &tools,
        None,
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("done".to_owned()));
    assert_eq!(provider.call_count(), 2);
    assert!(store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::AssistantMessage { tool_calls, .. }
                if tool_calls.iter().any(|tool| {
                    tool.kind == crate::provider::request::ToolCallKind::Custom
                        && tool.call_id == "call_continue"
            })
        )
    }));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the custom tool must execute exactly once before continuation",
    );
}

#[tokio::test]
async fn explicit_continue_turn_replays_refusal_before_later_completion()
-> Result<(), Box<dyn std::error::Error>> {
    let refusal = completed_message_item(
        "msg_continue_refusal",
        &serde_json::json!([{"type": "refusal", "refusal": "not terminal yet"}]),
    )?;
    let provider = MockProvider::new(vec![
        vec![refusal, done_event(StopReason::ContinueTurn)],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

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

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("done".to_owned()));
    assert_eq!(provider.call_count(), 2);
    let requests = provider.requests()?;
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.response_items.iter().any(|entry| {
                entry.item.id() == Some("msg_continue_refusal") && entry.item.as_message().is_some()
            })
    }));
    Ok(())
}

#[tokio::test]
async fn empty_canonical_refusal_returns_refused_without_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let item = ResponseItem::from_value(serde_json::json!({
        "type": "message",
        "id": "msg_empty_refusal",
        "role": "assistant",
        "status": "completed",
        "content": [{"type": "refusal", "refusal": ""}]
    }))?;
    let events = vec![
        ProviderEvent::ResponseItemDone {
            item: ResponseTranscriptItem {
                item,
                provenance: ResponseStreamProvenance {
                    item_id: Some("msg_empty_refusal".to_owned()),
                    output_index: Some(0),
                    content_index: None,
                    sequence_number: Some(1),
                },
            },
        },
        done_event(StopReason::EndTurn),
    ];
    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

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
    let AgentStepResult::Refused {
        refusal,
        iterations,
        usage,
        ..
    } = result
    else {
        return Err(std::io::Error::other("empty refusal did not stop as Refused").into());
    };
    assert!(refusal.is_empty());
    assert_eq!(iterations, 1);
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(provider.call_count(), 1);

    let persisted = store.events().into_iter().find_map(|event| match event {
        SessionEvent::AssistantMessage { response_items, .. } => Some(response_items),
        _ => None,
    });
    let Some(response_items) = persisted else {
        return Err(std::io::Error::other("refusal turn was not persisted").into());
    };
    let Some(message) = response_items
        .first()
        .and_then(|entry| entry.item.as_message())
    else {
        return Err(std::io::Error::other("persisted refusal item was not a message").into());
    };
    assert!(matches!(
        message.content(),
        [ResponseContentPart::Refusal { refusal, .. }] if refusal.is_empty()
    ));
    Ok(())
}
