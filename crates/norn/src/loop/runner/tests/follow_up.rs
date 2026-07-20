use super::*;

// -- R7/R3-N011 Test: schema-mode follow-up triggers continuation -----
//
// R3 (N-011) acceptance: "Follow-up messages injected only when loop
// would return Completed" — this test verifies a FollowUp message
// buffered while the loop is otherwise ready to complete causes the
// loop to continue.

#[tokio::test]
async fn schema_mode_follow_up_triggers_continuation() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"second"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let sent = tx
            .send(make_channel_message(
                "operator",
                "any more thoughts?",
                crate::r#loop::inbound::MessageKind::Update,
                0,
            ))
            .await;
        assert!(sent.is_ok(), "follow-up fixture failed to send: {sent:?}");
    });

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
        output["answer"], "second",
        "final output should be from turn 2"
    );

    let events = store.events();
    let has_follow_up = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"operator\" ")
                && content.contains("kind=\"update\"")
                && content.contains("\nany more thoughts?\n")
        } else {
            false
        }
    });
    assert!(has_follow_up, "follow-up message should appear");
}

// -- R7 Test: no-schema-mode follow-up triggers continuation ----------

#[tokio::test]
async fn no_schema_mode_follow_up_triggers_continuation() {
    let turn1 = vec![text_delta("first text"), done_event(StopReason::EndTurn)];
    let turn2 = vec![text_delta("second text"), done_event(StopReason::EndTurn)];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let sent = tx
            .send(make_channel_message(
                "operator",
                "say more",
                crate::r#loop::inbound::MessageKind::Update,
                0,
            ))
            .await;
        assert!(sent.is_ok(), "follow-up fixture failed to send: {sent:?}");
    });

    let result = run_step_full(
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
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("second text".to_string()));

    let events = store.events();
    let has_follow_up = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"operator\" ")
                && content.contains("kind=\"update\"")
                && content.contains("\nsay more\n")
        } else {
            false
        }
    });
    assert!(has_follow_up, "follow-up message should appear");
}

// -- R7 Test: no follow-up at stop -> Completed normally --------------

#[tokio::test]
async fn no_follow_up_at_stop_returns_completed_normally() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"only"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (_tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "only");
    assert_eq!(
        provider.call_count(),
        1,
        "exactly one provider call expected when no follow-up"
    );
}

// -- R7 Test: follow-up does NOT consume schema budget ---------------

#[tokio::test]
async fn follow_up_does_not_consume_schema_budget() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"second"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let sent = tx
            .send(make_channel_message(
                "operator",
                "more please",
                crate::r#loop::inbound::MessageKind::Update,
                0,
            ))
            .await;
        assert!(sent.is_ok(), "follow-up fixture failed to send: {sent:?}");
    });

    // Budget = 1: if follow-up consumed budget, the second turn would
    // result in SchemaUnreachable. Successful Completed proves the
    // follow-up did NOT consume budget.
    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &config_with_budget(1),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "second");
}
