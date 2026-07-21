use super::*;

// -- Test 12: Streaming events forwarded to broadcast channel (R9) ----

#[tokio::test]
async fn streaming_events_forwarded_to_broadcast() {
    use crate::provider::agent_event::{AgentEvent, AgentEventKind, AgentEventSender};
    use uuid::Uuid;

    let events = vec![
        text_delta("hello"),
        text_delta(" world"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &default_config(),
        Some(&sender),
    )
    .await;

    let mut received = Vec::new();
    let mut unexpected_events = 0;
    while let Ok(agent_event) = rx.try_recv() {
        match agent_event.event {
            AgentEventKind::Provider(event) => received.push(event),
            AgentEventKind::UsageEstimate(_) => {}
            AgentEventKind::Subagent(_)
            | AgentEventKind::Message(_)
            | AgentEventKind::StreamRetry(_)
            | AgentEventKind::Compaction(_) => {
                unexpected_events += 1;
            }
        }
    }

    assert_eq!(unexpected_events, 0, "only provider events are expected");
    assert_eq!(received.len(), 3, "should receive all 3 events");
    assert!(matches!(&received[0], ProviderEvent::TextDelta { text } if text == "hello"));
    assert!(matches!(&received[1], ProviderEvent::TextDelta { text } if text == " world"));
    assert!(matches!(&received[2], ProviderEvent::Done { .. }));
}

// -- Test 13: Nudge contains tool name + schema + instruction (R8) ----

#[tokio::test]
async fn nudge_contains_required_content() {
    let turn1 = vec![text_delta("analyzing"), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        text_delta("still analyzing"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(2),
        None,
    )
    .await;

    let events = store.events();
    let nudge_content = events.iter().find_map(|e| {
        if let SessionEvent::UserMessage { content, .. } = e
            && content.contains("structured_output")
        {
            return Some(content.clone());
        }
        None
    });

    assert!(nudge_content.is_some(), "nudge message should exist");
    let content = nudge_content.unwrap_or_default();
    assert!(
        content.contains("structured_output"),
        "nudge must contain tool name"
    );
    assert!(
        content.contains("answer"),
        "nudge must contain schema field names"
    );
    assert!(
        content.contains("Call the structured_output tool"),
        "nudge must contain instruction"
    );
}

// -- Test 14: No-schema + tool then text (R10) ------------------------

#[tokio::test]
async fn no_schema_tool_then_text() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"bar"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        text_delta("file contained: bar"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "bar"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("file contained: bar".to_string()));

    let events = store.events();
    let tool_executed = events.iter().any(
        |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
    );
    assert!(tool_executed, "read_file should have been executed");
}
