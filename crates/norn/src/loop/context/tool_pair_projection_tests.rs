use super::{ContentTag, construct_prompt};
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;

fn assistant_with_call(call_id: &str, content: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: call_id.to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    }
}

fn tool_result(call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: call_id.to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::json!({"content": "ok"}),
        spool_ref: None,
        duration_ms: 1,
    }
}

fn canonical_item(
    raw: serde_json::Value,
    sequence_number: u64,
) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
    Ok(ResponseTranscriptItem {
        item: ResponseItem::from_value(raw)?,
        provenance: ResponseStreamProvenance {
            sequence_number: Some(sequence_number),
            ..ResponseStreamProvenance::default()
        },
    })
}

fn canonical_assistant(response_items: Vec<ResponseTranscriptItem>) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items,
        base: EventBase::new(None),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    }
}

#[test]
fn suppressing_a_call_drops_its_visible_result_from_the_prompt()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let assistant_id = store.append(assistant_with_call("call_hidden", ""))?;
    store.append(tool_result("call_hidden"))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, assistant_id)?;

    let view = construct_prompt(&store, &edits);

    assert!(
        view.events
            .iter()
            .all(|event| !matches!(event, SessionEvent::ToolResult { .. })),
        "a result cannot survive without its suppressed call",
    );
    assert!(crate::session::unresolved_local_tool_calls(&view.events).is_empty());
    assert!(view.tags.is_empty(), "only bookkeeping remains visible");
    Ok(())
}

#[test]
fn compacting_a_result_strips_its_call_but_preserves_assistant_text()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let assistant_id = store.append(assistant_with_call("call_split", "keep this answer"))?;
    let result_id = store.append(tool_result("call_split"))?;
    let mut edits = ContextEdits::new();
    let compaction_id =
        edits.summarize(&store, vec![result_id], "result compacted out".to_owned())?;

    let view = construct_prompt(&store, &edits);

    let assistant = view
        .events
        .iter()
        .find(|event| event.base().id == assistant_id)
        .ok_or_else(|| std::io::Error::other("assistant text was discarded with the split call"))?;
    assert_eq!(
        assistant.assistant_text().as_deref(),
        Some("keep this answer")
    );
    assert!(
        assistant
            .assistant_tool_calls()
            .is_some_and(|calls| calls.is_empty()),
        "the split call is removed without discarding assistant text",
    );
    assert!(crate::session::unresolved_local_tool_calls(&view.events).is_empty());
    assert!(
        view.events
            .iter()
            .any(|event| event.base().id == compaction_id),
        "the compaction summary remains prompt content",
    );
    assert_eq!(view.tags, vec![ContentTag::Message, ContentTag::Compaction]);
    Ok(())
}

#[test]
fn split_canonical_call_is_removed_without_dropping_sibling_message_item()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let response_items = vec![
        canonical_item(
            serde_json::json!({
                "type": "message",
                "id": "msg_atomic",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "canonical answer",
                    "annotations": [],
                    "logprobs": []
                }]
            }),
            1,
        )?,
        canonical_item(
            serde_json::json!({
                "type": "function_call",
                "id": "fc_atomic",
                "call_id": "call_canonical_split",
                "name": "read",
                "arguments": "{}",
                "status": "completed"
            }),
            2,
        )?,
    ];
    let assistant_id = store.append(SessionEvent::AssistantMessage {
        response_items,
        base: EventBase::new(None),
        content: "stale flat projection".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    })?;
    let result_id = store.append(tool_result("call_canonical_split"))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, result_id)?;

    let view = construct_prompt(&store, &edits);

    let assistant = view
        .events
        .iter()
        .find(|event| event.base().id == assistant_id)
        .ok_or_else(|| std::io::Error::other("canonical sibling item was discarded"))?;
    assert_eq!(
        assistant.assistant_text().as_deref(),
        Some("canonical answer")
    );
    assert!(
        assistant
            .assistant_tool_calls()
            .is_some_and(|calls| calls.is_empty()),
    );
    let SessionEvent::AssistantMessage { response_items, .. } = assistant else {
        return Err(std::io::Error::other("assistant event changed variant").into());
    };
    assert_eq!(response_items.len(), 1);
    assert_eq!(response_items[0].item.item_type(), "message");
    Ok(())
}

