//! Injection paths for trusted operator input and completed child results.

use std::fmt::Write as _;

use crate::agent::result_channel::{ChildAgentResult, frame_child_result};
use crate::error::SessionError;
use crate::integration::hooks::HookRegistry;
use crate::r#loop::active_input::{ActiveInput, ActiveInputReceiver};
use crate::r#loop::children_usage::ChildrenUsage;
use crate::provider::request::{Message, MessageRole, ToolCallCaller};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::helpers::append_and_notify;

/// Drain human active-turn input and persist it as ordinary user messages.
///
/// Active input is trusted operator text, not inter-agent traffic, so it does
/// not use `<agent_message>` framing. A delivery acknowledgement is emitted only
/// after the corresponding `UserMessage` event has been appended and the local
/// conversation has been updated.
pub(super) async fn flush_active_inputs(
    store: &EventStore,
    messages: &mut Vec<Message>,
    active_input: Option<&mut ActiveInputReceiver>,
    hooks: Option<&HookRegistry>,
) -> Result<Vec<EventId>, SessionError> {
    let Some(active_input) = active_input else {
        return Ok(Vec::new());
    };
    inject_active_inputs(store, messages, active_input.drain(), hooks).await
}

async fn inject_active_inputs(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inputs: Vec<ActiveInput>,
    hooks: Option<&HookRegistry>,
) -> Result<Vec<EventId>, SessionError> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let mut event_ids = Vec::with_capacity(inputs.len());
    for input in inputs {
        let content = input.content().to_string();
        let event_id = append_and_notify(
            store,
            SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: content.clone(),
            },
            hooks,
        )
        .await?;
        messages.push(Message {
            response_items: Vec::new(),
            role: MessageRole::User,
            content: Some(content),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        });
        input.mark_delivered();
        event_ids.push(event_id);
    }
    Ok(event_ids)
}

/// Drain pending child-agent results and inject them into the running
/// conversation. Returns `true` if any results were injected.
///
/// `seed` carries a result that was already received outside this call -
/// the linger-await ([`super::linger`]) consumes one result when it wakes
/// and hands it here so every delivery, mid-run or lingering, goes through
/// this single injection path. `rx: None` with a seed injects the seed
/// alone; `rx: None` without one is a no-op.
///
/// Each result renders through [`frame_child_result`] - the same
/// harness-built, content-escaped framing contract as inbound messages, so a
/// child's output cannot forge an `<agent_message>` or `<agent_result>` frame
/// in the parent's conversation. Each drained batch is persisted as one
/// `UserMessage` event and pushed as one user-role message, keeping the
/// persisted event stream and live conversation in 1:1 correspondence.
///
/// W3.6 usage rollup: every drained result's `subtree_usage` - seed included -
/// is folded into `children_usage`. Because this function is the single
/// consumer of the bounded result channel while its receiver is installed on
/// the loop, every result it consumes is folded exactly once.
pub(super) async fn drain_child_results(
    store: &EventStore,
    messages: &mut Vec<Message>,
    rx: Option<&mut tokio::sync::mpsc::Receiver<ChildAgentResult>>,
    hooks: Option<&HookRegistry>,
    seed: Option<ChildAgentResult>,
    children_usage: &ChildrenUsage,
) -> Result<bool, SessionError> {
    let mut batch: Vec<ChildAgentResult> = seed.into_iter().collect();
    if let Some(rx) = rx {
        while let Ok(result) = rx.try_recv() {
            batch.push(result);
        }
    }
    if batch.is_empty() {
        return Ok(false);
    }
    for result in &batch {
        children_usage.add(&result.subtree_usage);
    }

    let formatted = if batch.len() == 1 {
        frame_child_result(&batch[0])
    } else {
        let mut output = format!("Results from {} completed agents:\n\n", batch.len());
        for result in &batch {
            let _ = write!(output, "{}\n\n", frame_child_result(result));
        }
        output
    };

    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: formatted.clone(),
        },
        hooks,
    )
    .await?;
    messages.push(Message {
        response_items: Vec::new(),
        role: MessageRole::User,
        content: Some(formatted),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: ToolCallCaller::Absent,
    });
    Ok(true)
}
