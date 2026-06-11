//! Tool execution pipeline extracted from `helpers.rs`.
//!
//! Houses the functions that dispatch individual tool calls, build
//! [`ToolEnvelope`]s, and append tool results to the event store and
//! conversation. The pipeline phases live in submodules: [`gating`]
//! (permission + pre-tool-hook gate, completion recording) and
//! [`batch`] (effect-scheduled batch execution).
//!
//! # Consent boundary (H16)
//!
//! Before any pre-tool hook runs, each call is evaluated against the
//! [`PermissionPolicy`](crate::config::permissions::PermissionPolicy)
//! published on the executor's shared
//! [`ToolContext`](crate::tool::context::ToolContext) (when one is
//! installed). A `deny` match blocks the call with a structured error
//! the model sees; an `ask` match is routed through the registered
//! `PreToolHook` chain when one exists (a `Block` refuses, anything else
//! is consent) and otherwise blocks with a "requires consent; no
//! interactive handler" error. Permission rules evaluate the
//! model-supplied arguments; hook-modified arguments are not
//! re-evaluated (hooks are trusted orchestrator code).
//!
//! # Effect-based scheduling
//!
//! [`execute_planned_tool_batch`] orders a batch through
//! [`SchedulingPlan`](crate::tool::scheduling::SchedulingPlan): adjacent
//! `ReadOnly` / `Network` calls run concurrently, while `Write` /
//! `Process` / `Unknown` calls run alone. `Process` stays serialized
//! because bash mutates the shared working directory (`cd` parsing) and
//! `Write` because file mutations may conflict; only effects with no
//! cross-call state are parallelised. Within a concurrent step, gating
//! (permissions + pre-tool hooks) runs sequentially in call order
//! *before* any call launches, and results are appended / recorded /
//! post-hooked sequentially in call order *after* every call in the
//! step finishes — so tool-result ordering in the conversation and on
//! the event channel always matches call order.

mod batch;
mod gating;

pub(super) use batch::{PlannedBatchRequest, execute_planned_tool_batch};

use std::time::Instant;

use serde_json::Value;

use std::sync::Arc;

use crate::error::ToolError;
use crate::integration::diagnostics::{DiagnosticCollector, NornDiagnostic};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::AssembledToolCall;
use crate::r#loop::config::{DispatchOutcome, ToolExecutor};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::ProviderEvent;
use crate::provider::request::{Message, MessageRole};
use crate::session::action_log::CompletionRecord;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope, split_envelope_fields};
use crate::tool::follow_up::FollowUpAction;

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

/// Construct a [`ToolEnvelope`] for an in-flight tool call so hook bodies see
/// a stable, normalized representation independent of the streaming format.
///
/// Returns the envelope alongside the model-supplied `tool_use_description`
/// (empty when the model provided none) so the dispatch path can record it on
/// the action log without re-splitting the raw arguments.
fn build_envelope(tc: &AssembledToolCall) -> (ToolEnvelope, String) {
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
/// `record.level_1_only` is derived here — set for the `action_log` tool's
/// own dispatches so a query of the log does not store its (potentially
/// large) query result (CO4); whatever the caller supplied is overwritten.
/// When no action log is wired, this is a no-op.
fn record_dispatch_completion(loop_context: &LoopContext, mut record: CompletionRecord<'_>) {
    let Some(action_log) = loop_context.action_log.as_ref() else {
        return;
    };
    record.level_1_only = record.tool_name == "action_log";
    action_log.record_completion(record);
}

/// Identity and payload of one tool result to append via
/// [`append_tool_result`].
///
/// `kind` records whether the originating call was a `function_call` or a
/// freeform `custom_tool_call` so the eventual request serializer picks the
/// matching output envelope. Callers that have no kind available pass
/// [`ToolCallKind::Function`](crate::provider::request::ToolCallKind::Function)
/// — the legacy behaviour and the wire default.
pub(super) struct ToolResultRecord<'a> {
    /// Provider-assigned id of the tool call this result answers.
    pub(super) tool_call_id: &'a str,
    /// Name of the tool that produced the result.
    pub(super) tool_name: &'a str,
    /// Wire form of the originating call (function vs custom).
    pub(super) kind: crate::provider::request::ToolCallKind,
    /// Structured tool output.
    pub(super) output: &'a Value,
    /// Wall-clock execution duration in milliseconds.
    pub(super) duration_ms: u64,
}

