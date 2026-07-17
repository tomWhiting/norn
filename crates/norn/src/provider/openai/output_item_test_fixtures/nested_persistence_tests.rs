use std::io;

use serde_json::Value;

use super::nested::nested_output_item_matrix;
use crate::provider::openai::request::{CATALOG_BACKEND_CODEX_SUBSCRIPTION, build_payload};
use crate::provider::request::ProviderRequest;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::conversion::events_to_messages;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::{EventStore, JsonlSink, read_session_events};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn transcript_items(raw_items: &[Value]) -> TestResult<Vec<ResponseTranscriptItem>> {
    raw_items
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| {
            let output_index = u64::try_from(index)?;
            let item_id = raw.get("id").and_then(Value::as_str).map(str::to_owned);
            Ok(ResponseTranscriptItem {
                item: ResponseItem::from_value(raw)?,
                provenance: ResponseStreamProvenance {
                    item_id,
                    output_index: Some(output_index),
                    content_index: None,
                    sequence_number: Some(output_index.saturating_add(1)),
                },
            })
        })
        .collect()
}

#[test]
fn every_nested_union_variant_persists_and_replays_verbatim() -> TestResult {
    let expected = nested_output_item_matrix("persist");
    let response_items = transcript_items(&expected)?;
    let temp = tempfile::tempdir()?;
    let session_id = "nested-output-unions";
    let path = temp.path().join(format!("{session_id}.jsonl"));
    let store = EventStore::with_sink(Box::new(JsonlSink::open(&path)?));
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_nested_unions".to_owned()),
    })?;
    store.checkpoint()?;
    drop(store);

    let replay = read_session_events(temp.path(), session_id)?;
    let messages = events_to_messages(&replay.events);
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
    let payload = build_payload(&request, CATALOG_BACKEND_CODEX_SUBSCRIPTION)?;
    let input = payload
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("request input was not an array"))?;
    assert_eq!(input, &expected);
    assert!(input.iter().all(|item| item.get("output_index").is_none()));
    assert!(
        input
            .iter()
            .all(|item| item.get("sequence_number").is_none())
    );
    Ok(())
}
