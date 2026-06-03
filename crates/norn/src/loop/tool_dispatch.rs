//! Tool execution pipeline extracted from `helpers.rs`.
//!
//! Houses the functions that dispatch individual tool calls, build
//! [`ToolEnvelope`]s, and append tool results to the event store and
//! conversation.

use std::time::{Duration, Instant};

use serde_json::Value;

use std::sync::Arc;

use crate::error::{HookType, NornError, ToolError};
use crate::integration::diagnostics::{DiagnosticCollector, NornDiagnostic};
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::internal::extraction::SharedProvider;
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::config::{AgentLoopConfig, DispatchOutcome, ToolExecutor};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::rule_wiring::build_runtime_events;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::ProviderEvent;
use crate::provider::request::{Message, MessageRole};
use crate::provider::traits::Provider;
use crate::rules::types::RuleInjection;
use crate::session::action_log::{CompletionRecord, Outcome};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope, split_envelope_fields};
use crate::tool::follow_up::FollowUpAction;
use crate::tool::traits::ToolOutput;

use super::helpers::append_and_notify;

/// Outcome of a single tool dispatch, carrying the model-facing output plus
/// the metadata the action log records (`duration`, follow-ups, post-validate
/// outcome). On the error path the follow-ups are empty and the post-validate
/// outcome is `None` — the executor returns no metadata when a phase fails.
pub(super) struct SingleToolResult {
    /// Model-facing structured output (errors surfaced under an `error` key).
    pub output: Value,
    /// Wall-clock execution duration in milliseconds.
    pub duration_ms: u64,
    /// Full follow-up actions registered by the tool, for action-log indexing.
    pub follow_ups: Vec<FollowUpAction>,
    /// Post-validate outcome captured for the call, when one was produced.
    pub post_validate_outcome: Option<Value>,
}

/// Execute a single tool and return its [`SingleToolResult`].
///
/// When `diagnostics` is `Some`, pre-validate blocks and post-validate
/// failures are also recorded as [`NornDiagnostic`] values on the
/// collector. The agent loop's control flow is unchanged — the diagnostic
/// push is observational.
pub(super) async fn execute_single_tool(
    executor: &dyn ToolExecutor,
    name: &str,
    call_id: &str,
    arguments_json: &str,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
) -> SingleToolResult {
    let start = Instant::now();

    let args = serde_json::from_str::<Value>(arguments_json)
        .unwrap_or_else(|e| Value::String(format!("invalid JSON arguments: {e}")));

    let (output, follow_ups, post_validate_outcome) = match executor
        .execute_with_outcome(name, call_id, args)
        .await
    {
        Ok(DispatchOutcome {
            content,
            follow_ups,
            post_validate_outcome,
        }) => (content, follow_ups, post_validate_outcome),
        Err(e) => {
            if let Some(collector) = diagnostics
                && matches!(
                    e,
                    ToolError::PreValidationFailed { .. } | ToolError::PostValidationFailed { .. }
                )
            {
                collector.report(NornDiagnostic::from_tool_error(name, &e));
            }
            let output = match &e {
                ToolError::PostValidationFailed {
                    committed_output: Some(Value::Object(map)),
                    ..
                } => {
                    let mut out = map.clone();
                    out.insert("error".to_owned(), Value::String(e.to_string()));
                    Value::Object(out)
                }
                ToolError::PostValidationFailed {
                    committed_output: Some(val),
                    ..
                } => {
                    tracing::warn!("PostValidationFailed committed_output is not a JSON object");
                    serde_json::json!({
                        "error": e.to_string(),
                        "committed_output": val,
                    })
                }
                _ => serde_json::json!({ "error": e.to_string() }),
            };
            (output, Vec::new(), None)
        }
    };

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    SingleToolResult {
        output,
        duration_ms,
        follow_ups,
        post_validate_outcome,
    }
}

