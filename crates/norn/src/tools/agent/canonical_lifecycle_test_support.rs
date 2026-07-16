use std::io;

use serde_json::Value;

use crate::provider::events::ProviderEvent;
use crate::provider::openai::response_contract::public_output_item;
use crate::provider::request::ProviderRequest;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::conversion::events_to_messages;
use crate::session::events::SessionEvent;

pub(super) fn spawn_non_audio_items(id_suffix: &str, text: &str) -> Vec<Value> {
    crate::provider::openai::output_item_test_fixtures::spawn_lifecycle_items(id_suffix, text)
}

pub(super) fn historical_non_audio_items(id_suffix: &str, text: &str) -> Vec<Value> {
    crate::provider::openai::output_item_test_fixtures::historical_replay_items(id_suffix, text)
}

pub(super) fn transcript_item(
    raw: Value,
    output_index: u64,
) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
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
}

pub(super) fn completed_item_event(
    raw: Value,
    output_index: u64,
) -> Result<ProviderEvent, crate::provider::ResponseItemError> {
    Ok(ProviderEvent::ResponseItemDone {
        item: transcript_item(raw, output_index)?,
    })
}

pub(super) fn canonical_item_values(events: &[SessionEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { response_items, .. } => Some(response_items),
            _ => None,
        })
        .flatten()
        .map(|entry| entry.item.raw().clone())
        .collect()
}

pub(super) fn stateless_payload_input(
    events: &[SessionEvent],
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let request = ProviderRequest {
        messages: events_to_messages(events),
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
        .ok_or_else(|| io::Error::other("Responses payload had no input array"))?;
    Ok(input.clone())
}

pub(super) fn canonical_payload_items(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter(|item| {
            let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                return false;
            };
            public_output_item(item_type).is_some()
                && (item_type != "message"
                    || item.get("role").and_then(Value::as_str) == Some("assistant"))
        })
        .cloned()
        .collect()
}
