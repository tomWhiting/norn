//! Post-step repair for local tool calls that did not reach a result.

use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

use super::helpers::append_off_executor;

/// Ensure every tool call in the event history has a legacy `ToolResult` or
/// canonical function/custom call-output item in the store. Appends synthetic
/// cancelled results for any that are missing.
///
/// Called after `run_agent_step` returns (all exit paths) and after external
/// cancellation (for example, Ctrl+C dropping the step future). This guarantees
/// the store is always in a valid state where no tool call is orphaned.
pub async fn ensure_tool_results_complete(store: &EventStore) {
    let events = store.events();
    let tool_calls = crate::session::unresolved_local_tool_calls(&events);

    for tool_call in &tool_calls {
        let event = SessionEvent::ToolResult {
            base: EventBase::new(store.last_event_id()),
            tool_call_id: tool_call.call_id.clone(),
            tool_name: tool_call.name.clone(),
            output: serde_json::json!({
                "error": "execution cancelled before completion"
            }),
            spool_ref: None,
            duration_ms: 0,
        };
        if let Err(error) = append_off_executor(store, event) {
            tracing::error!(
                tool_call_id = %tool_call.call_id,
                %error,
                "failed to append cancelled tool result",
            );
        }
    }
}
