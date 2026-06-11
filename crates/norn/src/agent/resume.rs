//! Session-resume reconstruction of derived state.
//!
//! The `action_log` tool documents a session-lifetime contract: every tool
//! call of the session is queryable at Level 1 (and Level 2/3 where data
//! exists). When an [`AgentBuilder`](crate::agent::builder::AgentBuilder)
//! resumes a prior session from an [`EventStore`], the in-memory
//! [`ActionLog`] (and its derived
//! [`MutationLedger`](crate::session::mutation_ledger::MutationLedger))
//! start empty — [`rebuild_action_log`] replays the persisted events to
//! restore both.
//!
//! Reconstruction limits (data the event log does not carry):
//!
//! - Follow-up actions (including `StoredContent` before-content snapshots)
//!   are in-memory closures and are not persisted; rebuilt entries carry no
//!   follow-ups, so a resumed `write` to a pre-existing file is recorded as
//!   `Created` in the mutation ledger rather than `Modified`.
//! - Entry timestamps are reconstruction-time, and mutation-ledger revert
//!   baselines are hashed from the file's content *at resume time* —
//!   external edits made while the session was suspended are treated as
//!   part of the resumed baseline.
//! - Post-validate outcomes are not persisted per tool call and rebuild as
//!   `None`.
//!
//! A reconstruction API on `session::ActionLog` itself (persisting outcome
//! and description on the `ToolResult` event) would remove these gaps; see
//! the module-level note on `session/action_log.rs` ownership.

use std::collections::HashMap;

use crate::session::action_log::{ActionLog, CompletionRecord, Outcome};
use crate::session::events::SessionEvent;
use crate::tool::envelope::split_envelope_fields;

/// Prefix the dispatch path uses for hook-blocked tool outputs. Used to map
/// a persisted `error` string back to [`Outcome::Blocked`] on rebuild.
const BLOCKED_BY_HOOK_PREFIX: &str = "blocked by hook";

