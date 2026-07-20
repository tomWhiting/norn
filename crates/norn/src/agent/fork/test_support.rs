use super::*;
use crate::session::ProviderFilteredForkBoundary;
use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(super) fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_string(),
    }
}

pub(super) fn assistant_with_tool_calls(calls: &[(&str, &str)]) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "calling tool".to_string(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: calls
            .iter()
            .map(|&(call_id, name)| ToolCallEvent {
                call_id: call_id.to_string(),
                name: name.to_string(),
                arguments: serde_json::json!({}),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            })
            .collect(),
        usage: EventUsage::default(),
        stop_reason: String::new(),
        response_id: None,
    }
}

pub(super) fn tool_result(call_id: &str, name: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: call_id.to_string(),
        tool_name: name.to_string(),
        output: serde_json::json!({"content":"hi"}),
        spool_ref: None,
        duration_ms: 5,
    }
}

pub(super) fn label() -> SessionEvent {
    SessionEvent::Label {
        base: EventBase::new(None),
        label: "checkpoint".to_string(),
        description: None,
    }
}

pub(super) fn filtered_payload(events: &[SessionEvent]) -> Option<&[SessionEvent]> {
    let (boundary, payload) = events.split_last()?;
    assert!(ProviderFilteredForkBoundary::is_family(boundary));
    Some(payload)
}

pub(super) fn golden_identity_policy() -> crate::agent::child_policy::ChildPolicy {
    use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    ChildPolicy {
        messaging: MessagingScope::SiblingsAndParent,
        delegation: DelegationBudget {
            remaining_depth: 1,
            max_concurrent_children: 4,
        },
        inbound_capacity: 8,
        loop_config: None,
    }
}
