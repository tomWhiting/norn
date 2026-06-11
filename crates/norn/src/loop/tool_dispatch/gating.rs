//! Per-call gating and completion phases of the dispatch pipeline.
//!
//! [`prepare_tool_call`] is the consent boundary (H16): permission
//! evaluation (deny / ask) followed by the pre-tool hook chain.
//! [`finish_blocked_call`] / [`finish_executed_call`] are the completion
//! phase: append the result, record the action-log completion, fire
//! post-tool hooks, and emit rule-engine events. [`run_single_call`]
//! chains the phases for one serial call. See the parent module docs for
//! the consent-boundary semantics and ordering guarantees.

use std::time::Duration;

use serde_json::Value;

use std::sync::Arc;

use crate::config::permissions::{PermissionDecision, PermissionPolicy};
use crate::error::{HookType, NornError, ToolError};
use crate::integration::diagnostics::{DiagnosticSeverity, NornDiagnostic};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::assembly::{AssembledResponse, AssembledToolCall};
use crate::r#loop::config::{AgentLoopConfig, ToolExecutor};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::rule_wiring::build_runtime_events;
use crate::provider::agent_event::AgentEventSender;
use crate::rules::types::RuleInjection;
use crate::session::action_log::{CompletionRecord, Outcome};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

use super::{
    SingleToolResult, ToolResultRecord, append_tool_result, build_envelope, execute_single_tool,
    record_dispatch_completion,
};
use crate::provider::request::Message;

/// Shared, immutable references threaded through every per-call phase of
/// a dispatch. Bundling them keeps the phase helpers within the
/// `too_many_arguments` budget.
pub(super) struct CallEnv<'a> {
    pub(super) executor: &'a dyn ToolExecutor,
    pub(super) store: &'a EventStore,
    pub(super) config: &'a AgentLoopConfig,
    pub(super) event_tx: Option<&'a AgentEventSender>,
    /// Consent-boundary policy resolved from the executor's shared
    /// [`ToolContext`], when installed.
    pub(super) permissions: Option<Arc<PermissionPolicy>>,
}

/// Gating verdict for one call, produced before execution.
pub(super) enum CallPlan {
    /// The call may execute. `args_override` carries hook-modified
    /// arguments serialized back to JSON; `None` means dispatch with the
    /// model's raw argument string.
    Execute { args_override: Option<String> },
    /// The call is blocked (permission deny / unanswerable ask / pre-tool
    /// hook block). `output` is the model-facing structured error and
    /// `reason` the action-log block reason.
    Blocked { output: Value, reason: String },
}

/// One call after the gating phase: envelope, description, and verdict.
pub(super) struct PreparedCall {
    pub(super) tc_index: usize,
    pub(super) envelope: ToolEnvelope,
    pub(super) description: String,
    pub(super) plan: CallPlan,
}

/// Run the gating phase for one call: permission evaluation (deny / ask)
/// followed by the pre-tool hook chain. See the parent module docs for
/// the consent-boundary semantics.
pub(super) async fn prepare_tool_call(
    env: &CallEnv<'_>,
    response: &AssembledResponse,
    tc_index: usize,
    loop_context: &LoopContext,
) -> Result<PreparedCall, NornError> {
    let tc = &response.tool_calls[tc_index];
    let (mut envelope, description) = build_envelope(tc);
    let tool_ctx = ToolContext::empty();

    if let Some(policy) = env.permissions.as_deref() {
        let pre_tool_hooks = loop_context
            .hooks
            .as_deref()
            .map_or(0, HookRegistry::pre_tool_len);
        let blocked_reason = match policy.evaluate(&tc.name, &envelope.model_args) {
            PermissionDecision::Deny { rule } => {
                Some(format!("denied by permissions.deny rule '{rule}'"))
            }
            PermissionDecision::Ask { rule } if pre_tool_hooks == 0 => Some(format!(
                "permissions.ask rule '{rule}' requires consent; no interactive handler is \
                 configured",
            )),
            // With at least one PreToolHook registered, the hook chain
            // below is the consent mechanism for ask-matched calls: a
            // Block refuses, anything else is consent.
            PermissionDecision::Ask { .. } | PermissionDecision::Allow => None,
        };
        if let Some(reason) = blocked_reason {
            report_permission_diagnostic(loop_context, &tc.name, &reason);
            return Ok(PreparedCall {
                tc_index,
                envelope,
                description,
                plan: CallPlan::Blocked {
                    output: serde_json::json!({
                        "error": format!("blocked by permissions: {reason}"),
                    }),
                    reason,
                },
            });
        }
    }

    let plan = if let Some(hooks) = loop_context.hooks.as_deref() {
        match hooks.run_pre_tool(&envelope, &tool_ctx).await {
            HookOutcome::Block { reason } => CallPlan::Blocked {
                output: serde_json::json!({
                    "error": format!("blocked by hook ({:?}): {reason}", HookType::PreTool),
                }),
                reason,
            },
            HookOutcome::Modify { updated_input } => {
                let serialized = serde_json::to_string(&updated_input).map_err(|e| {
                    NornError::Tool(ToolError::ExecutionFailed {
                        reason: format!("failed to serialize hook-modified args: {e}"),
                    })
                })?;
                envelope.model_args = updated_input;
                CallPlan::Execute {
                    args_override: Some(serialized),
                }
            }
            HookOutcome::Proceed => CallPlan::Execute {
                args_override: None,
            },
        }
    } else {
        CallPlan::Execute {
            args_override: None,
        }
    };

    Ok(PreparedCall {
        tc_index,
        envelope,
        description,
        plan,
    })
}

