use std::io;

use serde_json::{Value, json};

use super::{CATALOG_BACKEND_CODEX_SUBSCRIPTION, build_payload};
use crate::provider::openai::output_item_test_fixtures::historical_shape_matrix_items;
use crate::provider::request::ProviderRequest;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::conversion::events_to_messages;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::{CreateSessionOptions, DurabilityPolicy, SessionManager};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn options(model: &str) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: "/workspace".to_owned(),
        name: None,
    }
}

fn opaque_history(id_suffix: &str) -> Vec<Value> {
    let mut history = historical_shape_matrix_items(id_suffix, "opaque lifecycle");
    assert_eq!(history.len(), 52);
    history.push(json!({
        "type": "future_hosted_call",
        "id": format!("future_{id_suffix}"),
        "status": "completed",
        "payload": {"retained": true}
    }));
    assert_eq!(history.len(), 53);
    history
}

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

fn assistant_event(items: Vec<ResponseTranscriptItem>) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: items,
        content: "lossy projection must not be replayed".to_owned(),
        thinking: "lossy reasoning projection".to_owned(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_opaque_lifecycle".to_owned()),
    }
}

fn persisted_items(events: &[SessionEvent]) -> TestResult<&[ResponseTranscriptItem]> {
    let Some(SessionEvent::AssistantMessage { response_items, .. }) = events.first() else {
        return Err(io::Error::other("strict history lost its assistant event").into());
    };
    Ok(response_items)
}

fn stateless_input(events: &[SessionEvent]) -> TestResult<Vec<Value>> {
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
    let payload = build_payload(&request, CATALOG_BACKEND_CODEX_SUBSCRIPTION)?;
    payload
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| io::Error::other("Responses payload had no input array").into())
}

fn assert_opaque_tail(events: &[SessionEvent]) -> TestResult {
    let Some(tail) = persisted_items(events)?.last() else {
        return Err(io::Error::other("strict history lost its response items").into());
    };
    assert!(matches!(tail.item, ResponseItem::Opaque(_)));
    Ok(())
}

#[test]
fn unknown_item_survives_strict_reload_into_stateless_replay() -> TestResult {
    let expected = opaque_history("strict_reload");
    let expected_items = transcript_items(&expected)?;
    let directory = tempfile::tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "opaque-strict-reload",
        options("gpt-source"),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    opened
        .store
        .append(assistant_event(expected_items.clone()))?;
    opened.store.checkpoint()?;
    drop(opened);

    let resumed = manager.resume("opaque-strict-reload", DurabilityPolicy::FsyncPerEvent)?;
    let events = resumed.store.events();
    assert_eq!(persisted_items(&events)?, expected_items);
    assert_opaque_tail(&events)?;
    let input = stateless_input(&events)?;
    assert_eq!(input, expected);
    assert!(input.iter().all(|item| item.get("output_index").is_none()));
    assert!(
        input
            .iter()
            .all(|item| item.get("sequence_number").is_none())
    );
    Ok(())
}

#[test]
fn manager_fork_preserves_unknown_item_under_new_owner_and_strict_resume() -> TestResult {
    let expected = opaque_history("manager_fork");
    let expected_items = transcript_items(&expected)?;
    let directory = tempfile::tempdir()?;
    let manager = SessionManager::new(directory.path());
    let source = manager.create_with_id(
        "opaque-fork-source",
        options("gpt-source"),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let source_id = source.entry.id.clone();
    let source_generation = source.entry.generation;
    source
        .store
        .append(assistant_event(expected_items.clone()))?;
    source.store.checkpoint()?;
    drop(source);

    let fork = manager.fork(
        &source_id,
        options("gpt-fork"),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    assert_ne!(fork.entry.id, source_id);
    assert_ne!(fork.entry.generation, source_generation);
    let fork_id = fork.entry.id.clone();
    drop(fork);

    let resumed = manager.resume(&fork_id, DurabilityPolicy::FsyncPerEvent)?;
    let events = resumed.store.events();
    assert_eq!(persisted_items(&events)?, expected_items);
    assert_opaque_tail(&events)?;
    assert_eq!(stateless_input(&events)?, expected);
    Ok(())
}
