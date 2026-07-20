use super::*;
use crate::provider::request::MessageRole;
use crate::session::events::{EventBase, EventId, EventUsage};
use crate::session::store::EventStore;
use crate::session::{ResponsePublicationFixture, response_publication_fixture};

pub(super) fn message(role: MessageRole, content: &str) -> Message {
    Message {
        response_items: Vec::new(),
        role,
        content: Some(content.to_string()),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
    }
}

pub(super) fn stored_assistant_events(
    content: &str,
    response_id: &str,
) -> Result<Vec<SessionEvent>, crate::error::SessionError> {
    let fixture = publication_fixture(None)?;
    Ok(vec![
        fixture.boundary,
        fixture.provenance,
        assistant_event(fixture.assistant_base, content, response_id),
    ])
}

pub(super) fn append_stored_assistant(
    store: &EventStore,
    content: &str,
    response_id: &str,
) -> Result<EventId, crate::error::SessionError> {
    let fixture = publication_fixture(store.last_event_id())?;
    let assistant_id = fixture.assistant_base.id.clone();
    store.append_batch(&[
        fixture.boundary,
        fixture.provenance,
        assistant_event(fixture.assistant_base, content, response_id),
    ])?;
    Ok(assistant_id)
}

fn publication_fixture(
    parent_id: Option<EventId>,
) -> Result<ResponsePublicationFixture, crate::error::SessionError> {
    response_publication_fixture(parent_id, true).map_err(|_source| {
        crate::error::SessionError::StorageError {
            reason: "failed to encode the provider-state provenance fixture".to_owned(),
        }
    })
}

fn assistant_event(base: EventBase, content: &str, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base,
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

pub(super) fn config(mode: ConversationStateMode) -> AgentLoopConfig {
    AgentLoopConfig {
        conversation_state: mode,
        ..AgentLoopConfig::default()
    }
}
