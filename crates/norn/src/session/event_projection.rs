//! Canonical-first projections over persisted session events.

use crate::provider::request::ToolCallKind;
use crate::provider::response_item::{ResponseContentPart, ResponseItem};

use super::events::{SessionEvent, ToolCallEvent};

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
                .filter_map(|entry| match &entry.item {
                    ResponseItem::FunctionCall(call) => Some(ToolCallEvent {
                        call_id: call.call_id().to_owned(),
                        name: call.name().to_owned(),
                        arguments: serde_json::from_str(call.arguments()).unwrap_or_else(|_| {
                            serde_json::Value::String(call.arguments().to_owned())
                        }),
                        kind: ToolCallKind::Function,
                    }),
                    ResponseItem::CustomToolCall(call) => Some(ToolCallEvent {
                        call_id: call.call_id().to_owned(),
                        name: call.name().to_owned(),
                        arguments: serde_json::Value::String(call.input().to_owned()),
                        kind: ToolCallKind::Custom,
                    }),
                    _ => None,
                })
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
