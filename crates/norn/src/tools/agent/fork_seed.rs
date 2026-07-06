//! Child event-store seeding for [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the R2 seeding step of the fork pipeline: copying the parent's
//! events into the fork's child [`EventStore`] and closing every orphan
//! `tool_call` — including the fork call itself — with a synthetic
//! `ToolResult` so the child context never reaches the provider API with
//! unanswered tool calls. The child store arrives sink-equipped from
//! [`SessionBinding::branch_child`](crate::session::SessionBinding::branch_child)
//! for persistent forks, so every seeded event is written through to the
//! fork's own on-disk timeline. Split out of the fork pipeline (now [`super::fork_context`] /
//! [`super::fork_outcome`]) to
//! keep both files inside the per-file 500-line production-code limit
//! (CO5).

use uuid::Uuid;

use crate::agent::fork::OrphanToolCall;
use crate::error::{NornError, SessionError};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Seed the fork's child [`EventStore`] with the full parent events plus
/// the synthetic tool results that close every orphan `tool_call` —
/// including the fork call itself (R2).
pub(super) fn seed_fork_events(
    child_store: &EventStore,
    parent_events: &[SessionEvent],
    fork_call_id: Option<&str>,
    fork_id: Uuid,
) -> Result<(), NornError> {
    let mut events = parent_events.to_vec();
    let all_orphans = find_all_orphan_tool_calls(&events);
    if !all_orphans.is_empty() {
        let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
        tracing::info!(
            fork_id = %fork_id,
            fork_call_id = ?fork_call_id,
            orphan_count = all_orphans.len(),
            orphan_ids = ?ids,
            "fork: closing orphan tool_calls in child context",
        );
    }
    for orphan in &all_orphans {
        let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
        let output = if is_fork_call {
            serde_json::json!({
                "agent_id": fork_id.to_string(),
                "status": "active",
                "message": crate::agent::fork::FORK_SYNTHETIC_RESULT_MESSAGE,
            })
        } else {
            serde_json::json!({
                "status": "in_progress",
                "message": "executing on parent agent",
            })
        };
        let tool_name = if is_fork_call {
            "fork".to_owned()
        } else {
            orphan.name.clone()
        };
        let parent_id = events.last().map(|e| e.base().id.clone());
        events.push(SessionEvent::ToolResult {
            base: EventBase::new(parent_id),
            tool_call_id: orphan.id.clone(),
            tool_name,
            output,
            duration_ms: 0,
        });
    }
    for event in &events {
        child_store.append(event.clone()).map_err(|e| {
            NornError::Session(SessionError::EventAppendFailed {
                reason: e.to_string(),
            })
        })?;
    }
    Ok(())
}

/// Scan ALL `AssistantMessage` events for `tool_call`s without a matching
/// `ToolResult` anywhere after them. Returns every orphan across the entire
/// history, not just the latest turn. This is the unconditional safety net
/// that ensures the child context never reaches the API with orphans.
fn find_all_orphan_tool_calls(events: &[SessionEvent]) -> Vec<OrphanToolCall> {
    use std::collections::HashSet;

    let mut result_ids: HashSet<String> = HashSet::new();
    for event in events {
        if let SessionEvent::ToolResult { tool_call_id, .. } = event {
            result_ids.insert(tool_call_id.clone());
        }
    }

    let mut orphans = Vec::new();
    for event in events {
        if let SessionEvent::AssistantMessage { tool_calls, .. } = event {
            for tc in tool_calls {
                if !result_ids.contains(&tc.call_id) {
                    orphans.push(OrphanToolCall {
                        id: tc.call_id.clone(),
                        name: tc.name.clone(),
                    });
                }
            }
        }
    }
    orphans
}