#[test]
fn duplicate_same_id_calls_are_filtered_by_within_event_occurrence()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let assistant_id = store.append(canonical_assistant(vec![
        canonical_item(
            serde_json::json!({
                "type": "function_call",
                "id": "fc_duplicate_first",
                "call_id": "call_duplicate",
                "name": "read",
                "arguments": "{}",
                "status": "completed"
            }),
            1,
        )?,
        canonical_item(
            serde_json::json!({
                "type": "function_call",
                "id": "fc_duplicate_second",
                "call_id": "call_duplicate",
                "name": "read",
                "arguments": "{}",
                "status": "completed"
            }),
            2,
        )?,
    ]))?;
    let first_output = store.append(tool_result("call_duplicate"))?;
    let second_output = store.append(tool_result("call_duplicate"))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, first_output)?;

    let view = construct_prompt(&store, &edits);
    let assistant = view
        .events
        .iter()
        .find(|event| event.base().id == assistant_id)
        .ok_or_else(|| std::io::Error::other("second duplicate call was discarded"))?;
    let SessionEvent::AssistantMessage { response_items, .. } = assistant else {
        return Err(std::io::Error::other("assistant event changed variant").into());
    };
    assert_eq!(response_items.len(), 1);
    assert_eq!(response_items[0].item.id(), Some("fc_duplicate_second"));
    assert!(
        view.events
            .iter()
            .any(|event| event.base().id == second_output)
    );
    assert!(crate::session::unresolved_local_tool_calls(&view.events).is_empty());
    Ok(())
}

#[test]
fn mixed_tool_families_with_one_call_id_do_not_cross_resolve()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let assistant_id = store.append(canonical_assistant(vec![
        canonical_item(
            serde_json::json!({
                "type": "function_call",
                "id": "fc_mixed",
                "call_id": "call_mixed",
                "name": "read",
                "arguments": "{}",
                "status": "completed"
            }),
            1,
        )?,
        canonical_item(
            serde_json::json!({
                "type": "custom_tool_call",
                "id": "ctc_mixed",
                "call_id": "call_mixed",
                "name": "patch",
                "input": "change",
                "status": "completed"
            }),
            2,
        )?,
    ]))?;
    let custom_output = store.append(canonical_assistant(vec![canonical_item(
        serde_json::json!({
            "type": "custom_tool_call_output",
            "id": "ctco_mixed",
            "call_id": "call_mixed",
            "output": "changed",
            "status": "completed"
        }),
        3,
    )?]))?;
    let function_output = store.append(canonical_assistant(vec![canonical_item(
        serde_json::json!({
            "type": "function_call_output",
            "id": "fco_mixed",
            "call_id": "call_mixed",
            "output": "read",
            "status": "completed"
        }),
        4,
    )?]))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, custom_output)?;

    let view = construct_prompt(&store, &edits);
    let assistant = view
        .events
        .iter()
        .find(|event| event.base().id == assistant_id)
        .ok_or_else(|| std::io::Error::other("function call was discarded"))?;
    let SessionEvent::AssistantMessage { response_items, .. } = assistant else {
        return Err(std::io::Error::other("assistant event changed variant").into());
    };
    assert_eq!(response_items.len(), 1);
    assert_eq!(response_items[0].item.item_type(), "function_call");
    assert!(
        view.events
            .iter()
            .any(|event| event.base().id == function_output)
    );
    assert!(crate::session::unresolved_local_tool_calls(&view.events).is_empty());
    Ok(())
}
