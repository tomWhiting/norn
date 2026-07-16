use std::io;

use super::*;

type MatrixResult = Result<(), Box<dyn std::error::Error>>;

fn transcript_event(
    raw: Value,
    output_index: u64,
) -> Result<ProviderEvent, crate::provider::ResponseItemError> {
    let item_id = raw.get("id").and_then(Value::as_str).map(str::to_owned);
    Ok(ProviderEvent::ResponseItemDone {
        item: ResponseTranscriptItem {
            item: ResponseItem::from_value(raw)?,
            provenance: ResponseStreamProvenance {
                item_id,
                output_index: Some(output_index),
                content_index: None,
                sequence_number: Some(output_index),
            },
        },
    })
}

fn hosted_turn_items() -> Vec<Value> {
    vec![
        serde_json::json!({
            "type": "web_search_call",
            "id": "ws_runner",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["Responses canonical replay"],
                "sources": [{
                    "type": "url",
                    "url": "https://example.test/runner-source",
                    "title": "Runner source"
                }]
            }
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_runner",
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "I found the source and will read the local file.",
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": 18,
                    "url": "https://example.test/runner-source",
                    "title": "Runner source"
                }],
                "logprobs": []
            }]
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc_runner",
            "call_id": "call_runner",
            "name": "read_file",
            "arguments": "{}",
            "status": "completed"
        }),
    ]
}

fn assert_continuation(input: &[Value], hosted: &[Value]) -> Result<(), io::Error> {
    let Some(start) = input
        .windows(hosted.len())
        .position(|window| window == hosted)
    else {
        return Err(io::Error::other(
            "continuation lost the canonical hosted-search item sequence",
        ));
    };
    let has_result = input.iter().skip(start + hosted.len()).any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item.get("call_id").and_then(Value::as_str) == Some("call_runner")
    });
    if !has_result {
        return Err(io::Error::other(
            "continuation lost the correlated local tool result",
        ));
    }
    Ok(())
}

fn payload_input(request: &ProviderRequest) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let payload = crate::provider::openai::request::build_payload(request, "codex_subscription")?;
    payload
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| io::Error::other("Responses payload had no input array").into())
}

#[tokio::test]
async fn hosted_search_survives_runner_tool_continuation_and_persisted_resume() -> MatrixResult {
    let hosted = hosted_turn_items();
    let mut first = Vec::with_capacity(hosted.len() + 1);
    for (index, raw) in hosted.iter().cloned().enumerate() {
        first.push(transcript_event(raw, u64::try_from(index)?)?);
    }
    first.push(done_event(StopReason::ToolUse));
    let final_item = serde_json::json!({
        "type": "message",
        "id": "msg_runner_final",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": "The local file confirms the result.",
            "annotations": [],
            "logprobs": []
        }]
    });
    let provider = MockProvider::new(vec![
        first,
        vec![
            transcript_event(final_item, 0)?,
            done_event(StopReason::EndTurn),
        ],
    ]);
    let executor = MockToolExecutor::new(read_file_handlers());
    let temp = tempfile::tempdir()?;
    let session_id = "hosted-runner-resume";
    let session_path = temp.path().join(format!("{session_id}.jsonl"));
    let store = EventStore::with_sink(Box::new(crate::session::JsonlSink::open(&session_path)?));

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
    if !matches!(result, AgentStepResult::Completed { .. }) {
        return Err(io::Error::other(format!(
            "hosted-search runner fixture did not complete: {result:?}"
        ))
        .into());
    }
    assert_eq!(provider.call_count(), 2);
    let requests = provider.requests()?;
    let continuation = requests
        .get(1)
        .ok_or_else(|| io::Error::other("runner made no tool-result continuation request"))?;
    assert_continuation(&payload_input(continuation)?, &hosted)?;

    store.checkpoint()?;
    drop(store);
    let artifacts = crate::session::read_session_events(temp.path(), session_id)?;
    assert_eq!(artifacts.skipped_lines, 0);
    let mut resumed_request = requests
        .first()
        .cloned()
        .ok_or_else(|| io::Error::other("runner made no initial request"))?;
    resumed_request.messages = crate::session::conversion::events_to_messages(&artifacts.events);
    resumed_request.previous_response_id = None;
    resumed_request.store = false;
    assert_continuation(&payload_input(&resumed_request)?, &hosted)?;
    Ok(())
}
