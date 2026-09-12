//! Canonical turns persist and resume without redundant legacy payloads.

use std::io;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn raw_items() -> Vec<Value> {
    vec![
        serde_json::json!({
            "type": "reasoning", "id": "rs_storage",
            "summary": [{"type": "summary_text", "text": "Inspect the file."}],
            "encrypted_content": "opaque-reasoning-".repeat(512),
            "future_field": {"preserve": true}
        }),
        serde_json::json!({
            "type": "message", "id": "msg_storage", "role": "assistant",
            "status": "completed", "phase": "commentary",
            "content": [{"type": "output_text", "text": "Checking the file.",
                         "annotations": [], "logprobs": []}]
        }),
        serde_json::json!({
            "type": "function_call", "id": "fc_storage", "call_id": "call_storage",
            "name": "read_file", "arguments": "{\"path\":\"README.md\"}",
            "status": "completed", "caller": null
        }),
    ]
}

fn transcript(raw: Value, index: u64) -> TestResult<ResponseTranscriptItem> {
    let item_id = raw.get("id").and_then(Value::as_str).map(str::to_owned);
    Ok(ResponseTranscriptItem {
        item: ResponseItem::from_value(raw)?,
        provenance: ResponseStreamProvenance {
            item_id,
            output_index: Some(index),
            content_index: None,
            sequence_number: Some(index),
        },
    })
}

fn assert_lean(message: &Message) {
    assert!(!message.response_items.is_empty());
    assert!(message.content.is_none());
    assert!(message.thinking.is_empty());
    assert!(message.reasoning.is_empty());
    assert!(message.tool_calls.is_empty());
}

fn payload_input(request: &ProviderRequest) -> TestResult<Vec<Value>> {
    let payload = crate::provider::openai::request::build_payload(request, "codex_subscription")?;
    payload
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| io::Error::other("canonical storage fixture has no input array").into())
}

fn assert_smaller_than_duplicated_row(event: &SessionEvent, raw: &[Value]) -> TestResult {
    let mut old = serde_json::to_value(event)?;
    old["content"] = serde_json::json!("Checking the file.");
    old["thinking"] = serde_json::json!("Inspect the file.");
    old["reasoning"] = serde_json::json!([serde_json::from_value::<
        crate::provider::reasoning::ReasoningItem,
    >(raw[0].clone())?]);
    old["tool_calls"] = serde_json::to_value(event.assistant_tool_calls())?;
    let new_bytes = serde_json::to_vec(event)?.len();
    let old_bytes = serde_json::to_vec(&old)?.len();
    assert!(
        old_bytes > new_bytes + 8192,
        "fixture did not remove its duplicate opaque payload"
    );
    eprintln!(
        "canonical storage fixture: old={old_bytes} new={new_bytes} saved={}",
        old_bytes - new_bytes
    );
    Ok(())
}

#[tokio::test]
async fn canonical_storage_is_lean_in_live_requests_and_durable_resume() -> TestResult {
    let raw = raw_items();
    let mut first = raw
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, item)| {
            Ok(ProviderEvent::ResponseItemDone {
                item: transcript(item, u64::try_from(index)?)?,
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    first.push(done_event(StopReason::ToolUse));
    let provider = MockProvider::new(vec![
        first,
        vec![text_delta("Finished."), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::new(read_file_handlers());
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("canonical-storage.jsonl");
    let store = EventStore::with_sink(Box::new(crate::session::JsonlSink::open(&path)?));
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
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let live = &requests[1];
    let assistant = live
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .ok_or_else(|| io::Error::other("no live canonical assistant"))?;
    assert_lean(assistant);
    assert_eq!(
        assistant
            .response_items
            .iter()
            .map(|item| item.item.raw())
            .collect::<Vec<_>>(),
        raw.iter().collect::<Vec<_>>()
    );
    let live_input = payload_input(live)?;
    assert!(live_input.windows(raw.len()).any(|window| window == raw));
    assert!(
        live_input
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "call_storage"
                && item["caller"].is_null())
    );
    store.checkpoint()?;
    drop(store);
    let artifacts = crate::session::read_session_events(temp.path(), "canonical-storage")?;
    crate::session::validate_provider_state_provenance(&artifacts.events)?;
    let canonical = artifacts
        .events
        .iter()
        .find(|event| {
            matches!(event,
        SessionEvent::AssistantMessage { response_items, .. } if !response_items.is_empty())
        })
        .ok_or_else(|| io::Error::other("no durable canonical assistant"))?;
    let encoded = serde_json::to_value(canonical)?;
    assert_eq!(encoded["content"], "");
    assert_eq!(encoded["thinking"], "");
    assert_eq!(encoded["tool_calls"], serde_json::json!([]));
    assert!(encoded.get("reasoning").is_none());
    assert_smaller_than_duplicated_row(canonical, &raw)?;
    let mut resumed = live.clone();
    resumed.messages = crate::session::conversion::events_to_messages(&artifacts.events);
    let resumed_assistant = resumed
        .messages
        .iter()
        .find(|message| !message.response_items.is_empty())
        .ok_or_else(|| io::Error::other("no resumed canonical assistant"))?;
    assert_lean(resumed_assistant);
    assert_eq!(resumed_assistant.response_items, assistant.response_items);
    let resumed_input = payload_input(&resumed)?;
    // The live request has a generated Developer tail; reload includes the
    // final legacy assistant instead. Compare the entire shared prefix.
    assert_eq!(
        &live_input[..live_input.len() - 1],
        &resumed_input[..resumed_input.len() - 1]
    );
    let final_message = resumed
        .messages
        .last()
        .ok_or_else(|| io::Error::other("no final legacy assistant"))?;
    assert_eq!(final_message.content.as_deref(), Some("Finished."));
    assert!(final_message.response_items.is_empty());
    Ok(())
}
