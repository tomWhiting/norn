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
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};

/// Prefix the dispatch path uses for hook-blocked tool outputs. Used to map
/// a persisted `error` string back to [`Outcome::Blocked`] on rebuild.
const BLOCKED_BY_HOOK_PREFIX: &str = "blocked by hook";

/// Prefix the dispatch path uses for permission-blocked tool outputs —
/// the other string the consent boundary records with
/// `Outcome::Blocked` (see `loop/tool_dispatch/gating.rs::prepare_tool_call`).
const BLOCKED_BY_PERMISSIONS_PREFIX: &str = "blocked by permissions";

/// Replay `events` into `action_log`, restoring Level 1/2 entries and the
/// mutation ledger for every persisted tool call of the resumed session.
///
/// Arguments and the model-supplied `tool_use_description` are recovered
/// from the matching [`SessionEvent::AssistantMessage`] tool call; the
/// coarse outcome is derived from the persisted output's `error` key via
/// the typed [`ToolErrorPayload`] machinery — covering the object form
/// the dispatch path persists today, the legacy string form in event
/// files written by earlier norn versions, and the consent-boundary
/// block strings — see [`outcome_from_output`].
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
        let outcome = outcome_from_output(output);
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

/// Derive the coarse [`Outcome`] for a rebuilt entry from a persisted
/// tool-result `output`, mirroring how the live dispatch path recorded it:
///
/// * no `error` key → [`Outcome::Success`] (matches
///   `finish_executed_call` deriving Success from an absent typed
///   payload);
/// * an `error` whose re-typed message carries one of the
///   consent-boundary prefixes (`blocked by hook` / `blocked by
///   permissions`) → [`Outcome::Blocked`] when the payload's kind is a
///   block kind (`blocked` / `permission_denied`), or unconditionally for
///   the kind-less legacy string form — `finish_blocked_call` is the only
///   writer of those messages. The bare block reason is recovered from
///   `detail.reason` (both block writers populate it); the full message
///   stands in only for the legacy string form, which carries no detail;
/// * any other `error` value → [`Outcome::Error`], re-typed through
///   [`ToolErrorPayload::from_error_value`] so both the object form the
///   dispatch path persists today (`{"error": {kind, message, detail}}`)
///   and the string form found in session files written by earlier norn
///   versions rebuild identically (message-plus-guidance, exactly what
///   `finish_executed_call` records live). Reading the old string form is
///   data compatibility with event files already on disk — durable user
///   data, not an API compat shim. An `error` value matching neither
///   shape still rebuilds as `Error` (rendering the raw JSON), never as
///   `Success`: the presence of the key is the failure signal.
fn outcome_from_output(output: &serde_json::Value) -> Outcome {
    let Some(error_value) = output.get("error") else {
        return Outcome::Success;
    };
    let Some(payload) = ToolErrorPayload::from_error_value(error_value) else {
        return Outcome::Error {
            message: error_value.to_string(),
        };
    };
    let has_block_prefix = payload.message.starts_with(BLOCKED_BY_HOOK_PREFIX)
        || payload.message.starts_with(BLOCKED_BY_PERMISSIONS_PREFIX);
    // Object form carries the writer's kind, so require a consent-boundary
    // kind alongside the prefix — an unrelated tool error whose message
    // merely begins with the prefix must stay `Error`. The legacy string
    // form has no kind to consult; there the prefix is the only signal.
    let is_block_kind = matches!(
        payload.kind,
        ToolErrorKind::Blocked | ToolErrorKind::PermissionDenied
    );
    if has_block_prefix && (is_block_kind || error_value.is_string()) {
        let reason = payload
            .detail
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&payload.message)
            .to_owned();
        return Outcome::Blocked { reason };
    }
    Outcome::Error {
        message: payload.model_message(),
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

    /// Blocker regression: the dispatch path persists typed OBJECT-form
    /// errors (`{"error": {kind, message, detail}}`). Before the fix the
    /// rebuild only recognised string-form errors, so every typed tool
    /// failure rebuilt as `Outcome::Success` after session resume.
    #[test]
    fn rebuild_derives_error_from_object_form_payload() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        let events = vec![
            assistant_with_call("tc-typed", "bash", serde_json::json!({"command": "false"})),
            tool_result(
                "tc-typed",
                "bash",
                serde_json::json!({
                    "error": {"kind": "execution_failed", "message": "exit 1"},
                }),
            ),
            assistant_with_call("tc-guided", "edit", serde_json::json!({"path": "/a.rs"})),
            tool_result(
                "tc-guided",
                "edit",
                serde_json::json!({
                    "error": {
                        "kind": "blocked",
                        "message": "file has not been read",
                        "detail": {"guidance": "read it first"},
                    },
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

        let typed = log.entry("tc-typed").ok_or("tc-typed entry missing")?;
        match &typed.outcome {
            Outcome::Error { message } => assert_eq!(message, "exit 1"),
            other => return Err(format!("expected Outcome::Error, got {other:?}")),
        }

        // Guidance renders the same way the live dispatch path records it
        // (`ToolErrorPayload::model_message`).
        let guided = log.entry("tc-guided").ok_or("tc-guided entry missing")?;
        match &guided.outcome {
            Outcome::Error { message } => {
                assert_eq!(message, "file has not been read Guidance: read it first");
            }
            other => return Err(format!("expected Outcome::Error, got {other:?}")),
        }
        Ok(())
    }

    /// Live-path parity: consent-boundary blocks are persisted in the typed
    /// object form (`{"error": {"kind": "blocked", "message": "blocked by
    /// hook ...", "detail": {"reason": ...}}}`) and recorded live as
    /// `Outcome::Blocked` with the bare reason — rebuild must restore
    /// exactly that, recovering the reason from `detail.reason`.
    #[test]
    fn rebuild_maps_object_form_hook_block_to_blocked() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-hook-obj",
                "bash",
                serde_json::json!({
                    "error": {
                        "kind": "blocked",
                        "message": "blocked by hook (PreTool): bash blocked",
                        "detail": {"hook": "pre_tool", "reason": "bash blocked"},
                    },
                }),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        let entry = log.entry("tc-hook-obj").ok_or("tc-hook-obj missing")?;
        match &entry.outcome {
            Outcome::Blocked { reason } => assert_eq!(reason, "bash blocked"),
            other => return Err(format!("expected Outcome::Blocked, got {other:?}")),
        }
        Ok(())
    }

    /// Legacy string form (session files written before the typed payload
    /// landed): permission blocks persisted as `{"error": "blocked by
    /// permissions: ..."}` — rebuild must restore Blocked, not a generic
    /// Error. The string form carries no `detail.reason`, so the full
    /// message stands in (documented reconstruction limit).
    #[test]
    fn rebuild_maps_permission_block_string_to_blocked() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-perm",
                "bash",
                serde_json::json!({
                    "error": "blocked by permissions: denied by permissions.deny rule 'no-bash'",
                }),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        let entry = log.entry("tc-perm").ok_or("tc-perm entry missing")?;
        assert!(
            matches!(entry.outcome, Outcome::Blocked { .. }),
            "permission-blocked call must rebuild as Blocked, got {:?}",
            entry.outcome,
        );
        Ok(())
    }

    /// Live-path parity for the typed object form the dispatch gate
    /// persists today: kind `permission_denied`, prefixed message, and the
    /// bare reason in `detail.reason` — rebuild must restore Blocked with
    /// exactly the bare reason `finish_blocked_call` records live.
    #[test]
    fn rebuild_maps_object_form_permission_block_to_blocked_with_bare_reason() -> Result<(), String>
    {
        let store = Arc::new(EventStore::new());
        let reason = "denied by permissions.deny rule 'no-bash'";
        store
            .append(tool_result(
                "tc-perm-obj",
                "bash",
                serde_json::json!({
                    "error": {
                        "kind": "permission_denied",
                        "message": format!("blocked by permissions: {reason}"),
                        "detail": {
                            "rule": "no-bash",
                            "decision": "deny",
                            "reason": reason,
                        },
                    },
                }),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        let entry = log.entry("tc-perm-obj").ok_or("tc-perm-obj missing")?;
        match &entry.outcome {
            Outcome::Blocked { reason: got } => assert_eq!(got, reason),
            other => return Err(format!("expected Outcome::Blocked, got {other:?}")),
        }
        Ok(())
    }

    /// An object-form error whose MESSAGE merely begins with a consent
    /// prefix but whose KIND is not a block kind is an ordinary tool
    /// failure — rebuild must keep it `Error`, matching what the live path
    /// recorded. Only the kind-less legacy string form may classify on the
    /// prefix alone.
    #[test]
    fn rebuild_keeps_non_block_kind_with_block_prefix_as_error() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-imposter",
                "bash",
                serde_json::json!({
                    "error": {
                        "kind": "execution_failed",
                        "message": "blocked by permissions check timing out upstream",
                        "detail": null,
                    },
                }),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        let entry = log.entry("tc-imposter").ok_or("tc-imposter missing")?;
        assert!(
            matches!(entry.outcome, Outcome::Error { .. }),
            "non-block kind must stay Error despite the prefix, got {:?}",
            entry.outcome,
        );
        Ok(())
    }

    /// An `error` key whose value matches neither the typed object form nor
    /// the string form still marks a failure — rebuilding it as Success
    /// would be the silent Success-on-failure class this module just fixed.
    #[test]
    fn rebuild_never_maps_unrecognised_error_shape_to_success() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-odd",
                "bash",
                serde_json::json!({"error": 42}),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        let entry = log.entry("tc-odd").ok_or("tc-odd entry missing")?;
        match &entry.outcome {
            Outcome::Error { message } => {
                assert!(message.contains("42"), "message renders the raw value");
            }
            other => return Err(format!("expected Outcome::Error, got {other:?}")),
        }
        Ok(())
    }

    /// Object-form failures must keep failed mutations out of the rebuilt
    /// mutation ledger, exactly like the legacy string form.
    #[test]
    fn rebuild_skips_ledger_for_object_form_failed_mutations() -> Result<(), String> {
        let store = Arc::new(EventStore::new());
        store
            .append(tool_result(
                "tc-bad-typed",
                "edit",
                serde_json::json!({
                    "path": "/nope.rs",
                    "error": {"kind": "not_found", "message": "no match"},
                }),
            ))
            .map_err(|e| format!("append: {e}"))?;

        let log = ActionLog::new(Arc::clone(&store));
        rebuild_action_log(&log, &store.events());
        assert!(
            log.mutation_entries().is_empty(),
            "typed failure must not rebuild into the mutation ledger",
        );
        assert_eq!(log.entries().len(), 1, "the entry itself is still listed");
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
