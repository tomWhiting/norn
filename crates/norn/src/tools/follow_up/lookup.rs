//! Action-log lookup for the `follow_up` tool.
//!
//! The `follow_up` tool references a prior call by `tool_call_id` and resolves
//! the named deferred action from that call's registered follow-ups. This
//! module owns the read path against the session
//! [`ActionLog`](crate::session::action_log::ActionLog): fetching the log from
//! the [`ToolContext`], loading the original call's arguments and follow-up
//! vector, and selecting an action by exact name.
//!
//! Lookup misses are returned as model-facing [`ToolOutput`] failures
//! (typed [`ToolErrorKind::NotFound`] payloads) rather than [`ToolError`]s —
//! a missing `tool_call_id` or `action` is a correctable model mistake, not
//! an infrastructure failure. [`ToolError`] is reserved for genuine
//! configuration problems (no action log published on the context).

use std::sync::Arc;

use crate::error::ToolError;
use crate::session::action_log::ActionLog;
use crate::tool::context::ToolContext;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::FollowUpAction;
use crate::tool::traits::ToolOutput;

/// The original call's data needed to execute a follow-up: its recorded
/// arguments and the full ordered follow-up vector.
#[derive(Debug)]
pub struct LoadedCall {
    /// Arguments the original tool was dispatched with.
    pub args: serde_json::Value,
    /// Follow-up actions registered for the original call, in registration
    /// order.
    pub follow_ups: Vec<FollowUpAction>,
}

/// Result of resolving an action name within a call's follow-up vector.
pub enum ActionSelection {
    /// An action with the requested name exists (expiry is checked separately).
    Found(Box<FollowUpAction>),
    /// No action with the requested name exists. `available` lists the names
    /// of the still-valid (non-expired) actions for the call.
    NotFound {
        /// Non-expired action names available for the call, in registration
        /// order.
        available: Vec<String>,
    },
}

/// Fetch the session [`ActionLog`] published on the tool context.
///
/// # Errors
///
/// Returns [`ToolError::MissingExtension`] when no action log is configured —
/// the tool cannot resolve any reference without it.
pub fn action_log_from_ctx(ctx: &ToolContext) -> Result<Arc<ActionLog>, ToolError> {
    ctx.require_extension::<ActionLog>()
}

/// Build the structured `tool_call_id not found` error output.
#[must_use]
pub fn not_found_output(tool_call_id: &str) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "tool_call_id": tool_call_id }),
        ToolErrorPayload::new(ToolErrorKind::NotFound, "tool_call_id not found")
            .with_detail(serde_json::json!({ "tool_call_id": tool_call_id })),
    )
}

/// Build the structured `action not found` error output, listing the
/// non-expired actions available for the call.
#[must_use]
pub fn action_not_found_output(
    tool_call_id: &str,
    action: &str,
    available_actions: &[String],
) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({
            "tool_call_id": tool_call_id,
            "action": action,
            "available_actions": available_actions,
        }),
        ToolErrorPayload::new(ToolErrorKind::NotFound, "action not found").with_detail(
            serde_json::json!({
                "action": action,
                "available_actions": available_actions,
            }),
        ),
    )
}

/// Load the original call's arguments and follow-up vector.
///
/// Returns a boxed [`ToolOutput`] carrying the structured `tool_call_id not
/// found` error when the call is absent from the log, so the caller can
/// surface it directly to the model.
///
/// # Errors
///
/// Returns the structured not-found [`ToolOutput`] when `tool_call_id` has no
/// entry in the action log.
pub fn load_call(
    action_log: &ActionLog,
    tool_call_id: &str,
) -> Result<LoadedCall, Box<ToolOutput>> {
    match action_log.get_detail(tool_call_id) {
        Some(detail) => Ok(LoadedCall {
            args: detail.args,
            follow_ups: detail.follow_ups,
        }),
        None => Err(Box::new(not_found_output(tool_call_id))),
    }
}

