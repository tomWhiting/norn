use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- Test 1: Two-turn tool interaction (R2) ---------------------------

#[tokio::test]
async fn two_turn_tool_interaction() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"foo.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"42"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "hello"}))),
    );
    let executor = MockToolExecutor::new(handlers);
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

    let (output, usage) = assert_completed(result);
    assert_eq!(output["answer"], "42");
    assert!(usage.input_tokens > 0);
    assert!(store.len() >= 4);
}

// -- Encrypted reasoning threads into the next iteration's request --

#[tokio::test]
async fn reasoning_items_threaded_into_next_request_messages() -> TestResult {
    // Seam regression: a reasoning output item captured on a tool-call
    // turn must ride the in-memory assistant Message so the next
    // provider request can replay it (stateless threading). The mock
    // provider records every request it receives; the second request's
    // assistant message must carry the captured item.
    let reasoning_item = crate::provider::reasoning::ReasoningItem {
        id: "rs_1".to_owned(),
        summary: vec![
            crate::provider::reasoning::ReasoningSummaryPart::SummaryText {
                text: "planning the tool call".to_owned(),
            },
        ],
        content: None,
        encrypted_content: Some("opaque-blob".to_owned()),
    };
    let turn1 = vec![
        ProviderEvent::ReasoningItemDone {
            item: reasoning_item.clone(),
        },
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"foo.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"42"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "hello"}))),
    );
    let executor = MockToolExecutor::new(handlers);
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
    assert_eq!(output["answer"], "42");

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2, "two provider turns expected");
    let assistant = requests[1]
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Assistant)
        .ok_or_else(|| std::io::Error::other("second request did not replay assistant turn"))?;
    assert_eq!(
        assistant.reasoning,
        vec![reasoning_item],
        "captured reasoning must thread into the next request's messages",
    );
    Ok(())
}

// -- Thinking is threaded from AssembledResponse into SessionEvent --

#[tokio::test]
async fn thinking_delta_threaded_into_assistant_message() -> TestResult {
    let events = vec![
        thinking_delta("first let me reason"),
        text_delta("The answer is 42."),
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

    let (_, _) = assert_completed(result);
    let assistant_msg = store
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::AssistantMessage {
                content, thinking, ..
            } => Some((content.clone(), thinking.clone())),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("no AssistantMessage was persisted"))?;
    assert_eq!(assistant_msg.0, "The answer is 42.");
    assert_eq!(assistant_msg.1, "first let me reason");
    Ok(())
}

#[tokio::test]
async fn unmoored_program_caller_fails_before_assistant_persistence_or_repair() -> TestResult {
    let raw = serde_json::json!({
        "type": "function_call",
        "id": "fc_unmoored",
        "call_id": "call_unmoored",
        "name": "read_file",
        "arguments": "{}",
        "caller": {"type": "program", "caller_id": "program_missing"}
    });
    let item = ResponseItem::from_value(raw)?;
    let provider = MockProvider::new(vec![vec![
        ProviderEvent::ResponseItemDone {
            item: ResponseTranscriptItem {
                item,
                provenance: ResponseStreamProvenance::default(),
            },
        },
        done_event(StopReason::ToolUse),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let mut loop_context = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
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
        Err(NornError::Provider(
            ProviderError::ResponseProtocolViolation {
                source: crate::provider::openai::response_reconciler::ResponseReconciliationError::UnmooredProgramCaller
            }
        ))
    ));
    assert!(
        store.events().iter().all(|event| !matches!(
            event,
            SessionEvent::AssistantMessage { .. } | SessionEvent::ToolResult { .. }
        )),
        "invalid program call must never become replayable or receive synthetic repair",
    );
    Ok(())
}

// -- Test 2: Text-only no-schema -> Completed with Value::String (R10)

#[tokio::test]
async fn text_only_no_schema_completes() {
    let events = vec![
        text_delta("The answer is 42."),
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

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("The answer is 42.".to_string()));
}

#[tokio::test]
async fn custom_tool_call_kind_propagated_to_session_event() -> TestResult {
    let events = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "ctc_1".to_string(),
            call_id: None,
            name: Some("apply_patch".to_string()),
            arguments_delta: "patch content".to_string(),
            kind: crate::provider::request::ToolCallKind::Custom,
        },
        ProviderEvent::ToolCallComplete {
            call_id: "call_custom".to_string(),
            name: "apply_patch".to_string(),
            arguments: "patch content".to_string(),
            kind: crate::provider::request::ToolCallKind::Custom,
        },
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![
        events,
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "apply_patch".to_string(),
        Box::new(|_| Ok(serde_json::json!({"applied": true}))),
    );
    let executor = MockToolExecutor::new(handlers);

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[ToolDefinition {
            name: "apply_patch".to_string(),
            description: "Apply a patch".to_string(),
            parameters: serde_json::json!({}),
        }],
        None,
        &default_config(),
        None,
    )
    .await;

    let assistant_event = store.events().into_iter().find_map(|e| {
        if let SessionEvent::AssistantMessage { tool_calls, .. } = e
            && !tool_calls.is_empty()
        {
            return Some(tool_calls);
        }
        None
    });
    let tool_calls = assistant_event
        .ok_or_else(|| std::io::Error::other("AssistantMessage had no tool calls"))?;
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(
        tool_calls[0].kind,
        crate::provider::request::ToolCallKind::Custom,
        "ToolCallEvent.kind must propagate Custom from AssembledToolCall, not hardcode Function",
    );
    assert_eq!(tool_calls[0].call_id, "call_custom");
    Ok(())
}
