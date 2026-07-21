use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- Test 10: [read_tool, schema_tool] -> read executes, schema valid (R5)

#[tokio::test]
async fn pre_schema_tools_execute() {
    let events = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let events = store.events();
    let read_result = events.iter().any(
        |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
    );
    assert!(read_result, "read_file tool should have been executed");
}

// -- Test 11: [schema_tool, read_tool] -> read REJECTED (R5) ----------

#[tokio::test]
async fn post_schema_tools_rejected() {
    let events = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "first");

    let events = store.events();
    let read_results: Vec<&SessionEvent> = events
        .iter()
        .filter(
            |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
        )
        .collect();
    assert_eq!(read_results.len(), 1, "should have one read_file result");
    if let SessionEvent::ToolResult { output, .. } = read_results[0] {
        let error_str = output["error"].as_str().unwrap_or("");
        assert!(
            error_str.contains("rejected"),
            "read_file should be rejected, got: {error_str}"
        );
    }

    // REVIEW H1: exactly one result for the schema tool call. The
    // pre-fix code appended an acceptance in BOTH
    // `accept_schema_tool_call` and `reject_post_schema_tools`,
    // producing a duplicate `function_call_output` that poisoned the
    // persisted session and drew a provider 400 on the next request.
    let schema_results: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::ToolResult { tool_name, .. }
                    if tool_name == "structured_output"
            )
        })
        .collect();
    assert_eq!(
        schema_results.len(),
        1,
        "exactly one structured_output result must be persisted",
    );
    if let SessionEvent::ToolResult {
        tool_call_id,
        output,
        ..
    } = schema_results[0]
    {
        assert_eq!(tool_call_id, "tc_schema");
        assert_eq!(output.as_str(), Some("accepted"));
    }
}

// -- REVIEW H1 regression: one persisted result per call_id ------------
//
// [read_file, structured_output, read_file] exercises pre-schema
// execution, schema acceptance, and post-schema rejection in one
// response. Every call_id must have exactly one ToolResult in the
// persisted store — duplicates poison session replay permanently.

#[tokio::test]
async fn schema_flow_persists_exactly_one_result_per_call_id() -> TestResult {
    let events = vec![
        tool_call_delta("tc_pre", Some("read_file"), r#"{"path":"a"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        tool_call_delta("tc_post", Some("read_file"), r#"{"path":"b"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "first");

    let mut results_by_call: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for event in store.events() {
        if let SessionEvent::ToolResult {
            tool_call_id,
            output,
            ..
        } = event
        {
            results_by_call
                .entry(tool_call_id)
                .or_default()
                .push(output);
        }
    }
    for call_id in ["tc_pre", "tc_schema", "tc_post"] {
        let outputs = results_by_call
            .get(call_id)
            .ok_or_else(|| std::io::Error::other(format!("missing result for {call_id}")))?;
        assert_eq!(
            outputs.len(),
            1,
            "{call_id} must have exactly one persisted result, got {outputs:?}",
        );
    }
    assert_eq!(results_by_call["tc_schema"][0].as_str(), Some("accepted"));
    assert!(
        results_by_call["tc_pre"][0]["content"].is_string(),
        "pre-schema tool must actually execute",
    );
    assert!(
        results_by_call["tc_post"][0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("rejected"),
        "post-schema tool must be rejected, not executed",
    );
    Ok(())
}

// -- REVIEW H3: SchemaInvalid must answer post-schema tool calls -------
//
// Turn 1 returns [structured_output(invalid), read_file]; turn 2
// returns a valid schema call. Pre-fix, tc_read was left unanswered:
// turn 2's request carried a dangling tool call and real providers
// reject it with a 400, wedging the retry loop.

#[tokio::test]
async fn schema_invalid_rejects_post_schema_tool_calls() -> TestResult {
    let turn1 = vec![
        tool_call_delta("tc_schema_1", Some("structured_output"), r#"{"wrong":1}"#),
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"ok"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    // Exactly one persisted result per call_id across the whole step.
    let mut result_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for event in store.events() {
        if let SessionEvent::ToolResult { tool_call_id, .. } = event {
            *result_counts.entry(tool_call_id).or_insert(0) += 1;
        }
    }
    assert_eq!(
        result_counts.get("tc_read"),
        Some(&1),
        "post-schema call after invalid schema must get exactly one result",
    );
    assert_eq!(result_counts.get("tc_schema_1"), Some(&1));
    assert_eq!(result_counts.get("tc_schema_2"), Some(&1));

    // The rejection is visible to the model on the retry request.
    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let answered = requests[1].messages.iter().any(|m| {
        matches!(m.role, MessageRole::ToolResult) && m.tool_call_id.as_deref() == Some("tc_read")
    });
    assert!(
        answered,
        "retry request must carry a result for the post-schema call",
    );
    Ok(())
}
