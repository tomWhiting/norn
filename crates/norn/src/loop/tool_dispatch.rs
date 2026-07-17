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
use crate::tool::envelope::{ToolEnvelope, split_envelope_fields};
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::FollowUpAction;
use crate::tool::output_budget::{
    MODEL_OUTPUT_INLINE_CHAR_LIMIT, ToolOutputBudget, project_model_output,
};

use super::helpers::append_and_notify;

/// Outcome of a single tool dispatch, carrying the model-facing output plus
/// the metadata the action log records (`duration`, follow-ups, post-validate
/// outcome). On the error path the follow-ups are empty and the post-validate
/// outcome is `None` — the executor returns no metadata when a phase fails.
pub(super) struct SingleToolResult {
    /// Model-facing structured output (errors surfaced under an `error` key).
    pub output: Value,
    /// Typed failure payload when the call failed — the structured form of
    /// `output["error"]`, identical for hard [`ToolError`]s and soft
    /// [`ToolOutput::failure`](crate::tool::traits::ToolOutput::failure)
    /// results — so downstream phases never re-parse the content. `None`
    /// for successful calls.
    pub error: Option<ToolErrorPayload>,
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

    let (output, error, follow_ups, post_validate_outcome) =
        match executor.execute_with_outcome(name, call_id, args).await {
            Ok(DispatchOutcome {
                content,
                follow_ups,
                post_validate_outcome,
            }) => {
                // Soft failures (ToolOutput::failure family) carry their typed
                // payload under the content's `error` key; re-type it once here
                // so downstream phases dispatch on the payload, not the JSON.
                let error = content
                    .get("error")
                    .and_then(ToolErrorPayload::from_error_value);
                (content, error, follow_ups, post_validate_outcome)
            }
            Err(e) => {
                if let Some(collector) = diagnostics
                    && matches!(
                        e,
                        ToolError::PreValidationFailed { .. }
                            | ToolError::PostValidationFailed { .. }
                    )
                {
                    collector.report(NornDiagnostic::from_tool_error(name, &e));
                }
                let (output, payload) = hard_error_output(&e);
                (output, Some(payload), Vec::new(), None)
            }
        };

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    SingleToolResult {
        output,
        error,
        duration_ms,
        follow_ups,
        post_validate_outcome,
    }
}

