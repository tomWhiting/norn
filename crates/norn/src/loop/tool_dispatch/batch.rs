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

use tokio_util::task::AbortOnDropHandle;

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
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::{ExecutionStep, SchedulingPlan, ToolEffect};

use super::gating::{
    CallEnv, CallPlan, PreparedCall, finish_blocked_call, finish_executed_call, prepare_tool_call,
    run_single_call,
};
use super::{SingleToolResult, execute_single_tool};

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
/// [`ToolEffectIndex`](crate::tool::scheduling::ToolEffectIndex) (via
/// `Tool::effect_for_args`); when the executor
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

    let index = executor.effect_index();
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

    // Phase 2 — concurrent execution of the permitted calls, in call
    // order. With an owned executor handle each call runs on its own
    // spawned task, so batch members execute in parallel across runtime
    // workers instead of interleaving on one; without one (executors
    // reachable only by borrow) the members share this task via
    // `join_all`. Both paths preserve input order, which is call order.
    let results = if let Some(executor) = env.executor.owned_handle() {
        run_spawned_members(&executor, response, &prepared, loop_context).await
    } else {
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
        futures_util::future::join_all(futures).await
    };
    let mut results = results.into_iter();

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
                        reason: "internal scheduling error: missing concurrent tool result"
                            .to_owned(),
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

/// Run each permitted call of one concurrent step on its own spawned
/// tokio task, awaiting the handles in call order so results come back
/// exactly as the sequential path would order them.
///
/// Handles are wrapped in [`AbortOnDropHandle`], so dropping this future
/// (a `step_timeout` cutting the step mid-batch) aborts the spawned
/// members — matching the single-task path, where the member futures die
/// with the dropped step. A member that panics (or is aborted out from
/// under the await, which cannot happen while this future is polled to
/// completion) surfaces as a structured `execution_failed` tool result
/// rather than tearing down the whole step task: the other members'
/// results are real work that must still be appended and audited.
async fn run_spawned_members(
    executor: &Arc<dyn ToolExecutor>,
    response: &AssembledResponse,
    prepared: &[PreparedCall],
    loop_context: &LoopContext,
) -> Vec<SingleToolResult> {
    let mut handles: Vec<AbortOnDropHandle<SingleToolResult>> = Vec::new();
    for p in prepared {
        let CallPlan::Execute { args_override } = &p.plan else {
            continue;
        };
        let tc = &response.tool_calls[p.tc_index];
        let executor = Arc::clone(executor);
        let name = tc.name.clone();
        let call_id = tc.call_id.clone();
        let args = args_override
            .clone()
            .unwrap_or_else(|| tc.arguments.clone());
        let diagnostics = loop_context.diagnostics.clone();
        handles.push(AbortOnDropHandle::new(tokio::spawn(async move {
            execute_single_tool(
                executor.as_ref(),
                &name,
                &call_id,
                &args,
                diagnostics.as_ref(),
            )
            .await
        })));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(join_error) => {
                tracing::error!(
                    task_cancelled = join_error.is_cancelled(),
                    task_panicked = join_error.is_panic(),
                    "concurrent tool task did not complete; surfacing a structured failure",
                );
                results.push(join_failure_result(&join_error));
            }
        }
    }
    results
}

