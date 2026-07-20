use super::seed_fork_events;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;
use uuid::Uuid;

fn dangling_assistant(call_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: call_id.to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    }
}

fn completed_tool_result(call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: call_id.to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::json!({"content": "complete"}),
        spool_ref: None,
        duration_ms: 1,
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

fn assert_no_synthetic_result(events: &[SessionEvent], call_id: &str) {
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            SessionEvent::ToolResult { tool_call_id, .. } if tool_call_id == call_id
        )
    }));
    assert!(crate::session::unresolved_effective_local_tool_calls(events).is_empty());
}

#[test]
fn identity_fork_does_not_reintroduce_a_suppressed_dangling_call()
-> Result<(), Box<dyn std::error::Error>> {
    let parent = EventStore::new();
    let call_id = "call_suppressed_fork";
    let assistant_id = parent.append(dangling_assistant(call_id))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&parent, assistant_id)?;
    let parent_events = parent.events();
    let child = EventStore::new();

    seed_fork_events(&child, &parent_events, None, Uuid::new_v4())?;

    assert_eq!(
        child.len(),
        parent_events.len(),
        "only audit rows are copied"
    );
    assert_no_synthetic_result(&child.events(), call_id);
    Ok(())
}

#[test]
fn identity_fork_does_not_reintroduce_a_compacted_dangling_call()
-> Result<(), Box<dyn std::error::Error>> {
    let parent = EventStore::new();
    let call_id = "call_compacted_fork";
    let assistant_id = parent.append(dangling_assistant(call_id))?;
    let mut edits = ContextEdits::new();
    edits.summarize(
        &parent,
        vec![assistant_id],
        "hidden interrupted call".to_owned(),
    )?;
    let parent_events = parent.events();
    let child = EventStore::new();

    seed_fork_events(&child, &parent_events, None, Uuid::new_v4())?;

    assert_eq!(
        child.len(),
        parent_events.len(),
        "only audit rows are copied"
    );
    assert_no_synthetic_result(&child.events(), call_id);
    Ok(())
}

#[test]
fn identity_fork_does_not_replace_a_suppressed_output_with_a_synthetic_one()
-> Result<(), Box<dyn std::error::Error>> {
    let parent = EventStore::new();
    let call_id = "call_suppressed_output_fork";
    parent.append(dangling_assistant(call_id))?;
    let output_id = parent.append(completed_tool_result(call_id))?;
    let mut edits = ContextEdits::new();
    edits.suppress(&parent, output_id)?;
    let parent_events = parent.events();
    let child = EventStore::new();

    seed_fork_events(&child, &parent_events, None, Uuid::new_v4())?;

    assert_eq!(
        child.len(),
        parent_events.len(),
        "only audit rows are copied"
    );
    assert_eq!(result_count(&child.events(), call_id), 1);
    assert!(crate::session::unresolved_effective_local_tool_calls(&child.events()).is_empty());
    Ok(())
}

#[test]
fn identity_fork_does_not_replace_a_compacted_output_with_a_synthetic_one()
-> Result<(), Box<dyn std::error::Error>> {
    let parent = EventStore::new();
    let call_id = "call_compacted_output_fork";
    parent.append(dangling_assistant(call_id))?;
    let output_id = parent.append(completed_tool_result(call_id))?;
    let mut edits = ContextEdits::new();
    edits.summarize(
        &parent,
        vec![output_id],
        "completed output omitted".to_owned(),
    )?;
    let parent_events = parent.events();
    let child = EventStore::new();

    seed_fork_events(&child, &parent_events, None, Uuid::new_v4())?;

    assert_eq!(
        child.len(),
        parent_events.len(),
        "only audit rows are copied"
    );
    assert_eq!(result_count(&child.events(), call_id), 1);
    assert!(crate::session::unresolved_effective_local_tool_calls(&child.events()).is_empty());
    Ok(())
}