/// Push a `permission-blocked` warning onto the loop's diagnostic
/// collector, when one is wired. Observational only.
fn report_permission_diagnostic(loop_context: &LoopContext, tool_name: &str, reason: &str) {
    if let Some(collector) = loop_context.diagnostics.as_ref() {
        collector.report(NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "permission-blocked".to_owned(),
            message: reason.to_owned(),
            source_tool: Some(tool_name.to_owned()),
            file_path: None,
            suggestion: None,
        });
    }
}

/// Append a blocked call's result and record it on the action log.
/// Post-tool hooks and rule events do not fire for blocked calls,
/// matching the long-standing hook-block behaviour.
pub(super) async fn finish_blocked_call(
    env: &CallEnv<'_>,
    messages: &mut Vec<Message>,
    loop_context: &LoopContext,
    tc: &AssembledToolCall,
    prepared: &PreparedCall,
    output: &Value,
    reason: &str,
) -> Result<(), NornError> {
    append_tool_result(
        env.store,
        messages,
        ToolResultRecord {
            tool_call_id: &tc.call_id,
            tool_name: &tc.name,
            kind: tc.kind,
            output,
            duration_ms: 0,
        },
        loop_context.hooks.as_deref(),
        env.event_tx,
    )
    .await?;
    record_dispatch_completion(
        loop_context,
        CompletionRecord {
            tool_name: &tc.name,
            tool_call_id: &tc.call_id,
            tool_use_description: &prepared.description,
            outcome: Outcome::Blocked {
                reason: reason.to_owned(),
            },
            output,
            args: prepared.envelope.model_args.clone(),
            duration_ms: 0,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        },
    );
    Ok(())
}

/// Append an executed call's result, record the completion, fire
/// post-tool hooks, and emit rule-engine events. Returns the rule
/// injections produced by the call.
pub(super) async fn finish_executed_call(
    env: &CallEnv<'_>,
    messages: &mut Vec<Message>,
    loop_context: &LoopContext,
    tc: &AssembledToolCall,
    prepared: &PreparedCall,
    result: SingleToolResult,
) -> Result<Vec<RuleInjection>, NornError> {
    let SingleToolResult {
        output,
        duration_ms,
        follow_ups,
        post_validate_outcome,
    } = result;

    append_tool_result(
        env.store,
        messages,
        ToolResultRecord {
            tool_call_id: &tc.call_id,
            tool_name: &tc.name,
            kind: tc.kind,
            output: &output,
            duration_ms,
        },
        loop_context.hooks.as_deref(),
        env.event_tx,
    )
    .await?;

    // Record after the result event is in the store so detail/context
    // queries find the matching event. The coarse outcome mirrors the
    // existing follow-up-chain convention: an `error` key means failure.
    let outcome = match output.get("error").and_then(|v| v.as_str()) {
        Some(message) => Outcome::Error {
            message: message.to_owned(),
        },
        None => Outcome::Success,
    };
    record_dispatch_completion(
        loop_context,
        CompletionRecord {
            tool_name: &tc.name,
            tool_call_id: &tc.call_id,
            tool_use_description: &prepared.description,
            outcome,
            output: &output,
            args: prepared.envelope.model_args.clone(),
            duration_ms,
            follow_ups,
            post_validate_outcome,
            level_1_only: false,
        },
    );

    if let Some(hooks) = loop_context.hooks.as_deref() {
        let tool_ctx = ToolContext::empty();
        let post_output = ToolOutput {
            content: output.clone(),
            is_error: output.get("error").is_some(),
            duration: Duration::from_millis(duration_ms),
        };
        hooks
            .run_post_tool(&prepared.envelope, &post_output, &tool_ctx)
            .await;
        // NH-006 R7 / C59: PostToolFailureHook fires in addition to the
        // existing PostToolHook, but only when the tool output indicates
        // an error (matches the same `is_error` test the post-tool block
        // uses for parity). Observational — no control flow effect.
        if post_output.is_error {
            hooks
                .run_post_tool_failure(&prepared.envelope, &post_output, &tool_ctx)
                .await;
        }
    }

    let mut injections = Vec::new();
    if loop_context.rules.is_some() && tc.name != env.config.schema_tool_name {
        let events = build_runtime_events(&tc.name, &tc.arguments);
        if let Some(engine) = loop_context.rules.as_ref() {
            for ev in &events {
                injections.extend(engine.process_event(ev).await);
            }
        }
    }

    Ok(injections)
}

/// Run the full pipeline for one call: gate, execute (when permitted),
/// then append / record / post-hook.
pub(super) async fn run_single_call(
    env: &CallEnv<'_>,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    tc_index: usize,
    loop_context: &LoopContext,
) -> Result<Vec<RuleInjection>, NornError> {
    let prepared = prepare_tool_call(env, response, tc_index, loop_context).await?;
    let tc = &response.tool_calls[tc_index];
    match &prepared.plan {
        CallPlan::Blocked { output, reason } => {
            let output = output.clone();
            let reason = reason.clone();
            finish_blocked_call(env, messages, loop_context, tc, &prepared, &output, &reason)
                .await?;
            Ok(Vec::new())
        }
        CallPlan::Execute { args_override } => {
            let args_str: &str = args_override.as_deref().unwrap_or(&tc.arguments);
            let result = execute_single_tool(
                env.executor,
                &tc.name,
                &tc.call_id,
                args_str,
                loop_context.diagnostics.as_ref(),
            )
            .await;
            finish_executed_call(env, messages, loop_context, tc, &prepared, result).await
        }
    }
}