/// Resolve `action_name` within `follow_ups` by exact name comparison.
///
/// An action is matched by exact name against the full vector regardless of
/// expiry (so an expired-but-present action surfaces a precise expiry error
/// rather than a generic "not found"). When no name matches, the returned
/// `available` list contains only the names of actions that pass
/// `is_unexpired`, in registration order — expired actions are hidden from the
/// available set.
#[must_use]
pub fn select_action<F>(
    follow_ups: &[FollowUpAction],
    action_name: &str,
    is_unexpired: F,
) -> ActionSelection
where
    F: Fn(&FollowUpAction) -> bool,
{
    if let Some(found) = follow_ups.iter().find(|a| a.action == action_name) {
        ActionSelection::Found(Box::new(found.clone()))
    } else {
        let available = follow_ups
            .iter()
            .filter(|a| is_unexpired(a))
            .map(|a| a.action.clone())
            .collect();
        ActionSelection::NotFound { available }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::session::action_log::{CompletionRecord, Outcome};
    use crate::session::store::EventStore;
    use crate::tool::follow_up::{BeforeContentSource, Confidence, ExpiryCondition};

    fn follow_up(action: &str, tool: &str) -> FollowUpAction {
        FollowUpAction {
            action: action.to_owned(),
            description: format!("{action} via {tool}"),
            tool: tool.to_owned(),
            args: serde_json::json!({}),
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }
    }

    fn seeded_log(follow_ups: Vec<FollowUpAction>) -> ActionLog {
        let log = ActionLog::new(Arc::new(EventStore::new()));
        log.record_completion(CompletionRecord {
            tool_name: "edit",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &serde_json::json!({}),
            args: serde_json::json!({ "file": "src/a.rs", "n": 7 }),
            duration_ms: 0,
            follow_ups,
            post_validate_outcome: None,
            level_1_only: false,
        });
        log
    }

    #[test]
    fn load_call_returns_args_and_follow_ups() {
        let log = seeded_log(vec![follow_up("undo", "apply_patch")]);
        let loaded = load_call(&log, "tc-1").expect("call present");
        assert_eq!(
            loaded.args.get("n").and_then(serde_json::Value::as_i64),
            Some(7)
        );
        assert_eq!(loaded.follow_ups.len(), 1);
        assert_eq!(loaded.follow_ups[0].action, "undo");
    }

    #[test]
    fn load_call_missing_returns_not_found_output() {
        let log = seeded_log(Vec::new());
        let out = load_call(&log, "missing").expect_err("call absent");
        assert!(out.is_error());
        assert_eq!(out.content["error"]["kind"], "not_found");
        assert_eq!(out.content["error"]["message"], "tool_call_id not found");
        assert_eq!(out.content["tool_call_id"], "missing");
    }

    #[test]
    fn select_action_finds_by_exact_name() {
        let actions = vec![
            follow_up("undo", "apply_patch"),
            follow_up("reapply", "apply_patch"),
        ];
        match select_action(&actions, "reapply", |_| true) {
            ActionSelection::Found(a) => assert_eq!(a.action, "reapply"),
            ActionSelection::NotFound { .. } => panic!("should have found reapply"),
        }
    }

    #[test]
    fn select_action_missing_lists_only_unexpired() {
        let actions = vec![
            follow_up("undo", "apply_patch"),
            follow_up("reapply", "apply_patch"),
        ];
        // Treat "reapply" as expired; only "undo" should be advertised.
        let is_unexpired = |a: &FollowUpAction| a.action != "reapply";
        match select_action(&actions, "nonexistent", is_unexpired) {
            ActionSelection::NotFound { available } => {
                assert_eq!(available, vec!["undo".to_owned()]);
            }
            ActionSelection::Found(_) => panic!("should not have matched"),
        }
    }

    #[test]
    fn select_action_present_but_expired_is_still_found() {
        // Exact-name match wins over expiry so the caller can raise a precise
        // expiry error rather than a generic not-found.
        let actions = vec![follow_up("reapply", "apply_patch")];
        match select_action(&actions, "reapply", |_| false) {
            ActionSelection::Found(a) => assert_eq!(a.action, "reapply"),
            ActionSelection::NotFound { .. } => panic!("exact match must be Found"),
        }
    }
}
