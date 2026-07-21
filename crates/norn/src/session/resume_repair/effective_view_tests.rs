use super::repair_dangling_tool_calls;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;

fn dangling_assistant(call_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: call_id.to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({"command": "pwd"}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    }
}

fn result_count(events: &[SessionEvent], call_id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::ToolResult { tool_call_id, .. } if tool_call_id == call_id
            )
        })
        .count()
}

fn tool_result(call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: call_id.to_owned(),
        tool_name: "bash".to_owned(),
        output: serde_json::json!({"content": "complete"}),
        spool_ref: None,
        duration_ms: 1,
    }
}

#[test]
fn repair_does_not_reintroduce_a_suppressed_dangling_call() -> Result<(), Box<dyn std::error::Error>>
{
    let store = EventStore::new();
    let call_id = "call_suppressed";
    let assistant_id = store.append(dangling_assistant(call_id))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, assistant_id)?;
    let event_count = store.len();

    let repaired = repair_dangling_tool_calls(&store)?;

    assert!(repaired.is_empty());
    assert_eq!(store.len(), event_count, "repair must not append a result");
    assert_eq!(result_count(&store.events(), call_id), 0);
    Ok(())
}

#[test]
fn repair_does_not_reintroduce_a_compacted_dangling_call() -> Result<(), Box<dyn std::error::Error>>
{
    let store = EventStore::new();
    let call_id = "call_compacted";
    let assistant_id = store.append(dangling_assistant(call_id))?;
    let mut edits = ContextEdits::new();
    edits.summarize(
        &store,
        vec![assistant_id],
        "interrupted turn omitted".to_owned(),
    )?;
    let event_count = store.len();

    let repaired = repair_dangling_tool_calls(&store)?;

    assert!(repaired.is_empty());
    assert_eq!(store.len(), event_count, "repair must not append a result");
    assert_eq!(result_count(&store.events(), call_id), 0);
    Ok(())
}

#[test]
fn repair_does_not_replace_a_suppressed_output_with_a_visible_synthetic_one()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let call_id = "call_output_suppressed";
    store.append(dangling_assistant(call_id))?;
    let output_id = store.append(tool_result(call_id))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&store, output_id)?;
    let event_count = store.len();

    let repaired = repair_dangling_tool_calls(&store)?;

    assert!(repaired.is_empty());
    assert_eq!(store.len(), event_count, "repair must not append a result");
    assert_eq!(result_count(&store.events(), call_id), 1);
    assert!(crate::session::unresolved_effective_local_tool_calls(&store.events()).is_empty());
    Ok(())
}

#[test]
fn repair_does_not_replace_a_compacted_output_with_a_visible_synthetic_one()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let call_id = "call_output_compacted";
    store.append(dangling_assistant(call_id))?;
    let output_id = store.append(tool_result(call_id))?;
    let mut edits = ContextEdits::new();
    edits.summarize(
        &store,
        vec![output_id],
        "completed output omitted".to_owned(),
    )?;
    let event_count = store.len();

    let repaired = repair_dangling_tool_calls(&store)?;

    assert!(repaired.is_empty());
    assert_eq!(store.len(), event_count, "repair must not append a result");
    assert_eq!(result_count(&store.events(), call_id), 1);
    assert!(crate::session::unresolved_effective_local_tool_calls(&store.events()).is_empty());
    Ok(())
}