/// Execute a single tool call by index.
///
/// Fires `PreToolHook`s (which may block execution), dispatches to the
/// `spoken_response` path when applicable, then fires `PostToolHook`s on the
/// resulting output. After execution the runtime emits `RuntimeEvent`s to the
/// rules engine; injections are returned to the caller to apply once the
/// surrounding tool batch finishes.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_tool_call(
    provider: Option<Arc<dyn Provider>>,
    executor: &dyn ToolExecutor,
    store: &EventStore,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    tc_index: usize,
    config: &AgentLoopConfig,
    loop_context: &mut LoopContext,
    event_tx: Option<&AgentEventSender>,
) -> Result<Vec<RuleInjection>, NornError> {
    let tc = &response.tool_calls[tc_index];
    let (mut envelope, description) = build_envelope(tc);
    let tool_ctx = ToolContext::empty();

    if let Some(shared) = executor.shared_context() {
        // Publish the provider used by this agent loop onto the executor's
        // shared ToolContext so tools can invoke lightweight internal agents
        // via `ctx.get_extension::<SharedProvider>()`.
        if let Some(provider) = provider {
            shared.insert_extension(Arc::new(SharedProvider(provider)));
        }

        // Publish the diagnostic collector onto the executor's shared
        // ToolContext (when present) so RuntimePostValidateCheck
        // implementations can retrieve it via `ctx.get_extension::<DiagnosticCollector>()`
        // and push diagnostics directly.
        if let Some(collector) = loop_context.diagnostics.as_ref() {
            shared.insert_extension(Arc::clone(collector));
        }
    }

    let modified_args_str: Option<String> = if let Some(hooks) = loop_context.hooks.as_deref() {
        match hooks.run_pre_tool(&envelope, &tool_ctx).await {
            HookOutcome::Block { reason } => {
                let blocked_output = serde_json::json!({
                    "error": format!("blocked by hook ({:?}): {reason}", HookType::PreTool),
                });
                append_tool_result(
                    store,
                    messages,
                    &tc.call_id,
                    &tc.name,
                    tc.kind,
                    &blocked_output,
                    0,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;
                record_dispatch_completion(
                    loop_context,
                    &tc.name,
                    &tc.call_id,
                    &description,
                    Outcome::Blocked {
                        reason: reason.clone(),
                    },
                    &blocked_output,
                    envelope.model_args.clone(),
                    0,
                    Vec::new(),
                    None,
                );
                return Ok(Vec::new());
            }
            HookOutcome::Modify { updated_input } => {
                let serialized = serde_json::to_string(&updated_input).map_err(|e| {
                    NornError::Tool(ToolError::ExecutionFailed {
                        reason: format!("failed to serialize hook-modified args: {e}"),
                    })
                })?;
                envelope.model_args = updated_input;
                Some(serialized)
            }
            HookOutcome::Proceed => None,
        }
    } else {
        None
    };

    let args_str: &str = modified_args_str.as_deref().unwrap_or(&tc.arguments);

    let (output, duration_ms) = {
        let SingleToolResult {
            output: out,
            duration_ms: ms,
            follow_ups,
            post_validate_outcome,
        } = execute_single_tool(
            executor,
            &tc.name,
            &tc.call_id,
            args_str,
            loop_context.diagnostics.as_ref(),
        )
        .await;
        append_tool_result(
            store,
            messages,
            &tc.call_id,
            &tc.name,
            tc.kind,
            &out,
            ms,
            loop_context.hooks.as_deref(),
            event_tx,
        )
        .await?;
        // Record after the result event is in the store so detail/context
        // queries find the matching event. The coarse outcome mirrors the
        // existing follow-up-chain convention: an `error` key means failure.
        let outcome = match out.get("error").and_then(|v| v.as_str()) {
            Some(message) => Outcome::Error {
                message: message.to_owned(),
            },
            None => Outcome::Success,
        };
        record_dispatch_completion(
            loop_context,
            &tc.name,
            &tc.call_id,
            &description,
            outcome,
            &out,
            envelope.model_args.clone(),
            ms,
            follow_ups,
            post_validate_outcome,
        );
        (out, ms)
    };

    if let Some(hooks) = loop_context.hooks.as_deref() {
        let post_output = ToolOutput {
            content: output.clone(),
            is_error: output.get("error").is_some(),
            duration: Duration::from_millis(duration_ms),
        };
        hooks
            .run_post_tool(&envelope, &post_output, &tool_ctx)
            .await;
        // NH-006 R7 / C59: PostToolFailureHook fires in addition to the
        // existing PostToolHook, but only when the tool output indicates
        // an error (matches the same `is_error` test the post-tool block
        // uses for parity). Observational — no control flow effect.
        if post_output.is_error {
            hooks
                .run_post_tool_failure(&envelope, &post_output, &tool_ctx)
                .await;
        }
    }

    let mut injections = Vec::new();
    if loop_context.rules.is_some() && tc.name != config.schema_tool_name {
        let events = build_runtime_events(&tc.name, &tc.arguments);
        if let Some(engine) = loop_context.rules.as_ref() {
            for ev in &events {
                injections.extend(engine.process_event(ev).await);
            }
        }
    }

    Ok(injections)
}

/// Construct a [`ToolEnvelope`] for an in-flight tool call so hook bodies see
/// a stable, normalized representation independent of the streaming format.
///
/// Returns the envelope alongside the model-supplied `tool_use_description`
/// (empty when the model provided none) so the dispatch path can record it on
/// the action log without re-splitting the raw arguments.
fn build_envelope(tc: &crate::r#loop::assembly::AssembledToolCall) -> (ToolEnvelope, String) {
    let raw = serde_json::from_str::<Value>(&tc.arguments).unwrap_or(Value::Null);
    let split = split_envelope_fields(raw);
    let envelope = ToolEnvelope {
        tool_call_id: tc.call_id.clone(),
        tool_name: tc.name.clone(),
        model_args: split.tool_args,
        runtime_inputs: RuntimeInputs::default(),
        metadata: split.metadata,
    };
    (envelope, split.description.unwrap_or_default())
}

/// Record one completed tool dispatch on the loop's action log, when present.
///
/// `level_1_only` is set for the `action_log` tool's own dispatches so a query
/// of the log does not store its (potentially large) query result (CO4). When
/// no action log is wired, this is a no-op.
#[allow(clippy::too_many_arguments)]
fn record_dispatch_completion(
    loop_context: &LoopContext,
    tool_name: &str,
    tool_call_id: &str,
    tool_use_description: &str,
    outcome: Outcome,
    output: &Value,
    args: Value,
    duration_ms: u64,
    follow_ups: Vec<FollowUpAction>,
    post_validate_outcome: Option<Value>,
) {
    let Some(action_log) = loop_context.action_log.as_ref() else {
        return;
    };
    action_log.record_completion(CompletionRecord {
        tool_name,
        tool_call_id,
        tool_use_description,
        outcome,
        output,
        args,
        duration_ms,
        follow_ups,
        post_validate_outcome,
        level_1_only: tool_name == "action_log",
    });
}

/// Append a tool result to both the event store and the local messages vec,
/// and broadcast it on the streaming channel if available.
///
/// `kind` records whether the originating call was a `function_call` or a
/// freeform `custom_tool_call` so the eventual request serializer picks the
/// matching output envelope. Callers that have no kind available pass
/// [`ToolCallKind::Function`] — the legacy behaviour and the wire default.
#[allow(clippy::too_many_arguments)]
pub(super) async fn append_tool_result(
    store: &EventStore,
    messages: &mut Vec<Message>,
    tool_call_id: &str,
    tool_name: &str,
    kind: crate::provider::request::ToolCallKind,
    output: &Value,
    duration_ms: u64,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<(), crate::error::SessionError> {
    let parent = store.last_event_id();
    append_and_notify(
        store,
        SessionEvent::ToolResult {
            base: EventBase::new(parent),
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            output: output.clone(),
            duration_ms,
        },
        hooks,
    )
    .await?;

    if let Some(sender) = event_tx {
        sender.send(ProviderEvent::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            output: output.clone(),
            duration_ms,
        });
    }

    let content = serde_json::to_string(output).ok();

    messages.push(Message {
        role: MessageRole::ToolResult,
        content,
        thinking: String::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call_id.to_string()),
        tool_name: Some(tool_name.to_string()),
        tool_call_kind: Some(kind),
    });

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::integration::hooks::{Hook, HookRegistry, PreToolHook};
    use crate::r#loop::assembly::{AssembledResponse, AssembledToolCall};
    use crate::r#loop::config::{MockToolExecutor, ToolHandler};
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;
    use crate::session::action_log::ActionLog;

    fn tool_call(call_id: &str, name: &str, arguments: &str) -> AssembledToolCall {
        AssembledToolCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
            kind: crate::provider::request::ToolCallKind::Function,
        }
    }

    fn response_with(call: AssembledToolCall) -> AssembledResponse {
        AssembledResponse {
            text: String::new(),
            thinking: String::new(),
            tool_calls: vec![call],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }
    }

    /// Build a loop context that records into `action_log`, sharing `store`.
    fn loop_with_log(action_log: &Arc<ActionLog>) -> LoopContext {
        let mut loop_context = LoopContext::new("base");
        loop_context.action_log = Some(Arc::clone(action_log));
        loop_context
    }

    async fn dispatch(
        executor: &dyn ToolExecutor,
        store: &EventStore,
        loop_context: &mut LoopContext,
        response: &AssembledResponse,
    ) {
        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        execute_tool_call(
            None,
            executor,
            store,
            &mut messages,
            response,
            0,
            &config,
            loop_context,
            None,
        )
        .await
        .expect("dispatch returns Ok");
    }

    #[tokio::test]
    async fn dispatch_records_success_completion() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "read".to_owned(),
            Box::new(|_args| Ok(json!({ "path": "a.rs", "lines": 3 }))),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call(
            "tc-1",
            "read",
            r#"{"path":"a.rs","tool_use_description":"reading"}"#,
        ));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let entries = action_log.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_call_id, "tc-1");
        assert_eq!(entries[0].tool_name, "read");
        assert_eq!(entries[0].tool_use_description, "reading");
        assert!(matches!(entries[0].outcome, Outcome::Success));

        let detail = action_log.get_detail("tc-1").expect("detail recorded");
        assert_eq!(detail.output["path"], "a.rs");
        // Recorded args are the envelope tool args (description stripped out).
        assert_eq!(detail.args["path"], "a.rs");
        assert!(detail.args.get("tool_use_description").is_none());
    }

    #[tokio::test]
    async fn dispatch_records_error_completion() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "edit".to_owned(),
            Box::new(|_args| {
                Err(ToolError::ExecutionFailed {
                    reason: "boom".to_owned(),
                })
            }),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call("tc-e", "edit", r#"{"path":"a.rs"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let entries = action_log.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].outcome, Outcome::Error { .. }));
        let detail = action_log.get_detail("tc-e").expect("detail recorded");
        assert!(detail.output.get("error").is_some());
    }

    struct BlockEverything {
        reason: String,
    }

    #[async_trait::async_trait]
    impl PreToolHook for BlockEverything {
        async fn before_tool(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> crate::integration::hooks::HookOutcome {
            crate::integration::hooks::HookOutcome::Block {
                reason: self.reason.clone(),
            }
        }
    }

    #[tokio::test]
    async fn dispatch_records_hook_blocked_completion() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut registry = HookRegistry::new();
        registry.register(Hook::PreTool(Box::new(BlockEverything {
            reason: "policy".to_owned(),
        })));
        loop_context.hooks = Some(Arc::new(registry));

        let executor = MockToolExecutor::empty();
        let response = response_with(tool_call("tc-b", "bash", r#"{"cmd":"ls"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let entries = action_log.entries();
        assert_eq!(entries.len(), 1);
        match &entries[0].outcome {
            Outcome::Blocked { reason } => assert_eq!(reason, "policy"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_action_log_self_call_is_level_1_only() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "action_log".to_owned(),
            Box::new(|_args| Ok(json!({ "query": "list", "entries": [1, 2, 3] }))),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call("tc-self", "action_log", r#"{"query":"list"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        // Level 1 entry is retained.
        let entry = action_log.entry("tc-self").expect("level 1 entry recorded");
        assert_eq!(entry.tool_name, "action_log");

        // Level 2/3 payloads are dropped because the self-call is level_1_only.
        let detail = action_log.get_detail("tc-self").expect("detail present");
        assert_eq!(detail.output, Value::Null);
        assert_eq!(detail.args, Value::Null);
        assert_eq!(detail.duration_ms, 0);
    }

    #[tokio::test]
    async fn dispatch_without_action_log_is_a_noop() {
        // No action log wired: dispatch still succeeds and records nothing.
        let store = EventStore::new();
        let mut loop_context = LoopContext::new("base");
        assert!(loop_context.action_log.is_none());

        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "read".to_owned(),
            Box::new(|_args| Ok(json!({ "path": "a.rs" }))),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call("tc-n", "read", r#"{"path":"a.rs"}"#));
        dispatch(&executor, &store, &mut loop_context, &response).await;
        assert!(loop_context.action_log.is_none());
    }
}
