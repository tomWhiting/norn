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

fn assert_exact_input(input: &[Value], expected: &[Value]) -> Result<(), io::Error> {
    if input == expected {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "stateless continuation changed item order or content:\nactual={input:#?}\nexpected={expected:#?}",
    )))
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
            transcript_event(final_item.clone(), 0)?,
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
    assert!(!continuation.store);
    let user_item = serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "prompt"}]
    });
    let tool_result = serde_json::json!({
        "type": "function_call_output",
        "call_id": "call_runner",
        "output": "{\"content\":\"file data\"}"
    });
    let mut expected_continuation = vec![user_item];
    expected_continuation.extend(hosted.iter().cloned());
    expected_continuation.push(tool_result);
    expected_continuation.push(serde_json::json!({
        "type": "message",
        "role": "developer",
        "content": crate::system_prompt::CollaborationMode::Default.format_section()
    }));
    assert_exact_input(&payload_input(continuation)?, &expected_continuation)?;

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
    // This direct persistence/reload seam reconstructs only durable session
    // messages. The runner regenerates its managed dynamic Developer tail per
    // iteration, so it is expected on the live second request above but is not
    // part of the persisted response transcript.
    let mut expected_resume = expected_continuation;
    let dynamic_tail = expected_resume.pop();
    assert!(matches!(
        dynamic_tail,
        Some(Value::Object(ref item))
            if item.get("role").and_then(Value::as_str) == Some("developer")
    ));
    expected_resume.push(final_item);
    assert_exact_input(&payload_input(&resumed_request)?, &expected_resume)?;
    Ok(())
}
