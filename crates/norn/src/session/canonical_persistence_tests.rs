use std::io;

use serde_json::Value;

use super::conversion::events_to_messages;
use super::events::{EventBase, EventUsage, SessionEvent};
use super::manager::{CreateSessionOptions, SessionManager};
use super::persistence::read_session_events;
use super::store::{DurabilityPolicy, EventStore, JsonlSink};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn transcript_item(raw: Value, output_index: u64) -> TestResult<ResponseTranscriptItem> {
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

fn item_bytes(items: &[ResponseTranscriptItem]) -> TestResult<Vec<Vec<u8>>> {
    items
        .iter()
        .map(|entry| serde_json::to_vec(entry.item.raw()).map_err(Into::into))
        .collect()
}

#[test]
fn canonical_items_survive_jsonl_persistence_reload_and_resume_in_order() -> TestResult {
    let raw_items = [
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_persist",
            "summary": [{"type": "summary_text", "text": "inspect"}],
            "encrypted_content": "ciphertext",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_commentary",
            "role": "assistant",
            "status": "completed",
            "phase": "commentary",
            "content": [{
                "type": "output_text",
                "text": "Checking the repository.",
                "annotations": [{"type": "url_citation", "url": "https://example.test"}],
                "logprobs": []
            }]
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc_persist",
            "call_id": "call_persist",
            "name": "read_file",
            "arguments": "{\"path\":\"README.md\"}",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "image_generation_call",
            "id": "img_persist",
            "status": "completed",
            "result": "opaque-provider-payload"
        }),
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_after_call",
            "summary": [{"type": "summary_text", "text": "continue after tool output"}],
            "encrypted_content": "ciphertext-2",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "message",
            "id": "msg_final",
            "role": "assistant",
            "status": "completed",
            "phase": "final_answer",
            "content": [{
                "type": "output_text",
                "text": "Finished.",
                "annotations": [],
                "logprobs": []
            }]
        }),
    ];
    let expected = raw_items
        .into_iter()
        .enumerate()
        .map(|(index, raw)| transcript_item(raw, u64::try_from(index)?))
        .collect::<TestResult<Vec<_>>>()?;
    let expected_bytes = item_bytes(&expected)?;

    let temp = tempfile::tempdir()?;
    let session_id = "canonical-items";
    let path = temp.path().join(format!("{session_id}.jsonl"));
    let store = EventStore::with_sink(Box::new(JsonlSink::open(&path)?));
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: expected.clone(),
        content: "Finished.".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_persist".to_owned()),
    })?;
    store.checkpoint()?;
    drop(store);

    let artifacts = read_session_events(temp.path(), session_id)?;
    assert_eq!(artifacts.events.len(), 1);
    let Some(SessionEvent::AssistantMessage { response_items, .. }) = artifacts.events.first()
    else {
        return Err(io::Error::other("persisted assistant event was not reloaded").into());
    };
    assert_eq!(response_items, &expected);
    assert_eq!(item_bytes(response_items)?, expected_bytes);

    let messages = events_to_messages(&artifacts.events);
    let Some(message) = messages.first() else {
        return Err(io::Error::other("reloaded assistant event did not resume").into());
    };
    assert_eq!(message.response_items, expected);
    Ok(())
}

#[test]
fn manager_fork_copies_canonical_items_without_reconstruction() -> TestResult {
    let canonical = transcript_item(
        serde_json::json!({
            "type": "message",
            "id": "msg_fork",
            "role": "assistant",
            "status": "completed",
            "phase": "final_answer",
            "content": [{
                "type": "output_text",
                "text": "fork me",
                "annotations": [],
                "logprobs": []
            }]
        }),
        0,
    )?;
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let source = manager.create(
        CreateSessionOptions {
            model: "gpt-test".to_owned(),
            working_dir: "/workspace".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let source_id = source.entry.id.clone();
    source.store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: vec![canonical.clone()],
        content: "stale projection".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })?;
    source.store.checkpoint()?;
    drop(source);

    let fork = manager.fork(
        &source_id,
        CreateSessionOptions {
            model: "gpt-test".to_owned(),
            working_dir: "/workspace".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let Some(SessionEvent::AssistantMessage { response_items, .. }) =
        fork.store.events().into_iter().next()
    else {
        return Err(io::Error::other("fork did not copy the assistant event").into());
    };
    assert_eq!(response_items, vec![canonical]);
    Ok(())
}

fn committed_canonical_group(duplicated: bool) -> TestResult<Vec<SessionEvent>> {
    let raw = serde_json::json!({
        "type": "reasoning", "id": "rs_commit_storage",
        "summary": [{"type": "summary_text", "text": "Preserve this summary."}],
        "encrypted_content": "preserve-this-opaque-state",
        "future_field": [1, 2, 3]
    });
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: super::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let assistant_id = super::events::EventId::new();
    let provenance = super::ProviderStateProvenance::new(assistant_id.clone(), true)
        .into_custom_event(EventBase::new(Some(boundary.base().id.clone())))?;
    let mut base = EventBase::new(Some(provenance.base().id.clone()));
    base.id = assistant_id;
    let assistant = SessionEvent::AssistantMessage {
        base,
        response_items: vec![transcript_item(raw.clone(), 0)?],
        content: String::new(),
        thinking: if duplicated {
            "Preserve this summary.".to_owned()
        } else {
            String::new()
        },
        reasoning: if duplicated {
            vec![serde_json::from_value(raw)?]
        } else {
            Vec::new()
        },
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_storage_commit".to_owned()),
    };
    let mut group = vec![boundary, provenance, assistant];
    super::seal_response_publication_group(&mut group)?;
    Ok(group)
}

#[test]
fn canonical_storage_preserves_old_commitments_and_validates_new_lean_groups() -> TestResult {
    let temp = tempfile::tempdir()?;
    for (name, duplicated) in [("old-duplicate", true), ("new-lean", false)] {
        let group = committed_canonical_group(duplicated)?;
        let before = serde_json::to_vec(&group)?;
        let path = temp.path().join(format!("{name}.jsonl"));
        let store = EventStore::with_sink(Box::new(JsonlSink::open(&path)?));
        store.append_batch(&group)?;
        store.checkpoint()?;
        drop(store);
        let disk_before = std::fs::read(&path)?;
        let artifacts = read_session_events(temp.path(), name)?;
        super::validate_provider_state_provenance(&artifacts.events)?;
        assert_eq!(serde_json::to_vec(&artifacts.events)?, before);
        let messages = events_to_messages(&artifacts.events);
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.response_items.len(), 1);
        assert!(message.content.is_none());
        assert!(message.thinking.is_empty());
        assert!(message.reasoning.is_empty());
        assert!(message.tool_calls.is_empty());
        assert_eq!(
            message.response_items[0].item.raw()["encrypted_content"],
            "preserve-this-opaque-state"
        );
        assert_eq!(
            message.response_items[0].item.raw()["future_field"],
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(
            std::fs::read(&path)?,
            disk_before,
            "resume must not rewrite old storage"
        );
        let mut tampered = artifacts.events;
        for event in &mut tampered {
            if let SessionEvent::AssistantMessage { content, .. } = event {
                *content = "tampered after sealing".to_owned();
            }
        }
        assert!(super::validate_provider_state_provenance(&tampered).is_err());
    }
    Ok(())
}
