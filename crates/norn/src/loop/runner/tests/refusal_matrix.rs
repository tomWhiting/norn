use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

type MatrixResult = Result<(), Box<dyn std::error::Error>>;

fn refusal_item(
    id: &str,
    content: &Value,
) -> Result<(ProviderEvent, Value), Box<dyn std::error::Error>> {
    let raw = serde_json::json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": "completed",
        "content": content,
    });
    let event = completed_message_item(
        id,
        raw.get("content")
            .ok_or_else(|| io::Error::other("refusal fixture lost content"))?,
    )?;
    Ok((event, raw))
}

fn refused_result(result: AgentStepResult) -> Result<(String, u32, Usage), io::Error> {
    let AgentStepResult::Refused {
        refusal,
        iterations,
        usage,
        ..
    } = result
    else {
        return Err(io::Error::other(format!(
            "expected a refusal outcome, received {result:?}"
        )));
    };
    Ok((refusal, iterations, usage))
}

fn persisted_items(store: &EventStore) -> Result<Vec<ResponseTranscriptItem>, io::Error> {
    store
        .events()
        .into_iter()
        .rev()
        .find_map(|event| match event {
            SessionEvent::AssistantMessage { response_items, .. } => Some(response_items),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("refusal turn was not persisted"))
}

#[tokio::test]
async fn pure_and_mixed_refusals_stop_once_and_persist_canonical_content() -> MatrixResult {
    let cases = [
        (
            "pure",
            serde_json::json!([{
                "type": "refusal",
                "refusal": "I cannot complete that request."
            }]),
            "I cannot complete that request.",
        ),
        (
            "mixed",
            serde_json::json!([
                {
                    "type": "output_text",
                    "text": "I can explain the safe part. ",
                    "annotations": [],
                    "logprobs": []
                },
                {
                    "type": "refusal",
                    "refusal": "I cannot complete the remaining part."
                }
            ]),
            "I cannot complete the remaining part.",
        ),
    ];

    for (id, content, expected_refusal) in cases {
        let (item, raw) = refusal_item(id, &content)?;
        let provider = MockProvider::new(vec![vec![item, done_event(StopReason::EndTurn)]]);
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
        let (refusal, iterations, usage) = refused_result(result)?;
        assert_eq!(refusal, expected_refusal);
        assert_eq!(iterations, 1);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(provider.call_count(), 1);

        let persisted = persisted_items(&store)?;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].item.raw(), &raw);
    }
    Ok(())
}

#[tokio::test]
async fn structured_output_refusal_bypasses_schema_retry_and_dispatch() -> MatrixResult {
    let (item, raw) = refusal_item(
        "structured_refusal",
        &serde_json::json!([{
            "type": "refusal",
            "refusal": "I cannot provide that structured result."
        }]),
    )?;
    let provider = MockProvider::new(vec![vec![item, done_event(StopReason::EndTurn)]]);
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
    let (refusal, iterations, usage) = refused_result(result)?;
    assert_eq!(refusal, "I cannot provide that structured result.");
    assert_eq!(iterations, 1);
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(
        provider.call_count(),
        1,
        "refusal must not trigger a schema retry"
    );
    assert!(
        store
            .events()
            .iter()
            .all(|event| !matches!(event, SessionEvent::ToolResult { .. })),
        "a refusing response must not dispatch the schema tool"
    );
    assert_eq!(persisted_items(&store)?[0].item.raw(), &raw);
    Ok(())
}

#[tokio::test]
async fn tool_loop_then_refusal_executes_only_the_prior_call() -> MatrixResult {
    let dispatches = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&dispatches);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_owned(),
        Box::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"content": "file data"}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let first = vec![
        tool_call_delta(
            "call_before_refusal",
            Some("read_file"),
            r#"{"path":"README.md"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let (refusal, raw) = refusal_item(
        "after_tool_refusal",
        &serde_json::json!([{
            "type": "refusal",
            "refusal": "I cannot continue after reading that file."
        }]),
    )?;
    let provider = MockProvider::new(vec![first, vec![refusal, done_event(StopReason::EndTurn)]]);
    let store = EventStore::new();

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
    let (refusal, iterations, usage) = refused_result(result)?;
    assert_eq!(refusal, "I cannot continue after reading that file.");
    assert_eq!(iterations, 2);
    assert_eq!(usage.input_tokens, 20);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(provider.call_count(), 2);
    assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(persisted_items(&store)?[0].item.raw(), &raw);
    Ok(())
}

