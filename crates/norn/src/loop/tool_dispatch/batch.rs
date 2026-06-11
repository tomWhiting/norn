//! Effect-scheduled batch execution ([`SchedulingPlan`] wiring).
//!
//! [`execute_planned_tool_batch`] orders a batch of tool calls through
//! [`SchedulingPlan`] and executes each step: serial steps run one call
//! through the gate → execute → finish pipeline; concurrent steps gate
//! every call up front, launch the permitted calls together, and finish
//! them in call order. See the parent module docs for the ordering and
//! safety guarantees.

use serde_json::Value;

use std::sync::Arc;

use crate::config::permissions::PermissionPolicy;
use crate::error::{NornError, ToolError};
use crate::internal::extraction::SharedProvider;
use crate::r#loop::assembly::{AssembledResponse, AssembledToolCall};
use crate::r#loop::config::ToolExecutor;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::request::Message;
use crate::provider::traits::Provider;
use crate::rules::types::RuleInjection;
use crate::tool::envelope::split_envelope_fields;
use crate::tool::scheduling::{ExecutionStep, SchedulingPlan, ToolEffect, ToolEffectIndex};

use super::execute_single_tool;
use super::gating::{
    CallEnv, CallPlan, PreparedCall, finish_blocked_call, finish_executed_call, prepare_tool_call,
    run_single_call,
};

