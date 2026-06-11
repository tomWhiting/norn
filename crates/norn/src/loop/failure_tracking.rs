//! Per-iteration failure harvesting for the repeated-failure monitor
//! (REVIEW item 4).
//!
//! The iteration monitor's [`RepeatedFailure`](crate::r#loop::iteration::IterationSignal::RepeatedFailure)
//! signal compares normalized error signatures across iterations, but it can
//! only fire if the runner actually feeds it the failures each iteration
//! produced. This module extracts those failures from the session event
//! store: every failed tool execution (executor errors, permission blocks,
//! hook blocks) is persisted as a [`SessionEvent::ToolResult`] whose output
//! object carries a string `"error"` field, so scanning the events appended
//! by a tool batch recovers exactly the failures of that batch.

use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Collect tool-failure descriptions from events appended at or after
/// `from_event_index` (a watermark captured via [`EventStore::len`] before
/// the tool batch ran).
///
/// A failure is any [`SessionEvent::ToolResult`] whose output object carries
/// a string `"error"` field — the encoding the dispatch pipeline uses for
/// failed executions, permission blocks, and hook blocks. Each entry is
/// formatted as `"{tool_name}: {error}"` so the monitor's normalization can
/// distinguish different tools failing with similar messages.
pub(super) fn collect_tool_failures(store: &EventStore, from_event_index: usize) -> Vec<String> {
    let events = store.events();
    events
        .get(from_event_index..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } => output
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(|err| format!("{tool_name}: {err}")),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::events::EventBase;

    fn tool_result(name: &str, output: serde_json::Value) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: format!("call_{name}"),
            tool_name: name.to_string(),
            output,
            duration_ms: 1,
        }
    }

    #[test]
    fn collects_only_error_results_after_watermark() {
        let store = EventStore::new();
        store
            .append(tool_result(
                "early",
                serde_json::json!({"error": "before watermark"}),
            ))
            .expect("append");
        let watermark = store.len();
        store
            .append(tool_result("ok_tool", serde_json::json!({"ok": true})))
            .expect("append");
        store
            .append(tool_result(
                "bash",
                serde_json::json!({"error": "exit code 1"}),
            ))
            .expect("append");
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "not a tool result".to_string(),
            })
            .expect("append");

        let failures = collect_tool_failures(&store, watermark);
        assert_eq!(failures, vec!["bash: exit code 1".to_string()]);
    }

    #[test]
    fn non_string_error_field_is_ignored() {
        let store = EventStore::new();
        store
            .append(tool_result("weird", serde_json::json!({"error": 42})))
            .expect("append");
        assert!(collect_tool_failures(&store, 0).is_empty());
    }

    #[test]
    fn watermark_past_end_is_empty() {
        let store = EventStore::new();
        assert!(collect_tool_failures(&store, 10).is_empty());
    }
}
