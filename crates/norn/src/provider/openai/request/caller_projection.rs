//! Ordered projection of canonical tool-call ownership onto tool results.

use crate::error::ProviderError;
use crate::provider::request::{Message, ToolCallCaller, ToolCallKind};
use crate::provider::response_item::{KnownResponseItemKind, ResponseItem};

#[derive(Debug)]
struct PendingCaller {
    call_id: String,
    kind: ToolCallKind,
    caller: ToolCallCaller,
}

/// Tracks prior canonical calls so their opaque `caller` field can be copied
/// unchanged onto the corresponding output item.
#[derive(Debug, Default)]
pub(super) struct ToolCallerProjection {
    pending: Vec<PendingCaller>,
}

impl ToolCallerProjection {
    pub(super) fn observe_assistant(&mut self, message: &Message) -> Result<(), ProviderError> {
        if message.response_items.is_empty() {
            if message
                .tool_calls
                .iter()
                .any(|call| !call.caller.is_valid())
            {
                return Err(invalid_caller());
            }
            self.pending
                .extend(message.tool_calls.iter().map(|call| PendingCaller {
                    call_id: call.call_id.clone(),
                    kind: call.kind,
                    caller: call.caller.clone(),
                }));
            return Ok(());
        }
        for transcript_item in &message.response_items {
            let call = match &transcript_item.item {
                ResponseItem::FunctionCall(call) => Some((call.call_id(), ToolCallKind::Function)),
                ResponseItem::CustomToolCall(call) => Some((call.call_id(), ToolCallKind::Custom)),
                _ => None,
            };
            if let Some((call_id, kind)) = call {
                let caller = ToolCallCaller::from_item(transcript_item.item.raw());
                if !caller.is_valid() {
                    return Err(invalid_caller());
                }
                self.pending.push(PendingCaller {
                    call_id: call_id.to_owned(),
                    kind,
                    caller,
                });
                continue;
            }
            let output_kind = match &transcript_item.item {
                ResponseItem::Known(item)
                    if item.kind() == KnownResponseItemKind::FunctionCallOutput =>
                {
                    Some(ToolCallKind::Function)
                }
                ResponseItem::Known(item)
                    if item.kind() == KnownResponseItemKind::CustomToolCallOutput =>
                {
                    Some(ToolCallKind::Custom)
                }
                _ => None,
            };
            if let Some(kind) = output_kind
                && let Some(call_id) = transcript_item
                    .item
                    .raw()
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
            {
                let _resolved = self.take(call_id, kind);
            }
        }
        Ok(())
    }

    pub(super) fn caller_for_result(
        &mut self,
        message: &Message,
    ) -> Result<ToolCallCaller, ProviderError> {
        let projected = self.take_projected(message);
        if !message.tool_call_caller.is_absent()
            && !projected.is_absent()
            && message.tool_call_caller != projected
        {
            return Err(ProviderError::RequestSerializationFailed {
                reason: "tool result caller conflicts with its canonical originating call"
                    .to_owned(),
            });
        }
        let caller = if message.tool_call_caller.is_absent() {
            projected
        } else {
            message.tool_call_caller.clone()
        };
        if caller.is_valid() {
            Ok(caller)
        } else {
            Err(invalid_caller())
        }
    }

    fn take_projected(&mut self, message: &Message) -> ToolCallCaller {
        let Some(call_id) = message.tool_call_id.as_deref() else {
            return ToolCallCaller::Absent;
        };
        let kind = message.tool_call_kind.unwrap_or_default();
        self.take(call_id, kind)
            .map_or(ToolCallCaller::Absent, |pending| pending.caller)
    }

    fn take(&mut self, call_id: &str, kind: ToolCallKind) -> Option<PendingCaller> {
        let index = self
            .pending
            .iter()
            .position(|pending| pending.kind == kind && pending.call_id == call_id)?;
        Some(self.pending.remove(index))
    }
}

fn invalid_caller() -> ProviderError {
    ProviderError::RequestSerializationFailed {
        reason: "tool call caller did not match the supported Responses caller schema".to_owned(),
    }
}