/// Render a hard [`ToolError`] into the model-facing output value and its
/// typed payload. The payload is embedded verbatim under the `error` key —
/// the same wire shape soft failures use — so hard errors are equally
/// machine-readable in the persisted `ToolResult` event.
///
/// `PostValidationFailed` with committed output keeps the committed fields
/// at the top level (the model needs to see what was written) and embeds an
/// `error` payload without the committed copy, so the event does not carry
/// the output twice.
fn hard_error_output(e: &ToolError) -> (Value, ToolErrorPayload) {
    match e {
        ToolError::PostValidationFailed {
            reason,
            committed_output: Some(Value::Object(map)),
        } => {
            let payload = ToolErrorPayload::new(ToolErrorKind::ValidationFailed, reason.clone());
            let mut out = map.clone();
            out.insert("error".to_owned(), payload.to_value());
            (Value::Object(out), payload)
        }
        ToolError::PostValidationFailed {
            reason,
            committed_output: Some(val),
        } => {
            tracing::warn!("PostValidationFailed committed_output is not a JSON object");
            let payload = ToolErrorPayload::new(ToolErrorKind::ValidationFailed, reason.clone());
            let output = serde_json::json!({
                "error": payload.to_value(),
                "committed_output": val,
            });
            (output, payload)
        }
        _ => {
            let payload = ToolErrorPayload::from(e);
            let output = serde_json::json!({ "error": payload.to_value() });
            (output, payload)
        }
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

/// Resolve the effective model-facing inline character limit for tool
/// results dispatched through `executor`.
///
/// The embedder-installed [`ToolOutputBudget`] on the executor's shared
/// [`ToolContext`](crate::tool::context::ToolContext) is authoritative;
/// without one (executors assembled outside `runtime_init`, mock
/// executors) the documented default [`MODEL_OUTPUT_INLINE_CHAR_LIMIT`]
/// applies.
pub(super) fn installed_inline_char_limit(executor: &dyn ToolExecutor) -> usize {
    executor
        .shared_context()
        .and_then(|ctx| ctx.get_extension::<ToolOutputBudget>())
        .map_or(MODEL_OUTPUT_INLINE_CHAR_LIMIT, |budget| {
            budget.model_output_inline_char_limit
        })
}

/// Run a spool write with the file I/O kept off the async executor,
/// mirroring [`append_off_executor`](super::helpers::append_off_executor):
/// on a multi-thread runtime the blocking write runs under
/// `block_in_place`; elsewhere (current-thread runtime, no runtime) it
/// runs inline, exactly like the sink writes on those flavors.
fn spool_write_off_executor(
    spool: &crate::session::spool::SpoolWriter,
    event_id: &crate::session::events::EventId,
    output: &Value,
) -> Result<String, crate::session::persistence::SessionPersistError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| spool.write(event_id, output))
        }
        _ => spool.write(event_id, output),
    }
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
    /// Exact provider caller field to echo on the output.
    pub(super) caller: crate::provider::request::ToolCallCaller,
    /// Structured tool output.
    pub(super) output: &'a Value,
    /// Wall-clock execution duration in milliseconds.
    pub(super) duration_ms: u64,
    /// Effective model-facing inline character limit for this result —
    /// [`installed_inline_char_limit`] of the dispatching executor, so an
    /// embedder-installed [`ToolOutputBudget`] governs the persisted
    /// model-facing projection.
    pub(super) inline_char_limit: usize,
}