#[tokio::test]
async fn persisted_refusal_reloads_and_replays_without_flat_substitution() -> MatrixResult {
    let temp = tempfile::tempdir()?;
    let session_id = "refusal-resume";
    let path = temp.path().join(format!("{session_id}.jsonl"));
    let store = EventStore::with_sink(Box::new(crate::session::JsonlSink::open(&path)?));
    let (item, raw) = refusal_item(
        "persisted_refusal",
        &serde_json::json!([{
            "type": "refusal",
            "refusal": "I cannot resume that operation."
        }]),
    )?;
    let provider = MockProvider::new(vec![vec![item, done_event(StopReason::EndTurn)]]);
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
    let (refusal, _, usage) = refused_result(result)?;
    assert_eq!(refusal, "I cannot resume that operation.");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    store.checkpoint()?;
    drop(store);

    let artifacts = crate::session::read_session_events(temp.path(), session_id)?;
    let persisted_usage = artifacts
        .events
        .iter()
        .find_map(|event| match event {
            SessionEvent::AssistantMessage { usage, .. } => Some(usage),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("reloaded refusal had no usage record"))?;
    assert_eq!(persisted_usage.input_tokens, 10);
    assert_eq!(persisted_usage.output_tokens, 5);
    assert_eq!(persisted_usage.cache_read_tokens, 0);
    assert_eq!(persisted_usage.cache_write_tokens, 0);
    assert!(persisted_usage.cost_usd.is_none());
    let messages = crate::session::conversion::events_to_messages(&artifacts.events);
    let assistant = messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .ok_or_else(|| io::Error::other("reloaded refusal had no assistant message"))?;
    assert_eq!(assistant.response_items.len(), 1);
    assert_eq!(assistant.response_items[0].item.raw(), &raw);
    assert_ne!(
        assistant.content.as_deref(),
        Some("I cannot resume that operation.")
    );

    let request = ProviderRequest {
        messages,
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    let payload = crate::provider::openai::request::build_payload(&request, "codex_subscription")?;
    let input = payload
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("resumed payload had no input array"))?;
    assert_eq!(input.last(), Some(&raw));
    assert!(input.iter().all(|item| item.get("output_index").is_none()));
    assert!(
        input
            .iter()
            .all(|item| item.get("sequence_number").is_none())
    );

    let (resumed_item, resumed_raw) = refusal_item(
        "resumed_turn_refusal",
        &serde_json::json!([{
            "type": "refusal",
            "refusal": "I still cannot complete that operation."
        }]),
    )?;
    let resumed_provider =
        MockProvider::new(vec![vec![resumed_item, done_event(StopReason::EndTurn)]]);
    let resumed_store = EventStore::with_sink_and_events(
        Box::new(crate::session::JsonlSink::open(&path)?),
        artifacts.events.clone(),
    );
    let resumed_result = run_step(
        &resumed_provider,
        &executor,
        &resumed_store,
        &[],
        None,
        &default_config(),
        None,
    )
    .await;
    let (resumed_refusal, iterations, resumed_usage) = refused_result(resumed_result)?;
    assert_eq!(resumed_refusal, "I still cannot complete that operation.");
    assert_eq!(iterations, 1);
    assert_eq!(resumed_usage.input_tokens, 10);
    assert_eq!(resumed_usage.output_tokens, 5);
    assert_eq!(resumed_provider.call_count(), 1);
    assert_eq!(persisted_items(&resumed_store)?[0].item.raw(), &resumed_raw);
    resumed_store.checkpoint()?;
    drop(resumed_store);

    let resumed_artifacts = crate::session::read_session_events(temp.path(), session_id)?;
    let refusal_usages = resumed_artifacts
        .events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { usage, .. } => Some(usage),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(refusal_usages.len(), 2);
    assert!(
        refusal_usages
            .iter()
            .all(|usage| usage.input_tokens == 10 && usage.output_tokens == 5)
    );
    Ok(())
}
