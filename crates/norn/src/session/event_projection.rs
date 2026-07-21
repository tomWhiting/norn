//! Canonical-first projections over persisted session events.

use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::provider::response_item::{KnownResponseItemKind, ResponseContentPart, ResponseItem};

use super::events::{SessionEvent, ToolCallEvent};

mod atomic;

pub(crate) use atomic::{atomic_local_tool_projection, unresolved_effective_local_tool_calls};

/// Return unresolved local function/custom calls in occurrence order.
///
/// Canonical call-output items resolve only an earlier call with the same
/// family and `call_id`. Legacy [`SessionEvent::ToolResult`] events resolve one
/// earlier call of either family. Outputs before calls, mismatched families,
/// and duplicate IDs therefore cannot erase unrelated pending work.
pub(crate) fn unresolved_local_tool_calls(events: &[SessionEvent]) -> Vec<ToolCallEvent> {
    let mut pending = Vec::new();
    for event in events {
        let _resolved_call = apply_local_tool_event(&mut pending, event);
    }
    pending
}

/// Apply one persisted event to the ordered local-call projection.
///
/// Canonical assistant items both open calls and consume earlier calls with a
/// family-compatible output. A legacy tool result consumes one earlier call
/// of either local family and returns that exact call so replay can recover its
/// kind and opaque caller metadata.
pub(crate) fn apply_local_tool_event(
    pending: &mut Vec<ToolCallEvent>,
    event: &SessionEvent,
) -> Option<ToolCallEvent> {
    match event {
        SessionEvent::ToolResult { tool_call_id, .. } => {
            resolve_pending_call(pending, tool_call_id, None)
        }
        SessionEvent::AssistantMessage {
            response_items,
            tool_calls,
            ..
        } => {
            if response_items.is_empty() {
                pending.extend(tool_calls.iter().cloned());
                return None;
            }
            for entry in response_items {
                let _retain_item = apply_local_response_item(pending, &entry.item);
            }
            None
        }
        _ => None,
    }
}

/// Remove local outputs whose originating call is outside `events`.
fn without_orphan_local_tool_outputs(events: Vec<SessionEvent>) -> Vec<SessionEvent> {
    let mut pending = Vec::new();
    let mut retained = Vec::with_capacity(events.len());
    for mut event in events {
        if let SessionEvent::AssistantMessage {
            response_items,
            tool_calls,
            ..
        } = &mut event
        {
            if response_items.is_empty() {
                pending.extend(tool_calls.iter().cloned());
            } else {
                response_items.retain(|entry| apply_local_response_item(&mut pending, &entry.item));
                if response_items.is_empty() {
                    continue;
                }
            }
            retained.push(event);
            continue;
        }
        let resolved_call = apply_local_tool_event(&mut pending, &event);
        if !matches!(event, SessionEvent::ToolResult { .. }) || resolved_call.is_some() {
            retained.push(event);
        }
    }
    retained
}

fn apply_local_response_item(pending: &mut Vec<ToolCallEvent>, item: &ResponseItem) -> bool {
    if let Some(call) = canonical_tool_call(item) {
        pending.push(call);
        return true;
    }
    let output_kind = match item {
        ResponseItem::Known(item) if item.kind() == KnownResponseItemKind::FunctionCallOutput => {
            Some(ToolCallKind::Function)
        }
        ResponseItem::Known(item) if item.kind() == KnownResponseItemKind::CustomToolCallOutput => {
            Some(ToolCallKind::Custom)
        }
        _ => None,
    };
    let Some(kind) = output_kind else {
        return true;
    };
    let Some(call_id) = item.raw().get("call_id").and_then(|id| id.as_str()) else {
        return false;
    };
    resolve_pending_call(pending, call_id, Some(kind)).is_some()
}

fn canonical_tool_call(item: &ResponseItem) -> Option<ToolCallEvent> {
    match item {
        ResponseItem::FunctionCall(call) => Some(ToolCallEvent {
            call_id: call.call_id().to_owned(),
            name: call.name().to_owned(),
            arguments: serde_json::from_str(call.arguments())
                .unwrap_or_else(|_| serde_json::Value::String(call.arguments().to_owned())),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::from_item(item.raw()),
        }),
        ResponseItem::CustomToolCall(call) => Some(ToolCallEvent {
            call_id: call.call_id().to_owned(),
            name: call.name().to_owned(),
            arguments: serde_json::Value::String(call.input().to_owned()),
            kind: ToolCallKind::Custom,
            caller: ToolCallCaller::from_item(item.raw()),
        }),
        _ => None,
    }
}

fn resolve_pending_call(
    pending: &mut Vec<ToolCallEvent>,
    call_id: &str,
    kind: Option<ToolCallKind>,
) -> Option<ToolCallEvent> {
    if let Some(index) = pending
        .iter()
        .position(|call| call.call_id == call_id && kind.is_none_or(|kind| call.kind == kind))
    {
        return Some(pending.remove(index));
    }
    None
}

impl SessionEvent {
    /// Return the assistant tool calls that are authoritative for this event.
    ///
    /// Canonical Responses items win whenever present. The separately stored
    /// `tool_calls` field is a compatibility projection for legacy sessions
    /// and provider-neutral consumers; trusting it beside a non-empty
    /// canonical transcript would create two competing histories.
    #[must_use]
    pub fn assistant_tool_calls(&self) -> Option<Vec<ToolCallEvent>> {
        let Self::AssistantMessage {
            response_items,
            tool_calls,
            ..
        } = self
        else {
            return None;
        };
        if response_items.is_empty() {
            return Some(tool_calls.clone());
        }
        Some(
            response_items
                .iter()
                .filter_map(|entry| canonical_tool_call(&entry.item))
                .collect(),
        )
    }

    /// Return the assistant display text derived from the authoritative form.
    #[must_use]
    pub fn assistant_text(&self) -> Option<String> {
        let Self::AssistantMessage {
            response_items,
            content,
            ..
        } = self
        else {
            return None;
        };
        if response_items.is_empty() {
            return Some(content.clone());
        }
        let mut text = String::new();
        for entry in response_items {
            let Some(message) = entry.item.as_message() else {
                continue;
            };
            for part in message.content() {
                if let ResponseContentPart::OutputText { text: part, .. } = part {
                    text.push_str(part);
                }
            }
        }
        Some(text)
    }
}