/// Argument bundle for [`execute_planned_tool_batch`]. Field-for-field
/// mirror of the runner-side `ToolBatchRequest` so the integration in
/// `helpers.rs::execute_tool_batch` is a single construction + call.
pub(in crate::r#loop) struct PlannedBatchRequest<'a> {
    /// Provider published on the shared context for internal agents.
    pub(in crate::r#loop) provider: Option<Arc<dyn Provider>>,
    /// Tool executor (usually a `ToolRegistry`).
    pub(in crate::r#loop) executor: &'a dyn ToolExecutor,
    /// Session event store results are appended to.
    pub(in crate::r#loop) store: &'a crate::session::store::EventStore,
    /// Local conversation messages results are appended to.
    pub(in crate::r#loop) messages: &'a mut Vec<Message>,
    /// The assembled provider response carrying the tool calls.
    pub(in crate::r#loop) response: &'a AssembledResponse,
    /// Indices into `response.tool_calls` to execute, in call order.
    pub(in crate::r#loop) tool_indices: Vec<usize>,
    /// Loop configuration (schema tool name for rules skipping).
    pub(in crate::r#loop) config: &'a crate::r#loop::config::AgentLoopConfig,
    /// Loop-wide context (hooks, rules, diagnostics, action log).
    pub(in crate::r#loop) loop_context: &'a mut LoopContext,
    /// Optional streaming event channel.
    pub(in crate::r#loop) event_tx: Option<&'a crate::provider::agent_event::AgentEventSender>,
}

/// Execute a batch of tool calls per an effect-based [`SchedulingPlan`].
///
/// Each call's effect is resolved through the executor's
/// [`ToolEffectIndex`] (via `Tool::effect_for_args`); when the executor
/// exposes no index every call is `Unknown` and the batch runs fully
/// serialized, preserving the historical behaviour. Concurrent steps
/// gate every call (permissions + pre-tool hooks) sequentially in call
/// order before launching, run the permitted calls via `join_all`, then
/// append results / record completions / fire post-tool hooks and rule
/// events sequentially in call order. Returns the concatenated rule
/// injections, exactly as a sequential dispatch would.
pub(in crate::r#loop) async fn execute_planned_tool_batch(
    request: PlannedBatchRequest<'_>,
) -> Result<Vec<RuleInjection>, NornError> {
    let PlannedBatchRequest {
        provider,
        executor,
        store,
        messages,
        response,
        tool_indices,
        config,
        loop_context,
        event_tx,
    } = request;

    publish_shared_extensions(provider, executor, loop_context);
    let env = CallEnv {
        executor,
        store,
        config,
        event_tx,
        permissions: permission_policy(executor),
    };

    let index = effect_index(executor);
    let call_effects: Vec<(String, ToolEffect)> = tool_indices
        .iter()
        .map(|&idx| {
            let tc = &response.tool_calls[idx];
            let effect = index.as_deref().map_or(ToolEffect::Unknown, |ix| {
                ix.effect_for(&tc.name, &model_args_for(tc))
            });
            (tc.call_id.clone(), effect)
        })
        .collect();
    let plan = SchedulingPlan::build(&call_effects);

    // The plan preserves original call order, so steps consume
    // `tool_indices` left to right; a cursor avoids any reliance on
    // call-id uniqueness.
    let mut cursor = 0usize;
    let mut injections = Vec::new();
    for step in &plan.steps {
        match step {
            ExecutionStep::Serial { .. } => {
                let idx = tool_indices[cursor];
                cursor += 1;
                injections
                    .extend(run_single_call(&env, messages, response, idx, loop_context).await?);
            }
            ExecutionStep::Concurrent { tool_call_ids } => {
                let count = tool_call_ids.len();
                let indices = &tool_indices[cursor..cursor + count];
                cursor += count;
                injections.extend(
                    run_concurrent_step(&env, messages, response, indices, loop_context).await?,
                );
            }
        }
    }
    Ok(injections)
}

/// Run one concurrent plan step: gate every call in order, launch the
/// permitted calls together, then finish each call in order.
async fn run_concurrent_step(
    env: &CallEnv<'_>,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    indices: &[usize],
    loop_context: &LoopContext,
) -> Result<Vec<RuleInjection>, NornError> {
    // Phase 1 — gating. Permission checks and pre-tool hooks run
    // sequentially in call order before anything launches, so a Block
    // still suppresses exactly its own call. (For serial dispatch the
    // gate for call N runs after call N-1 completed; in a concurrent
    // step all gates run up front — read-only/network tools have no
    // cross-call state for a gate to observe, which is what made the
    // step concurrent-eligible in the first place.)
    let mut prepared: Vec<PreparedCall> = Vec::with_capacity(indices.len());
    for &idx in indices {
        prepared.push(prepare_tool_call(env, response, idx, loop_context).await?);
    }

    // Phase 2 — concurrent execution of the permitted calls. `join_all`
    // preserves input order, which is call order.
    let futures: Vec<_> = prepared
        .iter()
        .filter_map(|p| match &p.plan {
            CallPlan::Execute { args_override } => {
                let tc = &response.tool_calls[p.tc_index];
                let args_str: &str = args_override.as_deref().unwrap_or(&tc.arguments);
                Some(execute_single_tool(
                    env.executor,
                    &tc.name,
                    &tc.call_id,
                    args_str,
                    loop_context.diagnostics.as_ref(),
                ))
            }
            CallPlan::Blocked { .. } => None,
        })
        .collect();
    let mut results = futures_util::future::join_all(futures).await.into_iter();

    // Phase 3 — sequential completion in call order: append results,
    // record on the action log, fire post-tool hooks and rule events.
    let mut injections = Vec::new();
    for p in &prepared {
        let tc = &response.tool_calls[p.tc_index];
        match &p.plan {
            CallPlan::Blocked { output, reason } => {
                finish_blocked_call(env, messages, loop_context, tc, p, output, reason).await?;
            }
            CallPlan::Execute { .. } => {
                let Some(result) = results.next() else {
                    return Err(NornError::Tool(ToolError::ExecutionFailed {
                        reason: format!(
                            "internal scheduling error: missing concurrent result for tool call \
                             '{}'",
                            tc.call_id,
                        ),
                    }));
                };
                injections.extend(
                    finish_executed_call(env, messages, loop_context, tc, p, result).await?,
                );
            }
        }
    }
    Ok(injections)
}

/// Publish the loop's provider and diagnostic collector onto the
/// executor's shared [`ToolContext`](crate::tool::context::ToolContext)
/// so tools can retrieve them via `ctx.get_extension`. Idempotent —
/// re-inserting replaces the same `TypeId` slot.
fn publish_shared_extensions(
    provider: Option<Arc<dyn Provider>>,
    executor: &dyn ToolExecutor,
    loop_context: &LoopContext,
) {
    let Some(shared) = executor.shared_context() else {
        return;
    };
    // Publish the provider used by this agent loop onto the executor's
    // shared ToolContext so tools can invoke lightweight internal agents
    // via `ctx.get_extension::<SharedProvider>()`.
    if let Some(provider) = provider {
        shared.insert_extension(Arc::new(SharedProvider(provider)));
    }
    // Publish the diagnostic collector onto the executor's shared
    // ToolContext (when present) so RuntimePostValidateCheck
    // implementations can retrieve it via
    // `ctx.get_extension::<DiagnosticCollector>()` and push diagnostics
    // directly.
    if let Some(collector) = loop_context.diagnostics.as_ref() {
        shared.insert_extension(Arc::clone(collector));
    }
}

/// Resolve the consent-boundary policy from the executor's shared
/// context, when one was installed by the runtime assembly.
fn permission_policy(executor: &dyn ToolExecutor) -> Option<Arc<PermissionPolicy>> {
    executor
        .shared_context()?
        .get_extension::<PermissionPolicy>()
}

/// Resolve the registry-maintained effect index from the executor's
/// shared context. Absent (e.g. mock executors), every call classifies
/// as [`ToolEffect::Unknown`] and the batch runs fully serialized —
/// the pre-scheduling behaviour.
fn effect_index(executor: &dyn ToolExecutor) -> Option<Arc<ToolEffectIndex>> {
    executor
        .shared_context()?
        .get_extension::<ToolEffectIndex>()
}

/// Parse a tool call's raw argument string and return the model-supplied
/// tool arguments (envelope metadata split off) for effect resolution
/// and permission matching.
fn model_args_for(tc: &AssembledToolCall) -> Value {
    let raw = serde_json::from_str::<Value>(&tc.arguments).unwrap_or(Value::Null);
    split_envelope_fields(raw).tool_args
}
