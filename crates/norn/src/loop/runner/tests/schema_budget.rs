use super::*;

// -- Test 3: Schema valid on first try (R4 case 1) --------------------

#[tokio::test]
async fn schema_valid_first_try() {
    let events = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"answer":"correct"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
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
    assert_eq!(output["answer"], "correct");
}

// -- Test 4: Schema invalid then valid (R4 case 2) --------------------

#[tokio::test]
async fn schema_invalid_then_valid() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"wrong":"field"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"fixed"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (output, usage) = assert_completed(result);
    assert_eq!(output["answer"], "fixed");
    assert_eq!(usage.input_tokens, 20);
}

// -- Test 5: Text stop then schema after nudge (R4 case 4) ------------

#[tokio::test]
async fn text_stop_then_schema_after_nudge() {
    let turn1 = vec![text_delta("thinking..."), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"answer":"nudged"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "nudged");

    let events = store.events();
    let has_nudge = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.contains("structured_output") && content.contains("schema")
        } else {
            false
        }
    });
    assert!(has_nudge, "nudge message should be in event store");
}

// -- Test 6: 3 text-only stops -> SchemaUnreachable (R7) ---------------

#[tokio::test]
async fn three_text_stops_schema_unreachable() {
    let responses: Vec<Vec<ProviderEvent>> = (0..3)
        .map(|_| {
            vec![
                text_delta("still thinking"),
                done_event(StopReason::EndTurn),
            ]
        })
        .collect();

    let provider = MockProvider::new(responses);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
}

// -- Test 7: 3 invalid schema calls -> SchemaUnreachable (R7) ---------

#[tokio::test]
async fn three_invalid_schema_calls_unreachable() {
    let responses: Vec<Vec<ProviderEvent>> = (0..3)
        .map(|i| {
            vec![
                tool_call_delta(
                    &format!("tc{i}"),
                    Some("structured_output"),
                    r#"{"wrong":"data"}"#,
                ),
                done_event(StopReason::ToolUse),
            ]
        })
        .collect();

    let provider = MockProvider::new(responses);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (best_attempt, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
    assert!(best_attempt.is_some());
}

// -- Test 8: 1 nudge + 2 invalid -> SchemaUnreachable(3) (R7) --------

#[tokio::test]
async fn nudge_plus_two_invalid_unreachable() {
    let turn1 = vec![text_delta("hmm"), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"bad":1}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn3 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"also_bad":2}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
}

// -- Test 9: Budget=1 + text stop -> SchemaUnreachable(1) (R7) --------

#[tokio::test]
async fn budget_one_text_stop_unreachable() {
    let events = vec![text_delta("nope"), done_event(StopReason::EndTurn)];

    let provider = MockProvider::new(vec![events]);
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

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 1);
}