/// Structured failure result for a spawned member whose task ended
/// without producing a result (panic, or runtime-level abort). The
/// payload rides the same `error`-key wire shape as every other tool
/// failure so downstream phases (action log, post-tool hooks, the
/// model-facing result) treat it uniformly.
fn join_failure_result(join_error: &tokio::task::JoinError) -> SingleToolResult {
    let message = if join_error.is_cancelled() {
        "concurrent tool task was cancelled"
    } else if join_error.is_panic() {
        "concurrent tool task panicked"
    } else {
        "concurrent tool task did not complete"
    };
    let payload = ToolErrorPayload::new(ToolErrorKind::ExecutionFailed, message.to_owned());
    SingleToolResult {
        output: serde_json::json!({ "error": payload.to_value() }),
        error: Some(payload),
        duration_ms: 0,
        follow_ups: Vec::new(),
        post_validate_outcome: None,
    }
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

/// Parse a tool call's raw argument string and return the model-supplied
/// tool arguments (envelope metadata split off) for effect resolution
/// and permission matching.
fn model_args_for(tc: &AssembledToolCall) -> Value {
    let raw = serde_json::from_str::<Value>(&tc.arguments).unwrap_or(Value::Null);
    split_envelope_fields(raw).tool_args
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::error::ToolError as CrateToolError;
    use crate::r#loop::assembly::AssembledToolCall;
    use crate::r#loop::config::AgentLoopConfig;
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;
    use crate::session::store::EventStore;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::registry::ToolRegistry;
    use crate::tool::traits::{Tool, ToolOutput};

    /// Read-only tool that blocks its worker thread for `block` — a
    /// thread-blocking (not `await`-yielding) delay, so wall-clock
    /// overlap is only possible when batch members run on *separate*
    /// spawned tasks across runtime workers. `join_all` on one task
    /// would serialize these exactly.
    struct BlockingReadTool {
        name: &'static str,
        block: Duration,
    }

    #[async_trait]
    impl Tool for BlockingReadTool {
        fn name(&self) -> &'static str {
            self.name
        }
        fn description(&self) -> &'static str {
            "thread-blocking read-only test tool"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, CrateToolError> {
            std::thread::sleep(self.block);
            Ok(ToolOutput::success(json!({ "tool": self.name })))
        }
    }

    /// Read-only tool that panics mid-execution.
    struct PanickingReadTool;

    const PANIC_TOOL_NAME: &str = "tool-name-secret-must-not-escape";
    const PANIC_CALL_ID: &str = "call-id-secret-must-not-escape";

    #[async_trait]
    impl Tool for PanickingReadTool {
        fn name(&self) -> &'static str {
            PANIC_TOOL_NAME
        }
        fn description(&self) -> &'static str {
            "read-only test tool that panics"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, CrateToolError> {
            panic!("deliberate test panic");
        }
    }

    fn read_call(call_id: &str, name: &str) -> AssembledToolCall {
        AssembledToolCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
            kind: crate::provider::request::ToolCallKind::Function,
        }
    }

    fn response_with(calls: Vec<AssembledToolCall>) -> AssembledResponse {
        AssembledResponse {
            reasoning: Vec::new(),
            text: String::new(),
            thinking: String::new(),
            tool_calls: calls,
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: None,
        }
    }

    async fn run_batch(
        executor: &Arc<dyn ToolExecutor>,
        response: &AssembledResponse,
        messages: &mut Vec<Message>,
        store: &EventStore,
    ) {
        let config = AgentLoopConfig::default();
        let mut loop_context = LoopContext::new("base");
        execute_planned_tool_batch(PlannedBatchRequest {
            provider: None,
            executor,
            store,
            messages,
            response,
            tool_indices: (0..response.tool_calls.len()).collect(),
            config: &config,
            loop_context: &mut loop_context,
            event_tx: None,
        })
        .await
        .expect("batch executes");
    }

    /// Two thread-blocking read-only tools scheduled in one concurrent
    /// step must overlap in wall-clock: each member runs on its own
    /// spawned task via the executor's owned handle. Serial execution
    /// would take at least 2 x 250ms.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_step_members_overlap_in_wall_clock() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BlockingReadTool {
            name: "slow_a",
            block: Duration::from_millis(250),
        }));
        registry.register(Box::new(BlockingReadTool {
            name: "slow_b",
            block: Duration::from_millis(250),
        }));
        let executor: Arc<dyn ToolExecutor> = Arc::new(registry);
        let store = EventStore::new();
        let mut messages = Vec::new();
        let response = response_with(vec![
            read_call("call_a", "slow_a"),
            read_call("call_b", "slow_b"),
        ]);

        let started = Instant::now();
        run_batch(&executor, &response, &mut messages, &store).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(450),
            "two 250ms thread-blocking read-only members must overlap, took {elapsed:?}",
        );
        // Results append in call order regardless of completion order.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_b"));
        assert!(
            messages[0]
                .content
                .as_deref()
                .expect("result content")
                .contains("slow_a"),
        );
        assert!(
            messages[1]
                .content
                .as_deref()
                .expect("result content")
                .contains("slow_b"),
        );
    }

    /// A spawned member that panics surfaces as a structured
    /// `execution_failed` tool result; the other member's result is
    /// unaffected and result order still matches call order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawned_member_panic_surfaces_structured_failure() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PanickingReadTool));
        registry.register(Box::new(BlockingReadTool {
            name: "steady",
            block: Duration::from_millis(5),
        }));
        let executor: Arc<dyn ToolExecutor> = Arc::new(registry);
        let store = EventStore::new();
        let mut messages = Vec::new();
        let response = response_with(vec![
            read_call(PANIC_CALL_ID, PANIC_TOOL_NAME),
            read_call("call_ok", "steady"),
        ]);

        run_batch(&executor, &response, &mut messages, &store).await;

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some(PANIC_CALL_ID));
        let failure = messages[0].content.as_deref().expect("failure content");
        assert!(
            failure.contains("error") && failure.contains("task panicked"),
            "panicked member must surface a structured failure: {failure}",
        );
        assert!(!failure.contains(PANIC_TOOL_NAME));
        assert!(!failure.contains(PANIC_CALL_ID));
        assert!(!failure.contains("deliberate test panic"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_ok"));
        assert!(
            messages[1]
                .content
                .as_deref()
                .expect("ok content")
                .contains("steady"),
        );
    }

    /// Without an owned handle (executor reachable only by borrow) the
    /// concurrent step still executes every member on the single-task
    /// fallback and preserves call order.
    #[tokio::test]
    async fn borrowed_executor_falls_back_to_single_task_concurrency() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BlockingReadTool {
            name: "slow_a",
            block: Duration::from_millis(1),
        }));
        registry.register(Box::new(BlockingReadTool {
            name: "slow_b",
            block: Duration::from_millis(1),
        }));
        let store = EventStore::new();
        let mut messages = Vec::new();
        let response = response_with(vec![
            read_call("call_a", "slow_a"),
            read_call("call_b", "slow_b"),
        ]);
        let config = AgentLoopConfig::default();
        let mut loop_context = LoopContext::new("base");
        execute_planned_tool_batch(PlannedBatchRequest {
            provider: None,
            executor: &registry,
            store: &store,
            messages: &mut messages,
            response: &response,
            tool_indices: vec![0, 1],
            config: &config,
            loop_context: &mut loop_context,
            event_tx: None,
        })
        .await
        .expect("fallback batch executes");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_b"));
    }
}