/// Append a tool result to both the event store and the local messages vec,
/// and broadcast it on the streaming channel if available.
///
/// The persisted event carries the bounded model-facing projection of
/// `record.output`; when the projection replaced an over-budget payload
/// and the store has a spool attached, the **full** output is first
/// written verbatim to the session's `spool/` directory and the event
/// records the durable spool reference (session-fidelity Gap 5). The
/// spool write precedes the event append — the same
/// write-through-before-visibility ordering as the primary log — so a
/// durable event never claims a spool payload that was not written
/// through. A spool write failure is a typed error and the event is not
/// appended (retrying constructs a fresh event, so the orphaned attempt
/// file, if any, is never referenced).
///
/// Returns the bounded projection it persisted, so callers that also
/// need the model-facing copy (the action-log record, post-tool hooks)
/// reuse it instead of re-serializing a possibly multi-megabyte output
/// a second time — the projection is computed exactly once per result.
pub(super) async fn append_tool_result(
    store: &EventStore,
    messages: &mut Vec<Message>,
    record: ToolResultRecord<'_>,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&AgentEventSender>,
) -> Result<Value, crate::error::SessionError> {
    let ToolResultRecord {
        tool_call_id,
        tool_name,
        kind,
        caller,
        output: full_output,
        duration_ms,
        inline_char_limit,
    } = record;
    let projection = project_model_output(tool_name, tool_call_id, full_output, inline_char_limit);
    let output = projection.value;
    let parent = store.last_event_id();
    let base = EventBase::new(parent);
    let spool_ref = if projection.truncated {
        match store.spool() {
            Some(spool) => Some(
                spool_write_off_executor(spool, &base.id, full_output).map_err(|error| {
                    crate::error::SessionError::StorageError {
                        reason: format!("failed to spool full tool output: {error}"),
                    }
                })?,
            ),
            // No spool attached: sink-less stores have no session
            // directory to spool into, and an embedder-built sink
            // without an attached spool made that configuration choice
            // explicitly (see `EventStore::attach_spool`). The event
            // honestly records no reference rather than a dangling one.
            None => None,
        }
    } else {
        None
    };
    append_and_notify(
        store,
        SessionEvent::ToolResult {
            base,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            output: output.clone(),
            spool_ref,
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

    // `Display` for `Value` is infallible (serde_json Values cannot hold
    // non-string keys or non-finite numbers), so the model-facing echo
    // can never silently degrade to a missing content field.
    let content = Some(output.to_string());

    messages.push(Message {
        response_items: Vec::new(),
        role: MessageRole::ToolResult,
        content,
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call_id.to_string()),
        tool_name: Some(tool_name.to_string()),
        tool_call_kind: Some(kind),
        tool_call_caller: caller,
    });

    Ok(output)
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
            caller: crate::provider::request::ToolCallCaller::Absent,
        }
    }

    fn response_with(call: AssembledToolCall) -> AssembledResponse {
        response_with_calls(vec![call])
    }

    fn response_with_calls(calls: Vec<AssembledToolCall>) -> AssembledResponse {
        AssembledResponse {
            response_items: Vec::new(),
            refusal: None,
            text: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: calls,
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
            response_audio: None,
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

    #[tokio::test]
    async fn dispatch_caps_oversized_tool_result_before_persisting() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "read".to_owned(),
            Box::new(|_args| {
                Ok(json!({
                    "path": "huge.log",
                    "content": "x".repeat(
                        crate::tool::output_budget::MODEL_OUTPUT_INLINE_CHAR_LIMIT + 1
                    ),
                    "follow_ups": [{ "action": "next" }],
                }))
            }),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call("tc-big", "read", r#"{"path":"huge.log"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let output = persisted_tool_result(store.as_ref(), "read");
        assert_eq!(output["truncated_for_model"], true);
        assert_eq!(output["path"], "huge.log");
        assert_eq!(output["follow_ups"][0]["action"], "next");

        let detail = action_log.get_detail("tc-big").expect("detail recorded");
        assert_eq!(detail.output["truncated_for_model"], true);
    }

    /// A mock executor that publishes a shared [`ToolContext`], so tests
    /// can install extensions (e.g. a [`ToolOutputBudget`]) the dispatch
    /// pipeline resolves through [`installed_inline_char_limit`].
    struct SharedContextExecutor {
        inner: MockToolExecutor,
        ctx: Arc<ToolContext>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for SharedContextExecutor {
        async fn execute(
            &self,
            name: &str,
            call_id: &str,
            arguments: Value,
        ) -> Result<Value, ToolError> {
            self.inner.execute(name, call_id, arguments).await
        }

        fn shared_context(&self) -> Option<Arc<ToolContext>> {
            Some(Arc::clone(&self.ctx))
        }
    }

    /// An embedder-installed [`ToolOutputBudget`] with a small
    /// `model_output_inline_char_limit` actually caps: an output far
    /// below the documented 64k default is truncated at the installed
    /// limit, and the persisted record names that limit.
    #[tokio::test]
    async fn installed_budget_inline_limit_caps_tool_result() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let small_limit = 200;
        let oversized = "x".repeat(small_limit + 1);
        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        let payload = oversized.clone();
        handlers.insert(
            "read".to_owned(),
            Box::new(move |_args| Ok(json!({ "content": payload.clone() }))),
        );
        let ctx = Arc::new(ToolContext::empty());
        ctx.insert_extension(Arc::new(ToolOutputBudget {
            model_output_inline_char_limit: small_limit,
            ..ToolOutputBudget::default()
        }));
        let executor = SharedContextExecutor {
            inner: MockToolExecutor::new(handlers),
            ctx,
        };

        let response = response_with(tool_call("tc-budget", "read", r#"{"path":"a.log"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let output = persisted_tool_result(store.as_ref(), "read");
        assert_eq!(
            output["truncated_for_model"], true,
            "the installed budget's limit must cap the result: {output}",
        );
        assert_eq!(output["inline_char_limit"], small_limit);
        assert_eq!(output["original_chars"], json!(small_limit + 1 + 14));
    }

    /// Without an installed budget the documented default applies: the
    /// same sub-64k output persists uncapped.
    #[tokio::test]
    async fn default_inline_limit_applies_without_installed_budget() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let content = "x".repeat(201);
        let payload = content.clone();
        let mut handlers: HashMap<String, ToolHandler> = HashMap::new();
        handlers.insert(
            "read".to_owned(),
            Box::new(move |_args| Ok(json!({ "content": payload.clone() }))),
        );
        let executor = MockToolExecutor::new(handlers);

        let response = response_with(tool_call("tc-default", "read", r#"{"path":"a.log"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let output = persisted_tool_result(store.as_ref(), "read");
        assert_eq!(
            output,
            json!({ "content": content }),
            "under the 64k default a small output must persist unchanged",
        );
    }

    /// The persisted `ToolResult` output for `tool_name`, from the store.
    fn persisted_tool_result(store: &EventStore, tool_name: &str) -> Value {
        store
            .events()
            .iter()
            .find_map(|e| match e {
                SessionEvent::ToolResult {
                    tool_name: name,
                    output,
                    ..
                } if name == tool_name => Some(output.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no ToolResult event for {tool_name}"))
    }

    /// A hard `ToolError` must reach the persisted `ToolResult` event as a
    /// structured `error` object — kind, message — not a collapsed string,
    /// and the action log must still classify the call as an error.
    #[tokio::test]
    async fn hard_tool_error_persists_structured_error_in_event() {
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

        let response = response_with(tool_call("tc-hard", "edit", r#"{"path":"a.rs"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let output = persisted_tool_result(&store, "edit");
        assert_eq!(
            output["error"],
            json!({ "kind": "execution_failed", "message": "boom" }),
            "hard ToolError must persist as the typed payload object",
        );
        let reparsed = crate::tool::failure::ToolErrorPayload::from_error_value(&output["error"])
            .expect("persisted error value re-types");
        assert_eq!(
            reparsed.kind,
            crate::tool::failure::ToolErrorKind::ExecutionFailed
        );
        assert_eq!(reparsed.message, "boom");

        match &action_log.entries()[0].outcome {
            Outcome::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error outcome, got {other:?}"),
        }
    }

    /// Registry tool whose pre-validate blocks with a fully structured
    /// decision (kind + guidance + detail).
    struct GuidedBlockTool;

    #[async_trait::async_trait]
    impl Tool for GuidedBlockTool {
        fn name(&self) -> &'static str {
            "guarded_edit"
        }
        fn description(&self) -> &'static str {
            "blocks with guidance"
        }
        fn input_schema(&self) -> Value {
            json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::Write
        }
        async fn pre_validate(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &crate::tool::context::ToolContext,
        ) -> crate::tool::lifecycle::PreValidateOutcome {
            crate::tool::lifecycle::PreValidateOutcome::Block(
                crate::tool::lifecycle::BlockDecision::new("file has not been read")
                    .with_kind(crate::tool::failure::ToolErrorKind::PermissionDenied)
                    .with_guidance("read the file first")
                    .with_detail(json!({ "path": "a.rs" })),
            )
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &crate::tool::context::ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            panic!("execute must not run for a blocked call");
        }
    }

    /// A `PreValidationFailed` block round-trips end-to-end: the kind,
    /// message, guidance, and detail set by the tool's `BlockDecision`
    /// survive registry dispatch into the persisted `ToolResult` event, and
    /// the action log's error message keeps the model-facing
    /// message-plus-guidance rendering.
    #[tokio::test]
    async fn pre_validation_block_round_trips_kind_and_guidance_into_event() {
        let store = Arc::new(EventStore::new());
        let action_log = Arc::new(ActionLog::new(Arc::clone(&store)));
        let mut loop_context = loop_with_log(&action_log);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(GuidedBlockTool));

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-guided", "guarded_edit", r"{}"));
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

        let output = persisted_tool_result(&store, "guarded_edit");
        let error = &output["error"];
        assert_eq!(error["kind"], "permission_denied");
        assert_eq!(error["message"], "file has not been read");
        assert_eq!(error["detail"]["guidance"], "read the file first");
        assert_eq!(error["detail"]["path"], "a.rs");

        let reparsed = crate::tool::failure::ToolErrorPayload::from_error_value(error)
            .expect("persisted block error re-types");
        assert_eq!(
            reparsed.kind,
            crate::tool::failure::ToolErrorKind::PermissionDenied
        );
        assert_eq!(reparsed.guidance(), Some("read the file first"));

        match &action_log.entries()[0].outcome {
            Outcome::Error { message } => assert_eq!(
                message, "file has not been read Guidance: read the file first",
                "action log keeps the model-facing message+guidance rendering",
            ),
            other => panic!("expected Error outcome, got {other:?}"),
        }
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
            Ok(ToolOutput::success(
                json!({ "ok": true, "call": envelope.tool_call_id }),
            ))
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

    /// A permission denial must persist as a typed `permission_denied`
    /// payload in the `ToolResult` event — embedders dispatching on
    /// `error.kind` must never see a permission block retyped as
    /// `execution_failed` (the legacy collapsed-string behaviour).
    #[tokio::test]
    async fn permission_block_persists_permission_denied_kind_in_event() {
        let store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::new("base");

        let tool = ProbeTool::new("bash", ToolEffect::Process);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-deny-k", "bash", r#"{"command":"rm -rf /"}"#));
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

        let output = persisted_tool_result(&store, "bash");
        let error = &output["error"];
        assert_eq!(
            error["kind"], "permission_denied",
            "permission blocks must persist the typed kind: {output}",
        );
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("denied by permissions.deny rule 'bash(rm *)'"),
            "message must name the deny rule: {error}",
        );
        assert_eq!(
            error["detail"]["rule"], "bash(rm *)",
            "the matched rule must be machine-readable in detail: {error}",
        );
        let reparsed = crate::tool::failure::ToolErrorPayload::from_error_value(error)
            .expect("persisted permission error re-types");
        assert_eq!(
            reparsed.kind,
            crate::tool::failure::ToolErrorKind::PermissionDenied,
            "from_error_value must classify the persisted error as permission_denied",
        );
    }

    /// An ask-rule block without a consent handler is also a permission
    /// denial and must carry the same typed kind plus the matched rule.
    #[tokio::test]
    async fn permission_ask_block_persists_permission_denied_kind_in_event() {
        let store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::new("base");

        let tool = ProbeTool::new("write", ToolEffect::Write);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));
        install_policy(
            &registry,
            PermissionPolicy::from_patterns(&[], &["write"], &[]),
        );

        let mut messages = Vec::new();
        let config = AgentLoopConfig::default();
        let response = response_with(tool_call("tc-ask-k", "write", r#"{"path":"a.rs"}"#));
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

        let error = &persisted_tool_result(&store, "write")["error"];
        assert_eq!(error["kind"], "permission_denied", "{error}");
        assert_eq!(error["detail"]["rule"], "write", "{error}");
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("requires consent; no interactive handler"),
            "{error}",
        );
    }

    /// A pre-tool hook block must persist as a typed `blocked` payload in
    /// the `ToolResult` event, carrying the hook identity and the hook's
    /// stated reason in detail — never the legacy collapsed string that
    /// `from_error_value` retypes as `execution_failed`.
    #[tokio::test]
    async fn hook_block_persists_blocked_kind_in_event() {
        let store = Arc::new(EventStore::new());
        let mut loop_context = LoopContext::new("base");

        let mut hook_registry = HookRegistry::new();
        hook_registry.register(Hook::PreTool(Box::new(BlockEverything {
            reason: "policy says no".to_owned(),
        })));
        loop_context.hooks = Some(Arc::new(hook_registry));

        let executor = MockToolExecutor::empty();
        let response = response_with(tool_call("tc-hb", "bash", r#"{"cmd":"ls"}"#));
        dispatch(&executor, store.as_ref(), &mut loop_context, &response).await;

        let error = &persisted_tool_result(&store, "bash")["error"];
        assert_eq!(
            error["kind"], "blocked",
            "hook blocks must persist the typed kind: {error}",
        );
        assert_eq!(
            error["detail"]["hook"], "pre_tool",
            "the blocking hook must be machine-readable in detail: {error}",
        );
        assert_eq!(
            error["detail"]["reason"], "policy says no",
            "the hook's stated reason must be machine-readable in detail: {error}",
        );
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("policy says no"),
            "the model-facing message must carry the hook's reason: {error}",
        );
        let reparsed = crate::tool::failure::ToolErrorPayload::from_error_value(error)
            .expect("persisted hook-block error re-types");
        assert_eq!(
            reparsed.kind,
            crate::tool::failure::ToolErrorKind::Blocked,
            "from_error_value must classify the persisted error as blocked",
        );
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

    /// Regression (`serde_json::to_string(&output).ok()` swallowed the
    /// Result): the tool-result echo the model sees is rendered
    /// infallibly, so the message content is always present and
    /// round-trips the capped output exactly.
    #[tokio::test]
    async fn tool_result_message_content_is_always_present() {
        let store = EventStore::new();
        let mut messages = Vec::new();
        let output = json!({ "nested": { "value": 42, "text": "café \u{1F980}" } });

        append_tool_result(
            &store,
            &mut messages,
            ToolResultRecord {
                tool_call_id: "tc-content",
                tool_name: "read",
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
                output: &output,
                duration_ms: 3,
                inline_char_limit: MODEL_OUTPUT_INLINE_CHAR_LIMIT,
            },
            None,
            None,
        )
        .await
        .expect("append succeeds");

        assert_eq!(messages.len(), 1);
        let content = messages[0]
            .content
            .as_deref()
            .expect("tool-result content is infallible by construction");
        let parsed: Value = serde_json::from_str(content).expect("content is valid JSON");
        assert_eq!(parsed, output, "the echoed content round-trips the output");
    }

    // -- Gap 5: full-output spool (session-fidelity inventory) ---------

    use crate::session::manager::{CreateSessionOptions, SessionManager};
    use crate::session::persistence::SessionPersistError;
    use crate::session::spool::SpoolWriter;
    use crate::session::store::{DurabilityPolicy, PersistenceSink};

    /// Open a fresh manager-backed session in `data_dir`, returning its
    /// ID and sink-plus-spool-equipped store.
    fn open_spooled_session(data_dir: &std::path::Path) -> (String, EventStore) {
        let manager = SessionManager::new(data_dir);
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/tmp".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .expect("create session");
        (opened.entry.id, opened.store)
    }

    /// An over-budget payload noticeably larger than the head/tail
    /// previews, so the bounded projection is provably not the full
    /// output.
    fn oversized_output() -> Value {
        json!({ "stdout": "x".repeat(50_000) })
    }

    async fn append_oversized(
        store: &EventStore,
        messages: &mut Vec<Message>,
        inline_char_limit: usize,
        tool_call_id: &str,
        tool_name: &str,
    ) -> Result<Value, crate::error::SessionError> {
        append_tool_result(
            store,
            messages,
            ToolResultRecord {
                tool_call_id,
                tool_name,
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
                output: &oversized_output(),
                duration_ms: 1,
                inline_char_limit,
            },
            None,
            None,
        )
        .await
    }

    /// End-to-end spool round-trip on a manager-opened session: the full
    /// oversized output lands verbatim (no cap, no compression) in the
    /// ruled layout's `<data-dir>/<session-id>/spool/`, the persisted
    /// event carries the bounded projection plus a resolvable reference,
    /// the model-facing message echoes only the capped form, and a
    /// process-restart-shaped resume still resolves the reference to the
    /// full output — the forensics path.
    #[tokio::test]
    async fn oversized_tool_result_spools_full_output_verbatim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (session_id, store) = open_spooled_session(tmp.path());
        let full = oversized_output();
        let mut messages = Vec::new();

        append_oversized(&store, &mut messages, 1_000, "tc-spool", "bash")
            .await
            .expect("append succeeds");

        // Persisted event: capped projection + spool reference.
        let events = store.events();
        assert_eq!(events.len(), 1);
        let SessionEvent::ToolResult {
            base,
            output,
            spool_ref,
            ..
        } = &events[0]
        else {
            panic!("expected ToolResult, got {:?}", events[0]);
        };
        assert_eq!(output["truncated_for_model"], true);
        let spool_ref = spool_ref
            .as_deref()
            .expect("over-budget output must carry a spool reference");
        assert_eq!(spool_ref, format!("{session_id}/spool/{}.bin", base.id));

        // Verbatim bytes on disk under the ruled layout — the exact
        // serialized output, uncompressed and uncapped.
        let path = crate::session::spool::resolve_spool_ref(tmp.path(), spool_ref)
            .expect("reference resolves");
        assert_eq!(
            std::fs::read(&path).expect("spool file readable"),
            serde_json::to_vec(&full).expect("serialize"),
            "spool bytes must be the verbatim serialized output",
        );

        // The model-facing echo is the bounded projection, not the full
        // payload.
        let content = messages[0].content.as_deref().expect("content present");
        assert!(content.contains("truncated_for_model"));
        assert!(
            content.len() < serde_json::to_string(&full).expect("serialize").len(),
            "the prompt-facing copy must be smaller than the full output",
        );

        // Forensics after a restart: resume replays the event from disk
        // and its reference still resolves to the full output.
        drop(store);
        let resumed = SessionManager::new(tmp.path())
            .resume(&session_id, DurabilityPolicy::Flush)
            .expect("resume");
        let events = resumed.store.events();
        let SessionEvent::ToolResult {
            spool_ref: Some(replayed_ref),
            ..
        } = &events[0]
        else {
            panic!("replayed event must keep its spool reference");
        };
        assert_eq!(
            crate::session::spool::read_spooled_output(tmp.path(), replayed_ref)
                .expect("forensics path resolves the full output"),
            full,
        );
    }

    /// Within-budget outputs spool nothing: the event carries the full
    /// output inline with no reference, and no spool directory is
    /// created.
    #[tokio::test]
    async fn within_budget_tool_result_records_no_spool_reference() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (session_id, store) = open_spooled_session(tmp.path());
        let output = json!({ "ok": true });
        let mut messages = Vec::new();

        append_tool_result(
            &store,
            &mut messages,
            ToolResultRecord {
                tool_call_id: "tc-small",
                tool_name: "read",
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
                output: &output,
                duration_ms: 1,
                inline_char_limit: MODEL_OUTPUT_INLINE_CHAR_LIMIT,
            },
            None,
            None,
        )
        .await
        .expect("append succeeds");

        let events = store.events();
        let SessionEvent::ToolResult {
            output: persisted,
            spool_ref,
            ..
        } = &events[0]
        else {
            panic!("expected ToolResult");
        };
        assert_eq!(persisted, &output, "within-budget output persists inline");
        assert!(spool_ref.is_none());
        assert!(
            !tmp.path().join(&session_id).exists(),
            "no spool directory is created for within-budget outputs",
        );
    }

    /// A store with no spool attached (sink-less, or an embedder-built
    /// sink that made that configuration choice) still appends the capped
    /// projection and honestly records no reference — never a dangling
    /// one.
    #[tokio::test]
    async fn store_without_spool_appends_capped_projection_without_reference() {
        let store = EventStore::new();
        let mut messages = Vec::new();

        append_oversized(&store, &mut messages, 1_000, "tc-spool", "bash")
            .await
            .expect("append succeeds");

        let events = store.events();
        let SessionEvent::ToolResult {
            output, spool_ref, ..
        } = &events[0]
        else {
            panic!("expected ToolResult");
        };
        assert_eq!(output["truncated_for_model"], true);
        assert!(
            spool_ref.is_none(),
            "a spool-less store must not claim a reference",
        );
    }

    /// Durability ordering, direction one: the spool write-through
    /// precedes the event append, so a spool failure is a typed error and
    /// NO event is appended — a durable event can never reference a spool
    /// payload that was not written through.
    #[tokio::test]
    async fn spool_write_failure_is_typed_and_appends_no_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (session_id, store) = open_spooled_session(tmp.path());
        // Occupy the session's sibling-directory path with a regular FILE
        // so the spool directory cannot be created underneath it.
        std::fs::write(tmp.path().join(&session_id), b"not a directory")?;
        let mut messages = Vec::new();

        let err = append_oversized(
            &store,
            &mut messages,
            1_000,
            "call-id-secret-must-not-escape",
            "tool-name-secret-must-not-escape",
        )
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("spool write unexpectedly succeeded"))?;
        assert!(
            matches!(err, crate::error::SessionError::StorageError { .. }),
            "expected typed StorageError, got {err:?}",
        );
        let rendered = err.to_string();
        assert!(!rendered.contains("call-id-secret-must-not-escape"));
        assert!(!rendered.contains("tool-name-secret-must-not-escape"));
        assert!(
            store.is_empty(),
            "no event may be appended when its spool payload was not written through",
        );
        assert!(messages.is_empty(), "no model-facing echo either");
        Ok(())
    }

    /// Durability ordering, direction two: a sink failure AFTER the spool
    /// write-through (the crash window between the two steps) leaves an
    /// unreferenced orphan spool file — never a durable event with a
    /// dangling reference.
    #[tokio::test]
    async fn sink_failure_after_spool_write_leaves_orphan_never_dangling()
    -> Result<(), Box<dyn std::error::Error>> {
        struct FailingSink;
        impl PersistenceSink for FailingSink {
            fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
                Err(SessionPersistError::Io(std::io::Error::other("disk full")))
            }
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = crate::session::SessionManager::new(tmp.path());
        let opened = manager.create_with_id(
            "sess-ordering",
            crate::session::CreateSessionOptions {
                model: "test-model".to_owned(),
                working_dir: "/work".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        let mut store = EventStore::with_sink(Box::new(FailingSink));
        store.attach_spool(SpoolWriter::for_session(
            tmp.path(),
            &opened.entry,
            DurabilityPolicy::Flush,
            None,
        ));
        let mut messages = Vec::new();

        let err = append_oversized(&store, &mut messages, 1_000, "tc-spool", "bash")
            .await
            .expect_err("sink failure must surface");
        assert!(
            matches!(err, crate::error::SessionError::StorageError { .. }),
            "expected typed StorageError, got {err:?}",
        );
        assert!(store.is_empty(), "the failed event never reaches memory");

        // The spool payload was already written through — exactly the
        // artifact the write ordering permits in the crash window: an
        // orphan file no durable event references.
        let spool_dir = tmp.path().join("sess-ordering").join("spool");
        let orphans: Vec<_> = std::fs::read_dir(&spool_dir)
            .expect("spool dir exists: the write preceded the append")
            .collect::<Result<_, _>>()
            .expect("readable");
        assert_eq!(orphans.len(), 1, "exactly the one written-through payload");
        assert_eq!(
            std::fs::read(orphans[0].path()).expect("orphan readable"),
            serde_json::to_vec(&oversized_output()).expect("serialize"),
            "the orphan holds the verbatim full output",
        );
        Ok(())
    }
}
