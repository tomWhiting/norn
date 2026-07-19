use norn::provider::AgentEventKind;
use norn::provider::events::{ProviderEvent, StopReason};
use serde_json::{Value, json};

/// Derive the JSON-RPC `event/*` method name from the native event kind.
#[must_use]
pub(crate) fn agent_event_method(agent_event: &norn::provider::AgentEvent) -> &'static str {
    match &agent_event.event {
        AgentEventKind::Provider(event) => match event {
            ProviderEvent::TextComplete { .. }
            | ProviderEvent::ThinkingComplete { .. }
            | ProviderEvent::RefusalComplete { .. } => "event/message",
            ProviderEvent::ToolCallComplete { .. } => "event/toolCall",
            ProviderEvent::ToolResult { .. } => "event/toolResult",
            ProviderEvent::TextDelta { .. }
            | ProviderEvent::ThinkingDelta { .. }
            | ProviderEvent::RefusalDelta { .. }
            | ProviderEvent::ToolCallDelta { .. } => "event/progress",
            ProviderEvent::Done { .. } => "event/stop",
            ProviderEvent::Compaction { .. }
            | ProviderEvent::ReasoningItemDone { .. }
            | ProviderEvent::ResponseItemDone { .. }
            | ProviderEvent::ResponseStreamEvent { .. }
            | ProviderEvent::ResponseAudioFrame { .. }
            | ProviderEvent::Error { .. } => "event/raw",
        },
        AgentEventKind::Message(_) => "event/message",
        AgentEventKind::UsageEstimate(_) | AgentEventKind::StreamRetry(_) => "event/progress",
        AgentEventKind::Subagent(_) | AgentEventKind::Compaction(_) => "event/raw",
    }
}

pub(super) fn subagent_event_to_value(
    lifecycle: &norn::provider::SubagentLifecycle,
) -> Option<Value> {
    let type_label = match lifecycle {
        norn::provider::SubagentLifecycle::Started { .. } => "subagent_started",
        norn::provider::SubagentLifecycle::Completed { .. } => "subagent_completed",
    };
    let mut value = match serde_json::to_value(lifecycle) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("failed to serialize subagent lifecycle event to NDJSON: {err}");
            return None;
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("phase");
        object.insert("type".to_owned(), json!(type_label));
    }
    Some(value)
}

pub(super) fn message_event_to_value(
    lifecycle: &norn::provider::AgentMessageLifecycle,
) -> Option<Value> {
    let type_label = match lifecycle {
        norn::provider::AgentMessageLifecycle::Sent { .. } => "agent_message_sent",
        norn::provider::AgentMessageLifecycle::Delivered { .. } => "agent_message_delivered",
    };
    let mut value = match serde_json::to_value(lifecycle) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("failed to serialize agent message event to NDJSON: {err}");
            return None;
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("phase");
        object.insert("type".to_owned(), json!(type_label));
    }
    Some(value)
}

pub(super) fn is_delta_event(event: &ProviderEvent) -> bool {
    match event {
        ProviderEvent::TextDelta { .. }
        | ProviderEvent::ThinkingDelta { .. }
        | ProviderEvent::RefusalDelta { .. }
        | ProviderEvent::ToolCallDelta { .. } => true,
        ProviderEvent::ResponseStreamEvent { event } => {
            event.manifest_stage()
                == Some(norn::provider::openai::response_contract::StreamEventStage::Incremental)
        }
        ProviderEvent::TextComplete { .. }
        | ProviderEvent::ThinkingComplete { .. }
        | ProviderEvent::RefusalComplete { .. }
        | ProviderEvent::ReasoningItemDone { .. }
        | ProviderEvent::ResponseItemDone { .. }
        | ProviderEvent::ToolCallComplete { .. }
        | ProviderEvent::ToolResult { .. }
        | ProviderEvent::Compaction { .. }
        | ProviderEvent::ResponseAudioFrame { .. }
        | ProviderEvent::Done { .. }
        | ProviderEvent::Error { .. } => false,
    }
}

fn stop_reason_label(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::ContinueTurn => "continue_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::ContentFilter => "content_filter",
    }
}

/// Translate one provider event into the NC18 NDJSON payload.
pub(super) fn provider_event_to_value(event: &ProviderEvent) -> Option<Value> {
    let value = match event {
        ProviderEvent::TextDelta { text } => json!({
            "type": "text_delta",
            "text": text,
        }),
        ProviderEvent::ThinkingDelta { text } => json!({
            "type": "thinking_delta",
            "text": text,
        }),
        ProviderEvent::RefusalDelta {
            item_id,
            output_index,
            content_index,
            refusal,
        } => json!({
            "type": "refusal_delta",
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "refusal": refusal,
        }),
        ProviderEvent::ToolCallDelta {
            item_id,
            call_id,
            name,
            arguments_delta,
            kind,
        } => json!({
            "type": "tool_call_delta",
            "item_id": item_id,
            "call_id": call_id,
            "name": name,
            "arguments_delta": arguments_delta,
            "kind": kind,
        }),
        ProviderEvent::TextComplete { text } => json!({
            "type": "text",
            "text": text,
        }),
        ProviderEvent::ThinkingComplete { text } => json!({
            "type": "thinking",
            "text": text,
        }),
        ProviderEvent::RefusalComplete {
            item_id,
            output_index,
            content_index,
            refusal,
        } => json!({
            "type": "refusal",
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "refusal": refusal,
        }),
        ProviderEvent::ToolCallComplete {
            call_id,
            name,
            arguments,
            kind,
        } => {
            let args = serde_json::from_str(arguments)
                .unwrap_or_else(|_| Value::String(arguments.clone()));
            json!({
                "type": "tool_call",
                "call_id": call_id,
                "name": name,
                "arguments": args,
                "kind": kind,
            })
        }
        ProviderEvent::ToolResult {
            tool_call_id,
            tool_name,
            output,
            duration_ms,
        } => json!({
            "type": "tool_result",
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "output": output,
            "duration_ms": duration_ms,
        }),
        ProviderEvent::Compaction {
            item_type,
            encrypted_content,
        } => json!({
            "type": "compaction",
            "item_type": item_type,
            "encrypted_content": encrypted_content,
        }),
        ProviderEvent::ReasoningItemDone { item } => json!({
            "type": "reasoning_item",
            "item": item,
        }),
        ProviderEvent::ResponseItemDone { item } => json!({
            "type": "response_item",
            "item": item,
        }),
        ProviderEvent::ResponseStreamEvent { event } => event.raw().clone(),
        // Audio already emitted its lossless raw envelope; provider errors
        // exit through the separate agent-error path. Neither is duplicated.
        ProviderEvent::ResponseAudioFrame { .. } | ProviderEvent::Error { .. } => return None,
        ProviderEvent::Done {
            stop_reason,
            usage,
            response_id,
        } => {
            let mut object = json!({
                "type": "done",
                "stop_reason": stop_reason_label(stop_reason),
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_write_tokens": usage.cache_write_tokens,
                },
            });
            if let Some(cost) = usage.cost_usd {
                object["usage"]["cost_usd"] = json!(cost);
            }
            if let Some(response_id) = response_id {
                object["response_id"] = json!(response_id);
            }
            object
        }
    };
    Some(value)
}