/// Append a tool result to both the event store and the local messages vec,
/// and broadcast it on the streaming channel if available.
pub(super) async fn append_tool_result(
    store: &EventStore,
    messages: &mut Vec<Message>,
    record: ToolResultRecord<'_>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<(), crate::error::SessionError> {
    let ToolResultRecord {
        tool_call_id,
        tool_name,
        kind,
        output,
        duration_ms,
    } = record;
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::config::permissions::PermissionPolicy;
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
    use crate::r#loop::assembly::{AssembledResponse, AssembledToolCall};
    use crate::r#loop::config::AgentLoopConfig;
    use crate::r#loop::config::{MockToolExecutor, ToolHandler};
    use crate::provider::events::StopReason;
    use crate::provider::usage::Usage;
    use crate::session::action_log::{ActionLog, Outcome};
    use crate::tool::context::ToolContext;
    use crate::tool::registry::ToolRegistry;
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::{Tool, ToolOutput};

    fn tool_call(call_id: &str, name: &str, arguments: &str) -> AssembledToolCall {
        AssembledToolCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
            kind: crate::provider::request::ToolCallKind::Function,
        }
    }

    fn response_with(call: AssembledToolCall) -> AssembledResponse {
        response_with_calls(vec![call])
    }

    fn response_with_calls(calls: Vec<AssembledToolCall>) -> AssembledResponse {
        AssembledResponse {
            text: String::new(),
            thinking: String::new(),
            tool_calls: calls,
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
        execute_planned_tool_batch(PlannedBatchRequest {
            provider: None,
            executor,
            store,
            messages: &mut messages,
            response,
            tool_indices: vec![0],
            config: &config,
            loop_context,
            event_tx: None,
        })
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

    // -- H16: permission consent boundary ----------------------------------

    /// Instrumented registry tool: records executions, optionally waits
    /// on a barrier (concurrency proof) or sleeps, and logs start/end
    /// markers for ordering assertions.
    struct ProbeTool {
        tool_name: String,
        tool_effect: ToolEffect,
        executed: Arc<AtomicBool>,
        barrier: Option<Arc<tokio::sync::Barrier>>,
        sleep: Option<Duration>,
        log: Option<Arc<parking_lot::Mutex<Vec<String>>>>,
    }

    impl ProbeTool {
        fn new(name: &str, effect: ToolEffect) -> Self {
            Self {
                tool_name: name.to_owned(),
                tool_effect: effect,
                executed: Arc::new(AtomicBool::new(false)),
                barrier: None,
                sleep: None,
                log: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &'static str {
            "probe"
        }
        fn input_schema(&self) -> Value {
            json!({})
        }
        fn effect(&self) -> ToolEffect {
            self.tool_effect
        }
        async fn execute(
            &self,
            envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            self.executed.store(true, Ordering::SeqCst);
            if let Some(log) = self.log.as_ref() {
                log.lock().push(format!("start:{}", envelope.tool_call_id));
            }
            if let Some(barrier) = self.barrier.as_ref() {
                barrier.wait().await;
            }
            if let Some(sleep) = self.sleep {
                tokio::time::sleep(sleep).await;
            }
            if let Some(log) = self.log.as_ref() {
                log.lock().push(format!("end:{}", envelope.tool_call_id));
            }
            Ok(ToolOutput {
                content: json!({ "ok": true, "call": envelope.tool_call_id }),
                is_error: false,
                duration: Duration::ZERO,
            })
        }
    }

    fn install_policy(registry: &ToolRegistry, policy: PermissionPolicy) {
        registry
            .shared_context()
            .expect("registry has shared context")
            .insert_extension(Arc::new(policy));
    }

    #[tokio::test]
    async fn permission_deny_blocks_execution_with_structured_error() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let tool = ProbeTool::new("bash", ToolEffect::Process);
        let executed = Arc::clone(&tool.executed);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-deny", "bash", r#"{"command":"rm -rf /"}"#));
        execute_planned_tool_batch(planned_request(
            &registry,
            store.as_ref(),
            &mut messages,
            &response,
            &config,
            &mut loop_context,
        ))
        .await
        .expect("dispatch returns Ok");

        assert!(!executed.load(Ordering::SeqCst), "tool must not execute");
        let result = messages.last().expect("blocked result appended");
        let error = result.content.as_deref().unwrap_or_default();
        assert!(
            error.contains("denied by permissions.deny rule 'bash(rm *)'"),
            "model-facing error must name the deny rule: {error}",
        );
        let entries = action_log.entries();
        assert!(matches!(entries[0].outcome, Outcome::Blocked { .. }));
    }

    #[tokio::test]
    async fn permission_deny_overrides_allow() {
        let store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::new("base");

        let tool = ProbeTool::new("bash", ToolEffect::Process);
        let executed = Arc::clone(&tool.executed);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&["bash"], &[], &["bash"]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-da", "bash", r#"{"command":"ls"}"#));
        execute_planned_tool_batch(planned_request(
            &registry,
            store.as_ref(),
            &mut messages,
            &response,
            &config,
            &mut loop_context,
        ))
        .await
        .expect("dispatch returns Ok");

        assert!(!executed.load(Ordering::SeqCst), "deny must win over allow");
        let error = messages.last().unwrap().content.as_deref().unwrap();
        assert!(error.contains("denied by permissions.deny rule 'bash'"));
    }

    #[tokio::test]
    async fn permission_ask_without_handler_blocks_with_consent_error() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);
        assert!(loop_context.hooks.is_none(), "no pre-tool hook registered");

        let tool = ProbeTool::new("write", ToolEffect::Write);
        let executed = Arc::clone(&tool.executed);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&[], &["write"], &[]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-ask", "write", r#"{"path":"a.rs"}"#));
        execute_planned_tool_batch(planned_request(
            &registry,
            store.as_ref(),
            &mut messages,
            &response,
            &config,
            &mut loop_context,
        ))
        .await
        .expect("dispatch returns Ok");

        assert!(!executed.load(Ordering::SeqCst));
        let error = messages.last().unwrap().content.as_deref().unwrap();
        assert!(
            error.contains("requires consent; no interactive handler"),
            "ask without a handler must block with the documented error: {error}",
        );
        match &action_log.entries()[0].outcome {
            Outcome::Blocked { reason } => {
                assert!(reason.contains("permissions.ask rule 'write'"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    /// Pre-tool hook that proceeds — stands in for a consent handler.
    struct ProceedHook;

    #[async_trait::async_trait]
    impl PreToolHook for ProceedHook {
        async fn before_tool(&self, _envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            HookOutcome::Proceed
        }
    }

    #[tokio::test]
    async fn permission_ask_with_pre_tool_hook_delegates_consent() {
        let store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::new("base");
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::PreTool(Box::new(ProceedHook)));
        loop_context.hooks = Some(Arc::new(hook_registry));

        let tool = ProbeTool::new("write", ToolEffect::Write);
        let executed = Arc::clone(&tool.executed);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&[], &["write"], &[]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-consent", "write", r#"{"path":"a.rs"}"#));
        execute_planned_tool_batch(planned_request(
            &registry,
            store.as_ref(),
            &mut messages,
            &response,
            &config,
            &mut loop_context,
        ))
        .await
        .expect("dispatch returns Ok");

        assert!(
            executed.load(Ordering::SeqCst),
            "a Proceed from the pre-tool hook chain is consent for ask",
        );
    }

    // -- Effect-based scheduling (SchedulingPlan wiring) --------------------

    fn planned_request<'a>(
        registry: &'a ToolRegistry,
        store: &'a EventStore,
        messages: &'a mut Vec<Message>,
        response: &'a AssembledResponse,
        config: &'a AgentLoopConfig,
        loop_context: &'a mut LoopContext,
    ) -> PlannedBatchRequest<'a> {
        PlannedBatchRequest {
            provider: None,
            executor: registry,
            store,
            messages,
            response,
            tool_indices: (0..response.tool_calls.len()).collect(),
            config,
            loop_context,
            event_tx: None,
        }
    }

    #[tokio::test]
    async fn read_only_batch_executes_concurrently_with_ordered_results() {
        // Three ReadOnly calls rendezvous on a 3-party barrier inside
        // execute(): the test only completes if all three run at the
        // same time. Serial execution deadlocks and trips the timeout.
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut tool = ProbeTool::new("read", ToolEffect::ReadOnly);
        tool.barrier = Some(Arc::clone(&barrier));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));

        let store = EventStore::new();
        let mut loop_context = LoopContext::new("base");
        let config = AgentLoopConfig::default();
        let response = response_with_calls(vec![
            tool_call("tc-0", "read", r#"{"path":"a"}"#),
            tool_call("tc-1", "read", r#"{"path":"b"}"#),
            tool_call("tc-2", "read", r#"{"path":"c"}"#),
        ]);
        let mut messages = Vec::new();

        let injections = tokio::time::timeout(
            Duration::from_secs(10),
            execute_planned_tool_batch(planned_request(
                &registry,
                &store,
                &mut messages,
                &response,
                &config,
                &mut loop_context,
            )),
        )
        .await
        .expect("concurrent batch must not deadlock (serial execution would)")
        .expect("batch returns Ok");
        assert!(injections.is_empty());

        // Results land in call order regardless of completion order.
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["tc-0", "tc-1", "tc-2"]);
    }

    #[tokio::test]
    async fn write_call_serializes_against_reads() {
        // Batch: [read, read, write, read]. The two leading reads prove
        // concurrency via a 2-party barrier; the write and trailing read
        // must each run alone, strictly after the preceding step
        // finished — asserted via the start/end log.
        let log = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let mut read_tool = ProbeTool::new("read", ToolEffect::ReadOnly);
        read_tool.barrier = Some(Arc::clone(&barrier));
        read_tool.log = Some(Arc::clone(&log));

        let mut write_tool = ProbeTool::new("write", ToolEffect::Write);
        write_tool.log = Some(Arc::clone(&log));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(read_tool));
        registry.register(Box::new(write_tool));

        let store = EventStore::new();
        let mut loop_context = LoopContext::new("base");
        let config = AgentLoopConfig::default();
        let response = response_with_calls(vec![
            tool_call("r1", "read", r#"{"path":"a"}"#),
            tool_call("r2", "read", r#"{"path":"b"}"#),
            tool_call("w1", "write", r#"{"path":"c"}"#),
            tool_call("r3", "read", r#"{"path":"d"}"#),
        ]);
        let mut messages = Vec::new();

        // The trailing read is its own concurrent step of size 1, so the
        // 2-party barrier is only crossed by r1/r2; r3 must not block on
        // it — give it a fresh single-party barrier by reusing the same
        // tool is wrong, so instead the barrier counts: r1+r2 cross it,
        // and r3 would hang forever. Use a 2-party barrier and a
        // separate read tool name for the trailing call.
        let mut solo_read = ProbeTool::new("read_solo", ToolEffect::ReadOnly);
        solo_read.log = Some(Arc::clone(&log));
        registry.register(Box::new(solo_read));
        let mut response = response;
        response.tool_calls[3] = tool_call("r3", "read_solo", r#"{"path":"d"}"#);

        tokio::time::timeout(
            Duration::from_secs(10),
            execute_planned_tool_batch(planned_request(
                &registry,
                &store,
                &mut messages,
                &response,
                &config,
                &mut loop_context,
            )),
        )
        .await
        .expect("batch must not deadlock")
        .expect("batch returns Ok");

        // Result order matches call order.
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["r1", "r2", "w1", "r3"]);

        // The write started only after both reads ended, and the
        // trailing read started only after the write ended.
        let events = log.lock().clone();
        let pos = |needle: &str| {
            events
                .iter()
                .position(|e| e == needle)
                .unwrap_or_else(|| panic!("missing log event {needle}: {events:?}"))
        };
        assert!(
            pos("start:w1") > pos("end:r1"),
            "write after r1: {events:?}"
        );
        assert!(
            pos("start:w1") > pos("end:r2"),
            "write after r2: {events:?}"
        );
        assert!(
            pos("start:r3") > pos("end:w1"),
            "trailing read after write: {events:?}",
        );
    }

    /// Pre-tool hook that blocks exactly one tool name.
    struct BlockNamed {
        name: String,
    }

    #[async_trait::async_trait]
    impl PreToolHook for BlockNamed {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == self.name {
                HookOutcome::Block {
                    reason: "named-block".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    #[tokio::test]
    async fn blocking_pre_hook_blocks_only_its_call_in_concurrent_batch() {
        let mut loop_context = LoopContext::new("base");
        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::PreTool(Box::new(BlockNamed {
            name: "guarded".to_owned(),
        })));
        loop_context.hooks = Some(Arc::new(hook_registry));

        // Two-party barrier across the two permitted reads: blocked call
        // must not participate, or the batch hangs.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut read_tool = ProbeTool::new("read", ToolEffect::ReadOnly);
        read_tool.barrier = Some(Arc::clone(&barrier));
        let guarded = ProbeTool::new("guarded", ToolEffect::ReadOnly);
        let guarded_executed = Arc::clone(&guarded.executed);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(read_tool));
        registry.register(Box::new(guarded));

        let store = EventStore::new();
        let config = AgentLoopConfig::default();
        let response = response_with_calls(vec![
            tool_call("tc-a", "read", r#"{"path":"a"}"#),
            tool_call("tc-g", "guarded", r#"{"path":"g"}"#),
            tool_call("tc-b", "read", r#"{"path":"b"}"#),
        ]);
        let mut messages = Vec::new();

        tokio::time::timeout(
            Duration::from_secs(10),
            execute_planned_tool_batch(planned_request(
                &registry,
                &store,
                &mut messages,
                &response,
                &config,
                &mut loop_context,
            )),
        )
        .await
        .expect("batch must not deadlock")
        .expect("batch returns Ok");

        assert!(
            !guarded_executed.load(Ordering::SeqCst),
            "blocked call must not execute",
        );
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["tc-a", "tc-g", "tc-b"], "order preserved");
        let blocked = messages[1].content.as_deref().unwrap();
        assert!(
            blocked.contains("blocked by hook"),
            "blocked call carries the hook error: {blocked}",
        );
        let ok_a = messages[0].content.as_deref().unwrap();
        assert!(ok_a.contains("\"ok\":true"), "permitted call ran: {ok_a}");
    }

    #[tokio::test]
    async fn mock_executor_without_effect_index_runs_serialized() {
        // MockToolExecutor exposes no shared context, so every call is
        // Unknown-effect and the batch must serialize — and still work.
        let store = EventStore::new();
        let mut loop_context = LoopContext::new("base");
        let config = AgentLoopConfig::default();

        let counter = Arc::new(AtomicUsize::new(0));
        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        let c = Arc::clone(&counter);
        handlers.insert(
            "read".to_owned(),
            Box::new(move |_args| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "ok": true }))
            }),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with_calls(vec![
            tool_call("m-0", "read", "{}"),
            tool_call("m-1", "read", "{}"),
        ]);
        let mut messages = Vec::new();
        execute_planned_tool_batch(PlannedBatchRequest {
            provider: None,
            executor: &executor,
            store: &store,
            messages: &mut messages,
            response: &response,
            tool_indices: vec![0, 1],
            config: &config,
            loop_context: &mut loop_context,
            event_tx: None,
        })
        .await
        .expect("serialized batch returns Ok");

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["m-0", "m-1"]);
    }
}
