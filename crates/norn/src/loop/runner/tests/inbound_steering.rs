use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- R5/R6/R3-N011 Test: steer message injected at tool boundary ------
//
// R3 (N-011) acceptance: this test exercises the drain-and-inject
// pipeline between two turns of a tool batch — turn 1 has tools, drain
// happens at the tool boundary, the steer message becomes a UserMessage
// event before turn 2's provider call sees it.

#[tokio::test]
async fn steer_message_injected_between_turns() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let sent = tx
        .send(make_channel_message(
            "alice",
            "please use foo.rs",
            crate::r#loop::inbound::MessageKind::Steer,
            0,
        ))
        .await;
    assert!(sent.is_ok(), "steer fixture failed to send: {sent:?}");

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

    let events = store.events();
    let has_steer = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"alice\" ")
                && content.contains("kind=\"steer\"")
                && content.contains("\nplease use foo.rs\n")
        } else {
            false
        }
    });
    assert!(has_steer, "steer message should appear as UserMessage");
}

// -- R6 Test: multiple steer messages in timestamp order --------------

#[tokio::test]
async fn multiple_steer_messages_injected_in_timestamp_order() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    // Send in reverse timestamp order; injection must sort ascending.
    let first_send = tx
        .send(make_channel_message(
            "bob",
            "second by time",
            crate::r#loop::inbound::MessageKind::Steer,
            200,
        ))
        .await;
    assert!(first_send.is_ok(), "first steer failed: {first_send:?}");
    let second_send = tx
        .send(make_channel_message(
            "alice",
            "first by time",
            crate::r#loop::inbound::MessageKind::Steer,
            100,
        ))
        .await;
    assert!(second_send.is_ok(), "second steer failed: {second_send:?}");

    let _result = run_step_full(
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

    let events = store.events();
    let steer_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if let SessionEvent::UserMessage { content, .. } = e {
                if content.starts_with("<agent_message from=") {
                    Some(i)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(steer_indices.len(), 2, "expected 2 steer messages");

    let first_event = &events[steer_indices[0]];
    let second_event = &events[steer_indices[1]];
    let (
        SessionEvent::UserMessage { content: c1, .. },
        SessionEvent::UserMessage { content: c2, .. },
    ) = (first_event, second_event)
    else {
        return Err(std::io::Error::other("expected two UserMessage events").into());
    };
    assert!(c1.contains("first by time"), "got: {c1}");
    assert!(c2.contains("second by time"), "got: {c2}");
    Ok(())
}

// -- R3 (N-011) regression: drain still works between turns ----------
//
// The existing `steer_message_injected_between_turns` test above (and
// its `multiple_steer_messages_injected_in_timestamp_order` sibling)
// already cover R3's "Inbound channel drained after tool batch" and
// "Steer messages become UserMessage events before next call"
// acceptance bullets. The follow-up tests
// (`schema_mode_follow_up_triggers_continuation`,
// `no_schema_mode_follow_up_triggers_continuation`, and
// `follow_up_does_not_consume_schema_budget`) cover the
// "Follow-up messages injected only when loop would return Completed"
// bullet. They live above in this same module and remain unchanged.

// -- R5 Test: drain occurs after tool batch, not mid-batch -----------

#[tokio::test]
async fn no_inbound_when_no_channel_is_safe() {
    // Regression: passing None for inbound on every existing path
    // should not crash.
    let turn1 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"clean"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "clean");
}
