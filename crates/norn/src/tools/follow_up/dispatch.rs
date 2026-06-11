//! Target-tool dispatch for the `follow_up` tool.
//!
//! This module owns the orchestration of a single follow-up execution:
//! parse the model arguments, load the original call, resolve and expiry-check
//! the named action, merge argument overrides, and dispatch the target tool
//! through the shared [`ToolRegistry`]'s full lifecycle (pre-validate,
//! execute, post-validate, on-success, register-follow-ups). The target's
//! result is returned verbatim — the `follow_up` tool contributes no
//! follow-ups of its own.
//!
//! Two runtime handles are read from the [`ToolContext`] extension map:
//!
//! * [`SharedToolRegistry`] — the registry used for nested lifecycle dispatch.
//!   Publishing it on the context lets a tool dispatch another tool without a
//!   compile-time dependency on the agent loop.
//! * [`CurrentTurnId`] — the runtime's current turn id, consulted by
//!   [`ExpiryCondition::TurnScoped`](crate::tool::follow_up::ExpiryCondition::TurnScoped)
//!   checks. Absent on contexts the runtime has not threaded turn state
//!   through, in which case turn-scoped follow-ups are treated as expired.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::ToolError;
use crate::r#loop::runner::ToolExecutor;
use crate::session::action_log::{CompletionRecord, Outcome};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::ToolOutput;

use super::expiry::check_not_expired;
use super::lookup::{self, ActionSelection};
use super::merge::merge_args;

/// Shared tool registry published on the [`ToolContext`] so tools can dispatch
/// other tools through the full lifecycle.
///
/// The orchestrator wraps the agent's [`ToolRegistry`] in an [`Arc`] and
/// inserts this handle; the registry itself is never mutated through it.
pub struct SharedToolRegistry(pub Arc<ToolRegistry>);

/// The runtime's current turn id, published on the [`ToolContext`] so
/// turn-scoped follow-up expiry can be evaluated.
pub struct CurrentTurnId(pub String);

/// Model-supplied arguments for a `follow_up` call.
#[derive(Debug, Deserialize)]
struct FollowUpArgs {
    /// Tool-call id of the original call whose follow-up is being executed.
    tool_call_id: String,
    /// Name of the deferred action to execute.
    action: String,
}

/// Build the structured `target tool not found` error output.
fn missing_target_output(tool: &str) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "tool": tool }),
        ToolErrorPayload::new(ToolErrorKind::NotFound, "target tool not found")
            .with_detail(serde_json::json!({ "tool": tool })),
    )
}

/// Build the structured invalid-argument-merge error output.
fn merge_error_output(message: &str) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(
            ToolErrorKind::InvalidArguments,
            "could not merge follow-up arguments",
        )
        .with_detail(serde_json::json!({ "reason": message })),
    )
}

/// Execute the deferred action referenced by the `follow_up` envelope.
///
/// Resolves the original call, checks expiry, merges argument overrides, and
/// dispatches the target tool through the registry's full lifecycle. The
/// target's [`ToolOutput`] is returned unchanged. When the dispatch produces a
/// result, an action-log entry is recorded for the target with
/// `source_tool_call_id` set to the original call's id, chaining
/// `original -> follow-up -> result`.
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] for malformed arguments,
/// [`ToolError::MissingExtension`] for missing runtime configuration (no
/// action log / no registry on the context), and propagates any
/// [`ToolError`] the target tool's lifecycle raises (e.g. a gate-mode
/// post-validate failure), so the result matches a direct invocation of the
/// target tool. Model-correctable misses (unknown id, unknown action,
/// expired action, non-object arguments) are returned as failed
/// `Ok(ToolOutput)`s carrying typed error payloads.
pub async fn dispatch_follow_up(
    envelope: &ToolEnvelope,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let started = Instant::now();
    let args: FollowUpArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
        ToolError::ExecutionFailed {
            reason: format!("invalid arguments: {e}"),
        }
    })?;

    let action_log = lookup::action_log_from_ctx(ctx)?;
    let loaded = match lookup::load_call(&action_log, &args.tool_call_id) {
        Ok(loaded) => loaded,
        Err(output) => return Ok(*output),
    };

    let turn_id = ctx.get_extension::<CurrentTurnId>().map(|t| t.0.clone());
    let resolve = |path: &std::path::Path| ctx.resolve_path(path);
    let is_unexpired = |action: &crate::tool::follow_up::FollowUpAction| {
        check_not_expired(
            &action.expires,
            turn_id.as_deref(),
            &args.tool_call_id,
            &action.action,
            &resolve,
        )
        .is_ok()
    };

    let action = match lookup::select_action(&loaded.follow_ups, &args.action, is_unexpired) {
        ActionSelection::Found(action) => action,
        ActionSelection::NotFound { available } => {
            return Ok(lookup::action_not_found_output(
                &args.tool_call_id,
                &args.action,
                &available,
            ));
        }
    };

    if let Err(expired) = check_not_expired(
        &action.expires,
        turn_id.as_deref(),
        &args.tool_call_id,
        &action.action,
        &resolve,
    ) {
        return Ok(ToolOutput::failure_with_content(
            expired.to_content(),
            ToolErrorPayload::new(
                ToolErrorKind::Blocked,
                format!("follow-up expired: {}", expired.reason),
            )
            .with_detail(serde_json::json!({
                "tool_call_id": expired.tool_call_id,
                "action": expired.action,
                "suggestion": expired.suggestion,
            })),
        ));
    }

    let merged = match merge_args(&loaded.args, &action.args) {
        Ok(merged) => merged,
        Err(e) => return Ok(merge_error_output(&e.to_string())),
    };

    let registry = ctx.require_extension::<SharedToolRegistry>()?;

    if registry.0.get(&action.tool).is_none() {
        return Ok(missing_target_output(&action.tool));
    }

    // Dispatch the target through the registry's full lifecycle. A target
    // gate-mode failure propagates as a `ToolError`, matching a direct call.
    let output = registry
        .0
        .execute(&action.tool, &args.tool_call_id, merged.clone())
        .await?;

    record_chain_entry(
        &action_log,
        &action.tool,
        &args.tool_call_id,
        envelope,
        &output,
        merged,
        started.elapsed(),
    );

    // Re-type the target's result: the registry returned model-facing
    // content, so reconstruct the error payload from the `error`-key
    // convention.
    Ok(ToolOutput::from_content(output))
}

/// Record the dispatched target's completion in the action log with a chain
/// reference back to the original call.
fn record_chain_entry(
    action_log: &crate::session::action_log::ActionLog,
    target_tool: &str,
    source_tool_call_id: &str,
    envelope: &ToolEnvelope,
    content: &serde_json::Value,
    args: serde_json::Value,
    duration: Duration,
) {
    let entry_id = format!("{source_tool_call_id}->{target_tool}");
    let outcome = match content
        .get("error")
        .and_then(ToolErrorPayload::from_error_value)
    {
        Some(payload) => Outcome::Error {
            message: payload.message,
        },
        None => Outcome::Success,
    };
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);

    action_log.record_completion(CompletionRecord {
        tool_name: target_tool,
        tool_call_id: &entry_id,
        tool_use_description: &envelope.tool_call_id,
        outcome,
        output: content,
        args,
        duration_ms,
        follow_ups: Vec::new(),
        post_validate_outcome: None,
        level_1_only: false,
    });
}
