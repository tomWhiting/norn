//! Child event-store seeding for [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the R2 seeding step of the fork pipeline: copying the parent's
//! events into the fork's child [`EventStore`] (standalone mode) and
//! closing every orphan `tool_call` — including the fork call itself —
//! with a synthetic `ToolResult` so the child context never reaches the
//! provider API with unanswered tool calls. Split out of
//! [`super::fork_pipeline`] to keep both files inside the per-file
//! 500-line production-code limit (CO5).

use uuid::Uuid;

use crate::agent::fork::OrphanToolCall;
use crate::error::{NornError, SessionError};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Seed the fork's child [`EventStore`] with the full parent events plus
/// the synthetic tool result that closes the orphan fork `tool_call` (R2).
///
/// When `tree_seeded` is true, `tree.branch()` already populated the child
/// store with the full parent context — only the synthetic fork result is
/// appended. When false (standalone mode, no tree), all parent events are
/// copied then the synthetic result is appended.
pub(super) fn seed_fork_events(
    child_store: &EventStore,
    parent_events: &[SessionEvent],
    fork_call_id: Option<&str>,
    fork_id: Uuid,
    tree_seeded: bool,
) -> Result<(), NornError> {
    if tree_seeded {
        // Close ALL orphan tool_calls in the child store unconditionally.
        // No guard conditions — if any tool_call anywhere in the child's
        // event history lacks a matching ToolResult, inject a synthetic one.
        // This mirrors Codex's ensure_call_outputs_present approach: fix
        // orphans generically rather than trying to predict specific causes.
        let child_events = child_store.events();
        let all_orphans = find_all_orphan_tool_calls(&child_events);
        if !all_orphans.is_empty() {
            let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
            tracing::info!(
                fork_id = %fork_id,
                fork_call_id = ?fork_call_id,
                orphan_count = all_orphans.len(),
                orphan_ids = ?ids,
                child_event_count = child_events.len(),
                "fork: closing orphan tool_calls in child context",
            );
        }
        for orphan in &all_orphans {
            let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
            let output = if is_fork_call {
                serde_json::json!({
                    "fork_id": fork_id.to_string(),
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
            child_store
                .append(SessionEvent::ToolResult {
                    base: EventBase::new(child_store.last_event_id()),
                    tool_call_id: orphan.id.clone(),
                    tool_name,
                    output,
                    duration_ms: 0,
                })
                .map_err(|e| {
                    NornError::Session(SessionError::EventAppendFailed {
                        reason: e.to_string(),
                    })
                })?;
        }
    } else {
        // Standalone mode (no tree) — copy all parent events then close
        // ALL orphan tool_calls unconditionally, same as the tree path.
        let mut events = parent_events.to_vec();
        let all_orphans = find_all_orphan_tool_calls(&events);
        if !all_orphans.is_empty() {
            let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
            tracing::info!(
                fork_id = %fork_id,
                fork_call_id = ?fork_call_id,
                orphan_count = all_orphans.len(),
                orphan_ids = ?ids,
                "fork: closing orphan tool_calls in standalone mode",
            );
        }
        for orphan in &all_orphans {
            let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
            let output = if is_fork_call {
                serde_json::json!({
                    "fork_id": fork_id.to_string(),
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