/// Replay `events` into `action_log`, restoring Level 1/2 entries and the
/// mutation ledger for every persisted tool call of the resumed session.
///
/// Arguments and the model-supplied `tool_use_description` are recovered
/// from the matching [`SessionEvent::AssistantMessage`] tool call; the
/// coarse outcome is derived from the persisted output using the same
/// `error`-key convention the live dispatch path records with (an `error`
/// string prefixed `blocked by hook` rebuilds as a blocked outcome).
///
/// [`AgentBuilder`](crate::agent::builder::AgentBuilder) calls this
/// automatically when resuming from an
/// [`EventStore`](crate::session::store::EventStore); embedding consumers
/// that construct an [`ActionLog`] around a restored event history
/// themselves (e.g. a CLI rebuilding the action ledger on `--resume`)
/// call it directly with [`EventStore::events`](crate::session::store::EventStore::events).
/// See the module docs for the reconstruction limits — data the event log
/// does not carry (follow-ups, original timestamps, post-validate
/// outcomes) is not restored.
pub fn rebuild_action_log(action_log: &ActionLog, events: &[SessionEvent]) {
    // call_id → (clean tool args, tool_use_description)
    let mut call_meta: HashMap<&str, (serde_json::Value, String)> = HashMap::new();
    for event in events {
        if let SessionEvent::AssistantMessage { tool_calls, .. } = event {
            for tc in tool_calls {
                let split = split_envelope_fields(tc.arguments.clone());
                call_meta.insert(
                    tc.call_id.as_str(),
                    (split.tool_args, split.description.unwrap_or_default()),
                );
            }
        }
    }

    for event in events {
        let SessionEvent::ToolResult {
            tool_call_id,
            tool_name,
            output,
            duration_ms,
            ..
        } = event
        else {
            continue;
        };
        let (args, description) = call_meta
            .remove(tool_call_id.as_str())
            .unwrap_or((serde_json::Value::Null, String::new()));
        let outcome = match output.get("error").and_then(serde_json::Value::as_str) {
            Some(message) if message.starts_with(BLOCKED_BY_HOOK_PREFIX) => Outcome::Blocked {
                reason: message.to_owned(),
            },
            Some(message) => Outcome::Error {
                message: message.to_owned(),
            },
            None => Outcome::Success,
        };
        action_log.record_completion(CompletionRecord {
            tool_name,
            tool_call_id,
            tool_use_description: &description,
            outcome,
            output,
            args,
            duration_ms: *duration_ms,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            // Mirror the live dispatch path: the action_log tool's own
            // historical dispatches stay Level-1-only so the rebuilt log is
            // not bloated with old query results.
            level_1_only: tool_name == "action_log",
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::session::events::{EventBase, EventUsage, ToolCallEvent};
    use crate::session::store::EventStore;

    fn assistant_with_call(call_id: &str, name: &str, args: serde_json::Value) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                arguments: args,
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_owned(),
            response_id: None,
        }
    }

    fn tool_result(call_id: &str, name: &str, output: serde_json::Value) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_owned(),
            tool_name: name.to_owned(),
            output,
            duration_ms: 7,
        }
    }

    /// Fix 7 regression: a resumed session's tool calls are queryable again
    /// — entries, descriptions, args, durations, and outcomes all rebuild
    /// from the persisted events.
    #[test]
    fn rebuild_restores_entries_outcomes_and_detail() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        let events = vec![
            assistant_with_call(
                "tc-ok",
                "read",
                serde_json::json!({
                    "path": "src/a.rs",
                    "tool_use_description": "inspect module a",
                }),
            ),
            tool_result("tc-ok", "read", serde_json::json!({"lines": 3})),
            assistant_with_call("tc-err", "bash", serde_json::json!({"command": "false"})),
            tool_result("tc-err", "bash", serde_json::json!({"error": "exit 1"})),
            assistant_with_call("tc-blocked", "bash", serde_json::json!({"command": "rm"})),
            tool_result(
                "tc-blocked",
                "bash",
                serde_json::json!({"error": "blocked by hook (PreTool): policy"}),
            ),
        ];
        for event in &events {
            store
                .append(event.clone())
                .map_err(|e| format!("append: {e}"))?;
        }

        let log = ActionLog::new(Arc::clone(&store));
        assert!(log.entries().is_empty(), "fresh log starts empty");
        rebuild_action_log(&log, &store.events());

        let entries = log.entries();
        assert_eq!(entries.len(), 3, "every persisted tool call rebuilds");

        let ok = log.entry("tc-ok").ok_or("tc-ok entry missing")?;
        assert!(matches!(ok.outcome, Outcome::Success));
        assert_eq!(ok.tool_use_description, "inspect module a");

        let err = log.entry("tc-err").ok_or("tc-err entry missing")?;
        assert!(matches!(err.outcome, Outcome::Error { .. }));

        let blocked = log.entry("tc-blocked").ok_or("tc-blocked entry missing")?;
        assert!(matches!(blocked.outcome, Outcome::Blocked { .. }));

        let detail = log.get_detail("tc-ok").ok_or("tc-ok detail missing")?;
        assert_eq!(detail.duration_ms, 7);
        assert_eq!(
            detail.args.get("path").and_then(serde_json::Value::as_str),
            Some("src/a.rs"),
            "envelope fields are stripped; clean args are stored",
        );
        Ok(())
    }

    /// Fix 7 regression: the mutation ledger rebuilds from successful
    /// mutation-tool completions in the resumed history.
    #[test]
    fn rebuild_restores_mutation_ledger() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let file = dir.path().join("patched.rs");
        std::fs::write(&file, "fn main() {}\n").map_err(|e| format!("write: {e}"))?;

        let store = Arc::new(EventStore::new());
        let events = vec![
            assistant_with_call(
                "tc-edit",
                "edit",
                serde_json::json!({"path": file.display().to_string()}),
            ),
            tool_result(
                "tc-edit",
                "edit",
                serde_json::json!({
                    "path": file.display().to_string(),
                    "blast_radius": {"lines_added": 2, "lines_removed": 1},
                }),
            ),
        ];
        for event in &events {
            store
                .append(event.clone())
                .map_err(|e| format!("append: {e}"))?;
        }

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());

        let mutations = log.mutation_entries();
        assert_eq!(mutations.len(), 1, "edit completion rebuilds the ledger");
        assert_eq!(mutations[0].file_path, file);
        assert_eq!(mutations[0].diff_stats.lines_added, 2);
        assert_eq!(mutations[0].diff_stats.lines_removed, 1);
        Ok(())
    }

    /// Failed tool calls never reach the mutation ledger on rebuild —
    /// mirrors the live `record_completion` success gate.
    #[test]
    fn rebuild_skips_ledger_for_failed_mutations() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-bad",
                "edit",
                serde_json::json!({"path": "/nope.rs", "error": "no match"}),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        assert!(log.mutation_entries().is_empty());
        assert_eq!(log.entries().len(), 1, "the entry itself is still listed");
        Ok(())
    }
}
