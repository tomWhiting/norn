//! Mechanical compaction summaries and tool-result-safe cut selection.

use std::collections::{BTreeMap, HashSet};

use crate::provider::response_item::{ResponseCustomToolCallItem, ResponseFunctionCallItem};
use crate::session::events::SessionEvent;

/// Build the structured JSON digest recorded as an auto-compaction summary.
///
/// Describes the replaced span concretely so the model retains a factual
/// account of what was elided. The digest is mechanically derived and carries
/// an explicit `mechanical_digest` marker.
#[must_use]
pub fn build_compaction_digest(
    replaced: &[SessionEvent],
    token_estimate_freed: usize,
) -> serde_json::Value {
    let mut user_messages = 0_usize;
    let mut assistant_messages = 0_usize;
    let mut tool_results = 0_usize;
    let mut prior_compactions = 0_usize;
    let mut other_events = 0_usize;
    let mut tool_calls: BTreeMap<String, usize> = BTreeMap::new();
    let mut response_item_sequences = Vec::new();
    for event in replaced {
        match event {
            SessionEvent::UserMessage { .. } => user_messages += 1,
            SessionEvent::AssistantMessage {
                base,
                response_items,
                tool_calls: legacy_calls,
                ..
            } => {
                assistant_messages += 1;
                if response_items.is_empty() {
                    for call in legacy_calls {
                        *tool_calls.entry(call.name.clone()).or_insert(0) += 1;
                    }
                } else {
                    response_item_sequences.push(serde_json::json!({
                        "assistant_event_id": base.id.to_string(),
                        "item_types": response_items
                            .iter()
                            .map(|entry| entry.item.item_type())
                            .collect::<Vec<_>>(),
                    }));
                    for entry in response_items {
                        let name = entry
                            .item
                            .as_function_call()
                            .map(ResponseFunctionCallItem::name)
                            .or_else(|| {
                                entry
                                    .item
                                    .as_custom_tool_call()
                                    .map(ResponseCustomToolCallItem::name)
                            });
                        if let Some(name) = name {
                            *tool_calls.entry(name.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }
            SessionEvent::ToolResult { .. } => tool_results += 1,
            SessionEvent::Compaction { .. } => prior_compactions += 1,
            SessionEvent::Custom { .. }
            | SessionEvent::ModelChange { .. }
            | SessionEvent::ProviderEpochBoundary { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::RuleInjection { .. }
            | SessionEvent::ContextMark { .. }
            | SessionEvent::SpokenResponse { .. } => other_events += 1,
        }
    }
    let first = replaced.first().map(SessionEvent::base);
    let last = replaced.last().map(SessionEvent::base);
    serde_json::json!({
        "summary_kind": "mechanical_digest",
        "event_count_suppressed": replaced.len(),
        "token_estimate_freed": token_estimate_freed,
        "user_messages": user_messages,
        "assistant_messages": assistant_messages,
        "tool_results": tool_results,
        "prior_compactions": prior_compactions,
        "other_events": other_events,
        "tool_calls": tool_calls,
        "response_item_sequences": response_item_sequences,
        "first_timestamp": first.map(|base| base.timestamp.to_rfc3339()),
        "last_timestamp": last.map(|base| base.timestamp.to_rfc3339()),
        "turn_range": {
            "start": first.map(|base| base.id.to_string()),
            "end": last.map(|base| base.id.to_string()),
        }
    })
}

pub(crate) fn compact_boundary_after_tool_results(
    events: &[SessionEvent],
    assistant_idx: usize,
) -> usize {
    let call_ids = match events.get(assistant_idx) {
        Some(SessionEvent::AssistantMessage { tool_calls, .. }) => tool_calls
            .iter()
            .map(|call| call.call_id.clone())
            .collect::<HashSet<_>>(),
        _ => HashSet::new(),
    };
    if call_ids.is_empty() {
        return assistant_idx.saturating_add(1);
    }

    let mut unresolved = call_ids;
    let mut idx = assistant_idx.saturating_add(1);
    while idx < events.len() {
        match &events[idx] {
            SessionEvent::ToolResult { tool_call_id, .. } => {
                unresolved.remove(tool_call_id);
                if unresolved.is_empty() {
                    return idx.saturating_add(1);
                }
            }
            SessionEvent::AssistantMessage { .. } => return assistant_idx,
            SessionEvent::UserMessage { .. }
            | SessionEvent::Custom { .. }
            | SessionEvent::ModelChange { .. }
            | SessionEvent::ProviderEpochBoundary { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::RuleInjection { .. }
            | SessionEvent::ContextMark { .. }
            | SessionEvent::SpokenResponse { .. }
            | SessionEvent::Compaction { .. } => {}
        }
        idx += 1;
    }
    assistant_idx
}
