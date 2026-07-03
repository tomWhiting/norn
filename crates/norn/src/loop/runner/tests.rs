//! Integration-style tests for the agent step runner.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::error::{NornError, ProviderError};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::{Message, MessageRole, ProviderRequest, ToolDefinition};
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

// -- Helpers ----------------------------------------------------------

struct DelayedProvider {
    responses: Mutex<Vec<Vec<ProviderEvent>>>,
    delay: Duration,
}

impl DelayedProvider {
    fn new(responses: Vec<Vec<ProviderEvent>>, delay: Duration) -> Self {
        Self {
            responses: Mutex::new(responses),
            delay,
        }
    }
}

impl Provider for DelayedProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let mut responses = self
            .responses
            .lock()
            .map_err(|error| ProviderError::StreamError {
                reason: format!("delayed provider lock poisoned: {error}"),
                transient: None,
            })?;
        if responses.is_empty() {
            return Err(ProviderError::StreamError {
                reason: "delayed provider exhausted".to_owned(),
                transient: None,
            });
        }
        let events = responses.remove(0);
        let delay = self.delay;
        let event_stream = stream::unfold(
            (events.into_iter(), true),
            move |(mut iter, first)| async move {
                let event = iter.next()?;
                if first {
                    tokio::time::sleep(delay).await;
                }
                Some((Ok(event), (iter, false)))
            },
        );
        Ok(Box::pin(event_stream))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

fn done_event(reason: StopReason) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: reason,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        response_id: None,
    }
}

fn text_delta(text: &str) -> ProviderEvent {
    ProviderEvent::TextDelta {
        text: text.to_string(),
    }
}

fn thinking_delta(text: &str) -> ProviderEvent {
    ProviderEvent::ThinkingDelta {
        text: text.to_string(),
    }
}

fn tool_call_delta(item_id: &str, name: Option<&str>, args: &str) -> ProviderEvent {
    ProviderEvent::ToolCallDelta {
        item_id: item_id.to_string(),
        call_id: None,
        name: name.map(String::from),
        arguments_delta: args.to_string(),
        kind: crate::provider::request::ToolCallKind::Function,
    }
}

fn simple_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"]
    })
}

fn default_config() -> AgentLoopConfig {
    AgentLoopConfig::default()
}

fn config_with_budget(budget: u32) -> AgentLoopConfig {
    AgentLoopConfig {
        schema_attempt_budget: budget,
        ..AgentLoopConfig::default()
    }
}

fn read_file_handlers() -> std::collections::HashMap<String, ToolHandler> {
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "file data"}))),
    );
    handlers
}

fn read_file_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: "read_file".to_string(),
        description: "Read a file".to_string(),
        parameters: serde_json::json!({}),
    }
}

/// Extract output and usage from a Completed result, or fail the test.
#[track_caller]
fn assert_completed(result: AgentStepResult) -> (Value, Usage) {
    let AgentStepResult::Completed { output, usage, .. } = result else {
        let msg = format!("expected Completed, got {result:?}");
        // assert! with a non-const expression is needed here
        assert!(msg.is_empty(), "{msg}");
        return (Value::Null, Usage::default());
    };
    (output, usage)
}

/// Extract fields from a `SchemaUnreachable` result, or fail the test.
#[track_caller]
fn assert_schema_unreachable(result: AgentStepResult) -> (Option<Value>, Vec<String>, u32, Usage) {
    let AgentStepResult::SchemaUnreachable {
        best_attempt,
        validation_errors,
        attempts,
        usage,
        ..
    } = result
    else {
        let msg = format!("expected SchemaUnreachable, got {result:?}");
        assert!(msg.is_empty(), "{msg}");
        return (None, Vec::new(), 0, Usage::default());
    };
    (best_attempt, validation_errors, attempts, usage)
}

/// Bundled inputs for the `run_step*` helpers, keeping each helper
/// within the workspace argument-count lint budget.
struct StepArgs<'a> {
    provider: &'a dyn Provider,
    executor: &'a MockToolExecutor,
    store: &'a EventStore,
    tools: &'a [ToolDefinition],
    schema: Option<&'a Value>,
    config: &'a AgentLoopConfig,
    event_tx: Option<&'a AgentEventSender>,
    inbound: Option<&'a mut crate::r#loop::inbound::InboundChannel>,
}

async fn run_step(
    provider: &MockProvider,
    executor: &MockToolExecutor,
    store: &EventStore,
    tools: &[ToolDefinition],
    schema: Option<&Value>,
    config: &AgentLoopConfig,
    event_tx: Option<&AgentEventSender>,
) -> AgentStepResult {
    let mut loop_ctx = LoopContext::new("system");
    run_step_with(
        StepArgs {
            provider,
            executor,
            store,
            tools,
            schema,
            config,
            event_tx,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await
}

async fn run_step_full(
    args: StepArgs<'_>,
    event_schemas: Option<&crate::r#loop::event_schemas::EventSchemaSet>,
) -> AgentStepResult {
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.event_schemas = event_schemas.cloned();
    run_step_with(args, &mut loop_ctx).await
}

async fn run_step_with(args: StepArgs<'_>, loop_ctx: &mut LoopContext) -> AgentStepResult {
    let result = run_agent_step(AgentStepRequest {
        provider: args.provider,
        executor: args.executor,
        store: args.store,
        user_prompt: "prompt",
        tools: args.tools,
        output_schema: args.schema,
        model: "test-model",
        config: args.config,
        event_tx: args.event_tx,
        inbound: args.inbound,
        loop_context: loop_ctx,
        cancel: None,
    })
    .await;
    assert!(result.is_ok(), "run_agent_step failed: {:?}", result.err());
    result
        .ok()
        .unwrap_or(AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
            children_usage: Usage::default(),
        })
}

// -- Test 1: Two-turn tool interaction (R2) ---------------------------

#[tokio::test]
async fn two_turn_tool_interaction() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"foo.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"42"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "hello"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, usage) = assert_completed(result);
    assert_eq!(output["answer"], "42");
    assert!(usage.input_tokens > 0);
    assert!(store.len() >= 4);
}

// -- Encrypted reasoning threads into the next iteration's request --

#[tokio::test]
async fn reasoning_items_threaded_into_next_request_messages() {
    // Seam regression: a reasoning output item captured on a tool-call
    // turn must ride the in-memory assistant Message so the next
    // provider request can replay it (stateless threading). The mock
    // provider records every request it receives; the second request's
    // assistant message must carry the captured item.
    let reasoning_item = crate::provider::reasoning::ReasoningItem {
        id: "rs_1".to_owned(),
        summary: vec![
            crate::provider::reasoning::ReasoningSummaryPart::SummaryText {
                text: "planning the tool call".to_owned(),
            },
        ],
        content: None,
        encrypted_content: Some("opaque-blob".to_owned()),
    };
    let turn1 = vec![
        ProviderEvent::ReasoningItemDone {
            item: reasoning_item.clone(),
        },
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"foo.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"42"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "hello"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "42");

    let requests = provider.requests().expect("recorded requests");
    assert_eq!(requests.len(), 2, "two provider turns expected");
    let assistant = requests[1]
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Assistant)
        .expect("second request replays the assistant turn");
    assert_eq!(
        assistant.reasoning,
        vec![reasoning_item],
        "captured reasoning must thread into the next request's messages",
    );
}

// -- Thinking is threaded from AssembledResponse into SessionEvent --

#[tokio::test]
async fn thinking_delta_threaded_into_assistant_message() {
    let events = vec![
        thinking_delta("first let me reason"),
        text_delta("The answer is 42."),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &default_config(),
        None,
    )
    .await;

    let (_, _) = assert_completed(result);
    let assistant_msg = store
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::AssistantMessage {
                content, thinking, ..
            } => Some((content.clone(), thinking.clone())),
            _ => None,
        })
        .expect("at least one AssistantMessage");
    assert_eq!(assistant_msg.0, "The answer is 42.");
    assert_eq!(assistant_msg.1, "first let me reason");
}

// -- Test 2: Text-only no-schema -> Completed with Value::String (R10)

#[tokio::test]
async fn text_only_no_schema_completes() {
    let events = vec![
        text_delta("The answer is 42."),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("The answer is 42.".to_string()));
}

// -- Test 3: Schema valid on first try (R4 case 1) --------------------

#[tokio::test]
async fn schema_valid_first_try() {
    let events = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"answer":"correct"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "correct");
}

// -- Test 4: Schema invalid then valid (R4 case 2) --------------------

#[tokio::test]
async fn schema_invalid_then_valid() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"wrong":"field"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"answer":"fixed"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (output, usage) = assert_completed(result);
    assert_eq!(output["answer"], "fixed");
    assert_eq!(usage.input_tokens, 20);
}

// -- Test 5: Text stop then schema after nudge (R4 case 4) ------------

#[tokio::test]
async fn text_stop_then_schema_after_nudge() {
    let turn1 = vec![text_delta("thinking..."), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"answer":"nudged"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "nudged");

    let events = store.events();
    let has_nudge = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.contains("structured_output") && content.contains("schema")
        } else {
            false
        }
    });
    assert!(has_nudge, "nudge message should be in event store");
}

// -- Test 6: 3 text-only stops -> SchemaUnreachable (R7) ---------------

#[tokio::test]
async fn three_text_stops_schema_unreachable() {
    let responses: Vec<Vec<ProviderEvent>> = (0..3)
        .map(|_| {
            vec![
                text_delta("still thinking"),
                done_event(StopReason::EndTurn),
            ]
        })
        .collect();

    let provider = MockProvider::new(responses);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
}

// -- Test 7: 3 invalid schema calls -> SchemaUnreachable (R7) ---------

#[tokio::test]
async fn three_invalid_schema_calls_unreachable() {
    let responses: Vec<Vec<ProviderEvent>> = (0..3)
        .map(|i| {
            vec![
                tool_call_delta(
                    &format!("tc{i}"),
                    Some("structured_output"),
                    r#"{"wrong":"data"}"#,
                ),
                done_event(StopReason::ToolUse),
            ]
        })
        .collect();

    let provider = MockProvider::new(responses);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (best_attempt, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
    assert!(best_attempt.is_some());
}

// -- Test 8: 1 nudge + 2 invalid -> SchemaUnreachable(3) (R7) --------

#[tokio::test]
async fn nudge_plus_two_invalid_unreachable() {
    let turn1 = vec![text_delta("hmm"), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        tool_call_delta("tc1", Some("structured_output"), r#"{"bad":1}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn3 = vec![
        tool_call_delta("tc2", Some("structured_output"), r#"{"also_bad":2}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(3),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 3);
}

// -- Test 9: Budget=1 + text stop -> SchemaUnreachable(1) (R7) --------

#[tokio::test]
async fn budget_one_text_stop_unreachable() {
    let events = vec![text_delta("nope"), done_event(StopReason::EndTurn)];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(1),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 1);
}

// -- Test 10: [read_tool, schema_tool] -> read executes, schema valid (R5)

#[tokio::test]
async fn pre_schema_tools_execute() {
    let events = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let events = store.events();
    let read_result = events.iter().any(
        |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
    );
    assert!(read_result, "read_file tool should have been executed");
}

// -- Test 11: [schema_tool, read_tool] -> read REJECTED (R5) ----------

#[tokio::test]
async fn post_schema_tools_rejected() {
    let events = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "first");

    let events = store.events();
    let read_results: Vec<&SessionEvent> = events
        .iter()
        .filter(
            |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
        )
        .collect();
    assert_eq!(read_results.len(), 1, "should have one read_file result");
    if let SessionEvent::ToolResult { output, .. } = read_results[0] {
        let error_str = output["error"].as_str().unwrap_or("");
        assert!(
            error_str.contains("rejected"),
            "read_file should be rejected, got: {error_str}"
        );
    }

    // REVIEW H1: exactly one result for the schema tool call. The
    // pre-fix code appended an acceptance in BOTH
    // `accept_schema_tool_call` and `reject_post_schema_tools`,
    // producing a duplicate `function_call_output` that poisoned the
    // persisted session and drew a provider 400 on the next request.
    let schema_results: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::ToolResult { tool_name, .. }
                    if tool_name == "structured_output"
            )
        })
        .collect();
    assert_eq!(
        schema_results.len(),
        1,
        "exactly one structured_output result must be persisted",
    );
    if let SessionEvent::ToolResult {
        tool_call_id,
        output,
        ..
    } = schema_results[0]
    {
        assert_eq!(tool_call_id, "tc_schema");
        assert_eq!(output.as_str(), Some("accepted"));
    }
}

// -- REVIEW H1 regression: one persisted result per call_id ------------
//
// [read_file, structured_output, read_file] exercises pre-schema
// execution, schema acceptance, and post-schema rejection in one
// response. Every call_id must have exactly one ToolResult in the
// persisted store — duplicates poison session replay permanently.

#[tokio::test]
async fn schema_flow_persists_exactly_one_result_per_call_id() {
    let events = vec![
        tool_call_delta("tc_pre", Some("read_file"), r#"{"path":"a"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        tool_call_delta("tc_post", Some("read_file"), r#"{"path":"b"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "first");

    let mut results_by_call: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for event in store.events() {
        if let SessionEvent::ToolResult {
            tool_call_id,
            output,
            ..
        } = event
        {
            results_by_call
                .entry(tool_call_id)
                .or_default()
                .push(output);
        }
    }
    for call_id in ["tc_pre", "tc_schema", "tc_post"] {
        let outputs = results_by_call
            .get(call_id)
            .unwrap_or_else(|| panic!("missing result for {call_id}"));
        assert_eq!(
            outputs.len(),
            1,
            "{call_id} must have exactly one persisted result, got {outputs:?}",
        );
    }
    assert_eq!(results_by_call["tc_schema"][0].as_str(), Some("accepted"));
    assert!(
        results_by_call["tc_pre"][0]["content"].is_string(),
        "pre-schema tool must actually execute",
    );
    assert!(
        results_by_call["tc_post"][0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("rejected"),
        "post-schema tool must be rejected, not executed",
    );
}

// -- Test 12: Streaming events forwarded to broadcast channel (R9) ----

#[tokio::test]
async fn streaming_events_forwarded_to_broadcast() {
    use crate::provider::agent_event::{AgentEvent, AgentEventKind, AgentEventSender};
    use uuid::Uuid;

    let events = vec![
        text_delta("hello"),
        text_delta(" world"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    let sender = AgentEventSender::new(tx, Uuid::nil(), "root".to_string());

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &default_config(),
        Some(&sender),
    )
    .await;

    let mut received = Vec::new();
    while let Ok(agent_event) = rx.try_recv() {
        match agent_event.event {
            AgentEventKind::Provider(event) => received.push(event),
            AgentEventKind::UsageEstimate(_) => {}
            AgentEventKind::Subagent(_)
            | AgentEventKind::Message(_)
            | AgentEventKind::StreamRetry(_)
            | AgentEventKind::Compaction(_) => {
                panic!("the loop emits only provider events here")
            }
        }
    }

    assert_eq!(received.len(), 3, "should receive all 3 events");
    assert!(matches!(&received[0], ProviderEvent::TextDelta { text } if text == "hello"));
    assert!(matches!(&received[1], ProviderEvent::TextDelta { text } if text == " world"));
    assert!(matches!(&received[2], ProviderEvent::Done { .. }));
}

// -- Test 13: Nudge contains tool name + schema + instruction (R8) ----

#[tokio::test]
async fn nudge_contains_required_content() {
    let turn1 = vec![text_delta("analyzing"), done_event(StopReason::EndTurn)];
    let turn2 = vec![
        text_delta("still analyzing"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(2),
        None,
    )
    .await;

    let events = store.events();
    let nudge_content = events.iter().find_map(|e| {
        if let SessionEvent::UserMessage { content, .. } = e
            && content.contains("structured_output")
        {
            return Some(content.clone());
        }
        None
    });

    assert!(nudge_content.is_some(), "nudge message should exist");
    let content = nudge_content.unwrap_or_default();
    assert!(
        content.contains("structured_output"),
        "nudge must contain tool name"
    );
    assert!(
        content.contains("answer"),
        "nudge must contain schema field names"
    );
    assert!(
        content.contains("Call the structured_output tool"),
        "nudge must contain instruction"
    );
}

// -- Test 14: No-schema + tool then text (R10) ------------------------

#[tokio::test]
async fn no_schema_tool_then_text() {
    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"bar"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        text_delta("file contained: bar"),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| Ok(serde_json::json!({"content": "bar"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("file contained: bar".to_string()));

    let events = store.events();
    let tool_executed = events.iter().any(
        |e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file"),
    );
    assert!(tool_executed, "read_file should have been executed");
}

// -- Helpers for R5/R6/R7 ----------------------------------------------

fn make_channel_message(
    author: &str,
    content: &str,
    kind: crate::r#loop::inbound::MessageKind,
    offset_secs: i64,
) -> crate::r#loop::inbound::ChannelMessage {
    let base = chrono::Utc::now();
    let timestamp = base + chrono::Duration::milliseconds(offset_secs);
    crate::r#loop::inbound::ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::new_v4(),
        from: author.to_string(),
        role: None,
        to_id: uuid::Uuid::new_v4(),
        content: content.to_string(),
        kind,
        seq: None,
        timestamp,
    }
}

// -- R5/R6/R3-N011 Test: steer message injected at tool boundary ------
//
// R3 (N-011) acceptance: this test exercises the drain-and-inject
// pipeline between two turns of a tool batch — turn 1 has tools, drain
// happens at the tool boundary, the steer message becomes a UserMessage
// event before turn 2's provider call sees it.

#[tokio::test]
async fn steer_message_injected_between_turns() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tx.send(make_channel_message(
        "alice",
        "please use foo.rs",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    ))
    .await
    .expect("send steer");

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let events = store.events();
    let has_steer = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"alice\" ")
                && content.contains("kind=\"steer\"")
                && content.contains("\nplease use foo.rs\n")
        } else {
            false
        }
    });
    assert!(has_steer, "steer message should appear as UserMessage");
}

// -- R6 Test: multiple steer messages in timestamp order --------------

#[tokio::test]
async fn multiple_steer_messages_injected_in_timestamp_order() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    // Send in reverse timestamp order; injection must sort ascending.
    tx.send(make_channel_message(
        "bob",
        "second by time",
        crate::r#loop::inbound::MessageKind::Steer,
        200,
    ))
    .await
    .expect("send 1");
    tx.send(make_channel_message(
        "alice",
        "first by time",
        crate::r#loop::inbound::MessageKind::Steer,
        100,
    ))
    .await
    .expect("send 2");

    let _result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let events = store.events();
    let steer_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if let SessionEvent::UserMessage { content, .. } = e {
                if content.starts_with("<agent_message from=") {
                    Some(i)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(steer_indices.len(), 2, "expected 2 steer messages");

    let first_event = &events[steer_indices[0]];
    let second_event = &events[steer_indices[1]];
    if let (
        SessionEvent::UserMessage { content: c1, .. },
        SessionEvent::UserMessage { content: c2, .. },
    ) = (first_event, second_event)
    {
        assert!(c1.contains("first by time"), "got: {c1}");
        assert!(c2.contains("second by time"), "got: {c2}");
    } else {
        panic!("expected two UserMessage events");
    }
}

// -- R7/R3-N011 Test: schema-mode follow-up triggers continuation -----
//
// R3 (N-011) acceptance: "Follow-up messages injected only when loop
// would return Completed" — this test verifies a FollowUp message
// buffered while the loop is otherwise ready to complete causes the
// loop to continue.

#[tokio::test]
async fn schema_mode_follow_up_triggers_continuation() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"second"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        tx.send(make_channel_message(
            "operator",
            "any more thoughts?",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");
    });

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(
        output["answer"], "second",
        "final output should be from turn 2"
    );

    let events = store.events();
    let has_follow_up = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"operator\" ")
                && content.contains("kind=\"update\"")
                && content.contains("\nany more thoughts?\n")
        } else {
            false
        }
    });
    assert!(has_follow_up, "follow-up message should appear");
}

// -- R7 Test: no-schema-mode follow-up triggers continuation ----------

#[tokio::test]
async fn no_schema_mode_follow_up_triggers_continuation() {
    let turn1 = vec![text_delta("first text"), done_event(StopReason::EndTurn)];
    let turn2 = vec![text_delta("second text"), done_event(StopReason::EndTurn)];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        tx.send(make_channel_message(
            "operator",
            "say more",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");
    });

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("second text".to_string()));

    let events = store.events();
    let has_follow_up = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.starts_with("<agent_message from=\"operator\" ")
                && content.contains("kind=\"update\"")
                && content.contains("\nsay more\n")
        } else {
            false
        }
    });
    assert!(has_follow_up, "follow-up message should appear");
}

// -- R7 Test: no follow-up at stop -> Completed normally --------------

#[tokio::test]
async fn no_follow_up_at_stop_returns_completed_normally() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"only"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (_tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "only");
    assert_eq!(
        provider.call_count(),
        1,
        "exactly one provider call expected when no follow-up"
    );
}

// -- R7 Test: follow-up does NOT consume schema budget ---------------

#[tokio::test]
async fn follow_up_does_not_consume_schema_budget() {
    let turn1 = vec![
        tool_call_delta(
            "tc_schema_1",
            Some("structured_output"),
            r#"{"answer":"first"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"second"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(20));
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        tx.send(make_channel_message(
            "operator",
            "more please",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");
    });

    // Budget = 1: if follow-up consumed budget, the second turn would
    // result in SchemaUnreachable. Successful Completed proves the
    // follow-up did NOT consume budget.
    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &config_with_budget(1),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "second");
}

// -- R7 regression: idle-root inbound reaches first request ----------

/// A message delivered while the root loop is idle between user turns
/// must be injected before the next provider request. Without the
/// pre-request drain this message would only surface at the stop
/// boundary of the second step, wasting a provider turn and delaying
/// the push.
#[tokio::test]
async fn inbound_message_queued_between_steps_reaches_next_request() {
    let provider = MockProvider::new(vec![
        vec![text_delta("first"), done_event(StopReason::EndTurn)],
        vec![text_delta("second"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let mut loop_ctx = LoopContext::new("system");

    let first = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (first_output, _) = assert_completed(first);
    assert_eq!(first_output, Value::String("first".to_string()));

    tx.send(make_channel_message(
        "spawn/worker",
        "idle push",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    ))
    .await
    .expect("send idle message");

    let second = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (second_output, _) = assert_completed(second);
    assert_eq!(second_output, Value::String("second".to_string()));

    let requests = provider.requests().expect("requests");
    assert_eq!(
        requests.len(),
        2,
        "idle inbound should not force an extra provider turn",
    );
    let second_request_text = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("<agent_message from=\"spawn/worker\"")
            && second_request_text.contains("kind=\"update\"")
            && second_request_text.contains("\nidle push\n"),
        "second request must include idle inbound message: {second_request_text}",
    );
}

#[tokio::test]
async fn message_seeded_step_records_delivery_without_empty_prompt() {
    let provider = MockProvider::new(vec![vec![
        text_delta("handled"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    let mut message = make_channel_message(
        "spawn/worker",
        "wake root",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    );
    message.seq = Some(1);
    let message_id = message.id;

    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        initial_messages: vec![message],
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("message-seeded step completes");
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("handled".to_string()));

    let events = store.events();
    let user_messages = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1, "no synthetic empty prompt");
    assert!(user_messages[0].contains("<agent_message from=\"spawn/worker\""));
    assert!(user_messages[0].contains("\nwake root\n"));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Custom {
                event_type,
                data,
                ..
            } if event_type == crate::provider::agent_event::AGENT_MESSAGE_DELIVERED_EVENT_TYPE
                && data["message_id"] == message_id.to_string()
        )
    }));

    let requests = provider.requests().expect("requests");
    assert_eq!(requests.len(), 1);
    let request_text = requests[0]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("<agent_message from=\"spawn/worker\"")
            && request_text.contains("kind=\"steer\"")
            && request_text.contains("\nwake root\n"),
        "request must include delivered wake message: {request_text}",
    );
}

#[tokio::test]
async fn pending_agent_message_reaches_next_request() {
    let provider = MockProvider::new(vec![vec![
        text_delta("handled"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let pending = std::sync::Arc::new(crate::agent::PendingAgentMessages::new());
    let queued = make_channel_message(
        "spawn/worker",
        "durable push",
        crate::r#loop::inbound::MessageKind::Update,
        0,
    );
    let message_id = queued.id;
    let message_id_string = message_id.to_string();
    let queued_at = queued.timestamp;
    let to_label = "/root".to_owned();
    let mut queued = queued;
    queued.to_id = agent_id;
    pending
        .queue(crate::agent::PendingAgentMessage::new(
            queued, to_label, queued_at,
        ))
        .expect("queue pending message");

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.agent_id = Some(agent_id);
    loop_ctx.pending_agent_messages = Some(std::sync::Arc::clone(&pending));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("handled".to_string()));
    assert!(pending.is_empty(), "pending message must be drained once");

    let requests = provider.requests().expect("requests");
    let request_text = requests[0]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("<agent_message from=\"spawn/worker\"")
            && request_text.contains("kind=\"update\"")
            && request_text.contains("\ndurable push\n"),
        "first request must include queued agent message: {request_text}",
    );
    assert!(
        store.events().iter().any(|event| matches!(
            event,
            SessionEvent::Custom { event_type, data, .. }
                if event_type == crate::agent::AGENT_MESSAGE_DEQUEUED_EVENT_TYPE
                    && data.get("message_id").and_then(serde_json::Value::as_str)
                        == Some(message_id_string.as_str())
        )),
        "draining the pending message must append a dequeued audit event",
    );
}

#[tokio::test]
async fn pending_message_seeded_step_resumes_without_empty_prompt() {
    let provider = MockProvider::new(vec![vec![
        text_delta("resumed"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let pending = std::sync::Arc::new(crate::agent::PendingAgentMessages::new());
    let mut queued = make_channel_message(
        "spawn/worker",
        "durable resume",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    );
    queued.to_id = agent_id;
    let message_id = queued.id;
    let queued_at = queued.timestamp;
    pending
        .queue(crate::agent::PendingAgentMessage::new(
            queued,
            "/root".to_owned(),
            queued_at,
        ))
        .expect("queue pending message");

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.agent_id = Some(agent_id);
    loop_ctx.pending_agent_messages = Some(std::sync::Arc::clone(&pending));

    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        initial_messages: Vec::new(),
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("pending-message step completes");
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("resumed".to_string()));

    let events = store.events();
    let user_messages = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_messages.len(), 1, "no synthetic empty prompt");
    assert!(user_messages[0].contains("\ndurable resume\n"));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Custom {
                event_type,
                data,
                ..
            } if event_type == crate::agent::AGENT_MESSAGE_DEQUEUED_EVENT_TYPE
                && data["message_id"] == message_id.to_string()
        )
    }));
    assert!(pending.is_empty(), "resume step drains pending store");
}

// -- R3 (N-011) regression: drain still works between turns ----------
//
// The existing `steer_message_injected_between_turns` test above (and
// its `multiple_steer_messages_injected_in_timestamp_order` sibling)
// already cover R3's "Inbound channel drained after tool batch" and
// "Steer messages become UserMessage events before next call"
// acceptance bullets. The follow-up tests
// (`schema_mode_follow_up_triggers_continuation`,
// `no_schema_mode_follow_up_triggers_continuation`, and
// `follow_up_does_not_consume_schema_budget`) cover the
// "Follow-up messages injected only when loop would return Completed"
// bullet. They live above in this same module and remain unchanged.

// -- R2 (N-011): iteration monitor wiring ----------------------------

fn iteration_monitor(handoff_pct: f64, warn_pct: f64) -> crate::r#loop::IterationMonitorConfig {
    crate::r#loop::IterationMonitorConfig {
        context_window_tokens: 20,
        warn_threshold_pct: warn_pct,
        handoff_threshold_pct: handoff_pct,
        handoff_guidance: "Wrap up cleanly.".to_string(),
        failure_repeat_window: 0,
        hedging_patterns: Vec::new(),
    }
}

/// R2 acceptance: `evaluate_iteration` fires once per loop iteration and
/// a `TokenWarning` is recorded as a `Custom` event in the store. The
/// `MockProvider` emits 10 input + 5 output = 15 tokens per turn, so a
/// 20-token window with warn=0.5 / handoff=0.99 puts the first iteration
/// at 75% utilisation — squarely in the warn band.
#[tokio::test]
async fn token_warning_appends_custom_event() {
    let events = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"warned"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(iteration_monitor(0.99, 0.5));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "warned");

    let token_warnings: Vec<SessionEvent> = store
        .events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == "iteration.token_warning"
            )
        })
        .collect();
    assert_eq!(
        token_warnings.len(),
        1,
        "exactly one iteration.token_warning event expected, got {token_warnings:?}",
    );
    if let SessionEvent::Custom { data, .. } = &token_warnings[0] {
        assert_eq!(data["used"], 15);
        assert_eq!(data["limit"], 20);
        assert!(data["pct"].as_f64().is_some(), "pct must be numeric");
    }
}

/// R2 acceptance: `HandoffTriggered` injects a wrap-up `UserMessage`
/// that the next provider call sees. Turn 1 makes a tool call so the
/// loop's `ToolsOnly` branch keeps the loop running; the handoff message
/// is then visible to turn 2's provider call.
#[tokio::test]
async fn handoff_triggered_injects_user_message() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![text_delta("wrapping up"), done_event(StopReason::EndTurn)];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    // Handoff at 50% — first iteration's 15/20 = 75% triggers it.
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(iteration_monitor(0.5, 0.5));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("wrapping up".to_string()));

    // A handoff-shaped UserMessage must be present in the audit trail.
    let handoff_text = store.events().into_iter().find_map(|e| {
        if let SessionEvent::UserMessage { content, .. } = e
            && content.contains("Wrap up cleanly.")
            && content.contains("75.0%")
            && content.contains("summarize")
        {
            return Some(content);
        }
        None
    });
    assert!(
        handoff_text.is_some(),
        "expected a wrap-up UserMessage with guidance + percentage + summarize",
    );

    // And the provider must have been called twice — turn 2 must see
    // the handoff guidance before producing its wrap-up text.
    assert_eq!(
        provider.call_count(),
        2,
        "handoff must NOT terminate the loop; turn 2 must still run",
    );
}

/// R2 supporting: the `LoopContext::default()` iteration monitor field
/// is `None`, so existing tests (none of which set it) run unchanged.
#[test]
fn default_loop_context_has_no_iteration_monitor() {
    let ctx = LoopContext::default();
    assert!(
        ctx.iteration_monitor.is_none(),
        "default must be None so existing tests run unchanged",
    );
}

// -- R5 Test: drain occurs after tool batch, not mid-batch -----------

#[tokio::test]
async fn no_inbound_when_no_channel_is_safe() {
    // Regression: passing None for inbound on every existing path
    // should not crash.
    let turn1 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"clean"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "clean");
}

// -- N-017 R2/R3 wiring: rule with path glob fires on Write tool -----

/// Register a `**/*.rs` path-glob rule with `SystemContextAppend`
/// delivery, then run a turn that calls the `write` tool on a `.rs`
/// file. The rule's body must appear in a `<system-context>` user
/// message on the next provider call while the System message stays
/// stable.
#[tokio::test]
async fn rule_with_path_glob_fires_when_write_tool_runs() {
    use std::sync::Arc;

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreLlmHook};
    use crate::provider::request::{MessageRole, ProviderRequest};
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{
        DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming as TT,
    };

    struct CaptureSystem {
        captured: Arc<parking_lot::Mutex<Vec<CapturedTurn>>>,
    }
    #[async_trait::async_trait]
    impl PreLlmHook for CaptureSystem {
        async fn before_llm(&self, req: &ProviderRequest) -> HookOutcome {
            let system = req
                .messages
                .first()
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            let dynamic = req
                .messages
                .get(1)
                .filter(|m| matches!(m.role, MessageRole::Developer))
                .and_then(|m| m.content.clone());
            self.captured.lock().push(CapturedTurn { system, dynamic });
            HookOutcome::Proceed
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };

    let rule = Rule {
        id: RuleId::from("rust-conventions"),
        name: "Rust Conventions".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::SystemContextAppend,
        timing: TT::Before,
        body: "Follow Rust conventions.".to_string(),
        shell_source: None,
    };

    let captured: Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystem {
        captured: Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[write_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    let snapshots = captured.lock().clone();
    assert_eq!(snapshots.len(), 2, "expected two provider calls");

    assert_eq!(
        snapshots[0].system, "base-system",
        "turn 1 system must be the stable base",
    );
    let dyn_0 = snapshots[0].dynamic.as_deref().unwrap_or("");
    assert!(
        !dyn_0.contains("Follow Rust conventions."),
        "turn 1 dynamic must not yet contain the rule body",
    );

    assert_eq!(
        snapshots[1].system, "base-system",
        "turn 2 system must stay stable (same as turn 1)",
    );
    let dynamic = snapshots[1]
        .dynamic
        .as_ref()
        .expect("turn 2 must have a Developer message");
    assert!(
        dynamic.contains("Follow Rust conventions."),
        "developer message must contain rule body, got: {dynamic}",
    );
}

// -- N-017 R4 wiring: PreToolHook blocks bash ------------------------

/// Register a `PreToolHook` that blocks the `bash` tool. Run a turn
/// that calls bash; verify the tool result records the block reason
/// instead of the executor's output.
#[tokio::test]
async fn pre_tool_hook_blocks_bash() {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;

    struct BlockBash;
    #[async_trait::async_trait]
    impl PreToolHook for BlockBash {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == "bash" {
                HookOutcome::Block {
                    reason: "bash blocked".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command":"ls"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"after-block"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    // Handler that would PANIC if invoked, proving the block prevented
    // execution.
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "bash".to_string(),
        Box::new(|_| panic!("bash executor must not run when pre-tool hook blocks")),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let bash_tool = ToolDefinition {
        name: "bash".to_string(),
        description: "Run bash".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreTool(Box::new(BlockBash)));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[bash_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "after-block");

    // The bash tool result must contain the block reason.
    let events = store.events();
    let bash_result = events.iter().find_map(|e| {
        if let SessionEvent::ToolResult {
            tool_name, output, ..
        } = e
        {
            (tool_name == "bash").then(|| output.clone())
        } else {
            None
        }
    });
    let bash_output = bash_result.expect("bash ToolResult missing");
    // Hook blocks persist as the typed `blocked` payload (kind +
    // message + machine-readable detail), not a collapsed string.
    assert_eq!(
        bash_output["error"]["kind"], "blocked",
        "hook block must carry the typed kind, got: {bash_output}",
    );
    let message = bash_output["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("blocked by hook") && message.contains("bash blocked"),
        "expected block reason in bash output, got: {bash_output}",
    );
}

// -- NH-001 R3 wiring: PreToolHook rewrites bash args via Modify ------

/// Register a `PreToolHook` that returns `HookOutcome::Modify` with a
/// rewritten command. The mock bash handler records the args it sees;
/// after the turn, the recorded args must match the hook's replacement
/// rather than the original `tc.arguments`.
#[tokio::test]
async fn pre_tool_hook_modifies_bash_args() {
    use std::sync::{Arc, Mutex};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;

    struct RewriteBash;
    #[async_trait::async_trait]
    impl PreToolHook for RewriteBash {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == "bash" {
                HookOutcome::Modify {
                    updated_input: serde_json::json!({ "command": "echo modified" }),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command":"ls"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"after-modify"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let recorded: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let recorded_for_handler = Arc::clone(&recorded);

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "bash".to_string(),
        Box::new(move |args| {
            let mut slot = recorded_for_handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *slot = Some(args);
            Ok(serde_json::json!({"stdout": "modified"}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let bash_tool = ToolDefinition {
        name: "bash".to_string(),
        description: "Run bash".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreTool(Box::new(RewriteBash)));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[bash_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "after-modify");

    // The mock bash handler must have received the modified args, not
    // the model's original tc.arguments.
    let seen = recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("bash handler must have been invoked");
    assert_eq!(seen["command"], "echo modified");
}

// -- N-017 R5 wiring: PreLlmHook blocks after 3 calls ----------------

/// Register a `PreLlmHook` backed by an atomic counter that blocks on
/// the third call. Drive a mock provider whose first two turns make
/// tool calls so the loop keeps running; the third turn must return
/// `Err(NornError::HookBlocked)`.
#[tokio::test]
async fn pre_llm_hook_blocks_after_three_calls() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::error::HookType;
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreLlmHook};
    use crate::provider::request::ProviderRequest;

    struct BlockOnThird {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PreLlmHook for BlockOnThird {
        async fn before_llm(&self, _req: &ProviderRequest) -> HookOutcome {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= 3 {
                HookOutcome::Block {
                    reason: "third strike".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc1", Some("read_file"), r#"{"path":"a"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc2", Some("read_file"), r#"{"path":"b"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn3 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"never"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(BlockOnThird {
        calls: Arc::clone(&calls),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let tools = [read_file_tool_def()];
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &tools,
        output_schema: Some(&schema),
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await;

    match result {
        Err(NornError::HookBlocked { hook_type, reason }) => {
            assert_eq!(hook_type, HookType::PreLlm);
            assert_eq!(reason, "third strike");
        }
        other => panic!("expected HookBlocked, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "hook must have observed exactly three calls",
    );
}

// -- N-017 R6 wiring: SessionEventHook counts all appends ------------

/// Register a `SessionEventHook` that increments an atomic counter on
/// every event. After a two-turn loop with one tool call and a
/// structured-output finish, the counter must equal the number of
/// events visible from `store.events()`.
#[tokio::test]
async fn session_event_hook_counts_all_appends() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};

    struct CountAll {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl SessionEventHook for CountAll {
        async fn on_event(&self, _event: &SessionEvent) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let counter = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(CountAll {
        counter: Arc::clone(&counter),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let stored = store.len();
    assert!(stored >= 4, "expected at least 4 events, got {stored}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        stored,
        "session-event hook must fire once per stored event",
    );
}

// -- N-020 R4: reasoning_effort threads through to ProviderRequest --

/// Capture the most recent provider request, exposing its
/// `reasoning_effort` field for assertion.
struct CaptureReasoning {
    observed: std::sync::Arc<parking_lot::Mutex<Option<crate::provider::request::ReasoningEffort>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureReasoning {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.reasoning_effort;
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R4: When `loop_context.reasoning_effort` is set, the
/// `ProviderRequest` constructed by the loop must carry that value.
#[tokio::test]
async fn reasoning_effort_threads_to_provider_request() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::provider::request::ReasoningEffort;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Option<ReasoningEffort>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureReasoning {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.reasoning_effort = Some(ReasoningEffort::Low);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");
    let captured = *observed.lock();
    assert_eq!(
        captured,
        Some(ReasoningEffort::Low),
        "ProviderRequest must carry the LoopContext's reasoning_effort",
    );
}

struct CaptureServiceTier {
    observed: std::sync::Arc<parking_lot::Mutex<Option<crate::provider::request::ServiceTier>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureServiceTier {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.service_tier;
        crate::integration::hooks::HookOutcome::Proceed
    }
}

#[tokio::test]
async fn service_tier_threads_to_provider_request() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::provider::request::ServiceTier;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Option<ServiceTier>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureServiceTier {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.service_tier = Some(ServiceTier::Fast);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");
    assert_eq!(*observed.lock(), Some(ServiceTier::Fast));
}

// -- N-020 R5: slash command expansion lands in provider messages --

/// Capture the messages on the most recent provider request so we can
/// assert the slash expansion replaced the literal `/command …` text.
struct CaptureMessages {
    observed: std::sync::Arc<parking_lot::Mutex<Vec<Message>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureMessages {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.messages.clone();
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R5: A registered `/review foo.rs` slash command must expand
/// the literal user input into the handler's messages BEFORE the
/// provider call. The literal `/review foo.rs` text must not appear as
/// a `UserMessage` in the provider request.
#[tokio::test]
async fn slash_command_expands_before_provider_call() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::r#loop::commands::{SlashCommand, SlashCommandHandler, SlashCommandRegistry};

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Vec<Message>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureMessages {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut slash = SlashCommandRegistry::new();
    slash.register(SlashCommand {
        name: "review".to_owned(),
        handler: SlashCommandHandler::Skill {
            skill_name: "review".to_owned(),
        },
    });

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.slash_commands = Some(slash);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "/review foo.rs",
        tools: &[],
        output_schema: Some(&schema),
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("loop succeeds");
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    let messages = observed.lock().clone();
    // The literal `/review foo.rs` text must NOT appear in any user
    // message that hit the provider — the slash expansion must replace
    // it. The expansion contains both 'review' and 'foo.rs'.
    let user_bodies: Vec<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .filter_map(|m| m.content.clone())
        .collect();
    assert!(
        !user_bodies.iter().any(|b| b == "/review foo.rs"),
        "literal /review must be replaced by expansion; got {user_bodies:?}",
    );
    assert!(
        user_bodies
            .iter()
            .any(|b| b.contains("review") && b.contains("foo.rs")),
        "expansion must reference both skill name and argument; got {user_bodies:?}",
    );
}

// -- N-020 R6: prompt command stdout appears in system instruction --

/// Captured request snapshot: system message (messages[0]) content
/// and the Developer message (messages[1]) content.
#[derive(Clone, Debug)]
struct CapturedTurn {
    system: String,
    dynamic: Option<String>,
}

/// Capture the System message and Developer message content on
/// each provider call.
struct CaptureSystemContent {
    captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureSystemContent {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        let system = request
            .messages
            .first()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let dynamic = request
            .messages
            .get(1)
            .filter(|m| matches!(m.role, MessageRole::Developer))
            .and_then(|m| m.content.clone());
        self.captured.lock().push(CapturedTurn { system, dynamic });
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R6: a successful prompt command's stdout appears in the
/// Developer message (messages[1]), not in the System message (which
/// stays stable for prefix caching).
#[tokio::test]
async fn prompt_command_appears_in_dynamic_context() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::profile::PromptCommand;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystemContent {
        captured: std::sync::Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    loop_ctx.prompt_commands.push(PromptCommand {
        name: "cwd".to_owned(),
        command: "echo Current dir: token-found".to_owned(),
        cache_ttl: None,
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let _ = assert_completed(result);
    let snapshots = captured.lock().clone();
    assert!(!snapshots.is_empty(), "expected at least one provider call");
    assert_eq!(
        snapshots[0].system, "base-system",
        "system message must stay stable; got: {}",
        snapshots[0].system,
    );
    let dynamic = snapshots[0]
        .dynamic
        .as_ref()
        .expect("Developer message must be present at messages[1]");
    assert!(
        dynamic.contains("token-found"),
        "prompt command stdout must appear in dynamic context; got: {dynamic}",
    );
    assert!(
        dynamic.contains("cwd"),
        "prompt command name should appear as a section heading; got: {dynamic}",
    );
}

/// N-020 R6: a failing prompt command (non-zero exit) is logged and
/// skipped — it must NOT abort the loop and must NOT add a section.
#[tokio::test]
async fn prompt_command_failure_skips_section_without_abort() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::profile::PromptCommand;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystemContent {
        captured: std::sync::Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    loop_ctx.prompt_commands.push(PromptCommand {
        name: "bad".to_owned(),
        command: "exit 7".to_owned(),
        cache_ttl: None,
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    let snapshots = captured.lock().clone();
    assert!(
        !snapshots.is_empty(),
        "loop must complete despite prompt-command failure",
    );
    assert_eq!(
        snapshots[0].system, "base-system",
        "failed prompt command must not append a section",
    );
    let dyn_content = snapshots[0].dynamic.as_deref().unwrap_or("");
    assert!(
        !dyn_content.contains("bad"),
        "failed prompt command must not add its section to the developer message; got: {dyn_content}",
    );
}

// NH-006 R3 / C54: a UserPromptHook returning Block must short-
// circuit the loop entry. The agent step returns
// `NornError::HookBlocked { hook_type: UserPrompt, .. }` and no
// provider call is dispatched.
#[tokio::test]
async fn user_prompt_hook_block_returns_hook_blocked_error() {
    use crate::error::HookType;
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, UserPromptHook};

    struct AlwaysBlock;
    #[async_trait::async_trait]
    impl UserPromptHook for AlwaysBlock {
        async fn on_user_prompt(&self, _prompt: &str, _session_id: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "not allowed".to_owned(),
            }
        }
    }

    // Provider that panics if called — proves no provider request
    // ever fires when the user_prompt hook blocks.
    let provider = MockProvider::new(Vec::new());
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::UserPrompt(Box::new(AlwaysBlock)));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let tools = [read_file_tool_def()];
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hello",
        tools: &tools,
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await;

    match result {
        Err(NornError::HookBlocked { hook_type, reason }) => {
            assert_eq!(hook_type, HookType::UserPrompt);
            assert_eq!(reason, "not allowed");
        }
        other => panic!("expected HookBlocked, got {other:?}"),
    }
}

// NH-006 R7 / C59: PostToolFailureHook fires (additively to the
// existing PostToolHook) when a tool returns an error output. The
// counter increments on the erroring tool only — successful tool
// calls in the same turn do not fire it.
#[tokio::test]
async fn post_tool_failure_hook_fires_only_on_error_output() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookRegistry, PostToolFailureHook, PostToolHook};

    struct CountFailure {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PostToolFailureHook for CountFailure {
        async fn after_tool_failure(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _output: &crate::tool::traits::ToolOutput,
            _ctx: &crate::tool::context::ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct CountSuccess {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PostToolHook for CountSuccess {
        async fn after_tool(
            &self,
            _envelope: &crate::tool::envelope::ToolEnvelope,
            _output: &crate::tool::traits::ToolOutput,
            _ctx: &crate::tool::context::ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Tool handler that always errors so the dispatcher wraps the
    // output as {"error": "..."} — the `is_error` test inside
    // tool_dispatch sees this and fires PostToolFailureHook.
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "always_fails".to_string(),
        Box::new(|_| {
            Err(crate::error::ToolError::ExecutionFailed {
                reason: "boom".to_owned(),
            })
        }),
    );

    let turn1 = vec![
        tool_call_delta("tc_fail", Some("always_fails"), r"{}"),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_done",
            Some("structured_output"),
            r#"{"answer":"finished"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();

    let failure_count = Arc::new(AtomicUsize::new(0));
    let success_count = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PostToolFailure(Box::new(CountFailure {
        counter: Arc::clone(&failure_count),
    })));
    hooks.register(Hook::PostTool(Box::new(CountSuccess {
        counter: Arc::clone(&success_count),
    })));

    let tool_def = ToolDefinition {
        name: "always_fails".to_string(),
        description: "Always fails".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[tool_def],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let _ = assert_completed(result);

    assert_eq!(
        failure_count.load(Ordering::SeqCst),
        1,
        "PostToolFailureHook fires once for the erroring tool call",
    );
    // PostToolHook fires only for externally dispatched tools in this
    // path; the structured_output completion is not routed through the
    // normal tool-dispatch hook pipeline.
    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "PostToolHook fires once for the erroring tool call in this path",
    );
}

// NH-006 R4 / C55: a StopHook returning Block once then Proceed
// forces the loop to take one extra iteration with the block reason
// injected as a user message, then complete normally on the second
// round.
#[tokio::test]
async fn stop_hook_block_forces_extra_iteration() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, StopHook};

    struct BlockOnce {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl StopHook for BlockOnce {
        async fn on_stop(&self, _final_text: &str) -> HookOutcome {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                HookOutcome::Block {
                    reason: "keep going".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    // Two terminal turns: the first produces final text and the
    // hook blocks. The second runs after the injected user message
    // and the hook proceeds.
    let turn1 = vec![
        ProviderEvent::TextDelta {
            text: "round one".to_owned(),
        },
        done_event(StopReason::EndTurn),
    ];
    let turn2 = vec![
        ProviderEvent::TextDelta {
            text: "round two".to_owned(),
        },
        done_event(StopReason::EndTurn),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());

    let calls = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Stop(Box::new(BlockOnce {
        calls: Arc::clone(&calls),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hi",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("loop completes");

    match result {
        AgentStepResult::Completed { output, .. } => {
            assert_eq!(output, Value::String("round two".to_owned()));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "StopHook must observe both terminal classifications",
    );
}

// -- NB-P2: CancellationToken (C9 / C10 / C11) ------------------------

/// Provider whose event stream never yields anything, so
/// `call_provider`'s `next().await` hangs forever. Lets C10 exercise
/// the `tokio::select!` cancel arm against an in-flight provider
/// call without depending on real I/O.
struct HangingProvider;

impl Provider for HangingProvider {
    fn stream(
        &self,
        _request: ProviderRequest,
    ) -> Result<crate::provider::traits::ProviderStream, ProviderError> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
}

#[tokio::test]
async fn cancellation_before_first_iteration_returns_cancelled() {
    // C9: token is already cancelled when the loop starts, so the
    // top-of-iteration check fires before the provider is ever
    // invoked. A `HangingProvider` proves it — if the gate didn't
    // catch the cancel, the test would hang.
    let provider = HangingProvider;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let token = CancellationToken::new();
    token.cancel();

    let mut loop_ctx = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(token),
    })
    .await
    .expect("Cancelled is a structured result, not an error");

    assert!(
        matches!(result, AgentStepResult::Cancelled { .. }),
        "expected Cancelled, got {result:?}",
    );
}

#[tokio::test]
async fn cancellation_mid_iteration_returns_cancelled() {
    // C10: token fires while the provider call is in flight. The
    // tokio::select! race in the loop body resolves the cancel arm
    // and returns Cancelled. Usage stays zero because the provider
    // never produced a Done event (and so no `total_usage += ...`
    // ever ran), which matches the R3 acceptance — partial usage is
    // captured *if available*, not synthesised.
    let provider = HangingProvider;
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let token = CancellationToken::new();
    let config = default_config();

    let mut loop_ctx = LoopContext::new("system");
    let step = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(token.clone()),
    });
    let cancel_after_delay = async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();
    };

    let (result, ()) = tokio::join!(step, cancel_after_delay);
    let result = result.expect("Cancelled is structured, not an error");
    assert!(
        matches!(result, AgentStepResult::Cancelled { .. }),
        "expected Cancelled, got {result:?}",
    );
}

#[tokio::test]
async fn no_cancellation_token_runs_to_completion_unchanged() {
    // C11: regression baseline — passing `None` for `cancel`
    // bypasses the select! and direct-awaits the provider, so the
    // loop produces the same Completed result it did before NB-P2.
    let events = vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(StopReason::EndTurn),
    ];
    let provider = MockProvider::new(vec![events]);
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();

    let mut loop_ctx = LoopContext::new("system");
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "hello",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("loop completes with None cancel");

    assert!(
        matches!(result, AgentStepResult::Completed { .. }),
        "expected Completed with None cancel, got {result:?}",
    );
}

#[tokio::test]
async fn custom_tool_call_kind_propagated_to_session_event() {
    let events = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "ctc_1".to_string(),
            call_id: None,
            name: Some("apply_patch".to_string()),
            arguments_delta: "patch content".to_string(),
            kind: crate::provider::request::ToolCallKind::Custom,
        },
        ProviderEvent::ToolCallComplete {
            call_id: "call_custom".to_string(),
            name: "apply_patch".to_string(),
            arguments: "patch content".to_string(),
            kind: crate::provider::request::ToolCallKind::Custom,
        },
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![
        events,
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "apply_patch".to_string(),
        Box::new(|_| Ok(serde_json::json!({"applied": true}))),
    );
    let executor = MockToolExecutor::new(handlers);

    let _result = run_step(
        &provider,
        &executor,
        &store,
        &[ToolDefinition {
            name: "apply_patch".to_string(),
            description: "Apply a patch".to_string(),
            parameters: serde_json::json!({}),
        }],
        None,
        &default_config(),
        None,
    )
    .await;

    let assistant_event = store.events().into_iter().find_map(|e| {
        if let SessionEvent::AssistantMessage { tool_calls, .. } = e
            && !tool_calls.is_empty()
        {
            return Some(tool_calls);
        }
        None
    });
    let tool_calls = assistant_event.expect("AssistantMessage with tool_calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(
        tool_calls[0].kind,
        crate::provider::request::ToolCallKind::Custom,
        "ToolCallEvent.kind must propagate Custom from AssembledToolCall, not hardcode Function",
    );
    assert_eq!(tool_calls[0].call_id, "call_custom");
}

// -- REVIEW H3: SchemaInvalid must answer post-schema tool calls -------
//
// Turn 1 returns [structured_output(invalid), read_file]; turn 2
// returns a valid schema call. Pre-fix, tc_read was left unanswered:
// turn 2's request carried a dangling tool call and real providers
// reject it with a 400, wedging the retry loop.

#[tokio::test]
async fn schema_invalid_rejects_post_schema_tool_calls() {
    let turn1 = vec![
        tool_call_delta("tc_schema_1", Some("structured_output"), r#"{"wrong":1}"#),
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema_2",
            Some("structured_output"),
            r#"{"answer":"ok"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::new(read_file_handlers());
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def()],
        Some(&schema),
        &default_config(),
        None,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    // Exactly one persisted result per call_id across the whole step.
    let mut result_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for event in store.events() {
        if let SessionEvent::ToolResult { tool_call_id, .. } = event {
            *result_counts.entry(tool_call_id).or_insert(0) += 1;
        }
    }
    assert_eq!(
        result_counts.get("tc_read"),
        Some(&1),
        "post-schema call after invalid schema must get exactly one result",
    );
    assert_eq!(result_counts.get("tc_schema_1"), Some(&1));
    assert_eq!(result_counts.get("tc_schema_2"), Some(&1));

    // The rejection is visible to the model on the retry request.
    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 2);
    let answered = requests[1].messages.iter().any(|m| {
        matches!(m.role, MessageRole::ToolResult) && m.tool_call_id.as_deref() == Some("tc_read")
    });
    assert!(
        answered,
        "retry request must carry a result for the post-schema call",
    );
}

// -- REVIEW H2: developer-message sync must not clobber history --------

/// Resume with a compaction summary in history and no dynamic context:
/// pre-fix, the sync's first-Developer-role lookup matched the summary
/// and the `(None, Some(idx))` arm deleted it from the prompt.
#[tokio::test]
async fn history_compaction_summary_survives_dev_sync() {
    let store = EventStore::new();
    store
        .append(SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "older history summary".to_string(),
            replaced_event_ids: Vec::new(),
        })
        .expect("seed compaction");

    let provider = MockProvider::new(vec![vec![
        text_delta("hi"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    let summary_present = requests[0].messages.iter().any(|m| {
        matches!(m.role, MessageRole::Developer)
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("older history summary"))
    });
    assert!(
        summary_present,
        "history compaction summary must survive the developer-message sync: {:?}",
        requests[0].messages,
    );
}

/// Seam I2-1 (quadratic compaction re-walk): persisted compaction
/// marks load exactly once per loop context. Step 1 of a resumed
/// session (fresh `ContextEdits`, compaction already in the store)
/// walks the store and hides superseded history; step 2 must NOT
/// re-walk — proven by appending a raw compaction event between the
/// steps that only a re-walk could observe and asserting its
/// replaced event stays visible — while the step-1 marks survive.
#[tokio::test]
async fn persisted_compaction_marks_load_once_per_loop_context() {
    let store = EventStore::new();
    let old_question = store
        .append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "old question".to_string(),
        })
        .expect("seed user");
    let old_answer = store
        .append(SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "old answer".to_string(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        })
        .expect("seed assistant");
    // Persist a compaction the way a *previous run* would have — on a
    // tracker this loop context never sees.
    let mut prior_run_edits = crate::session::context_edit::ContextEdits::new();
    prior_run_edits
        .summarize(
            &store,
            vec![old_question, old_answer],
            "seeded summary".to_string(),
        )
        .expect("seed compaction");

    let provider = MockProvider::new(vec![
        vec![text_delta("first"), done_event(StopReason::EndTurn)],
        vec![text_delta("second"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    assert!(!loop_ctx.compaction_marks_loaded);

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);
    assert!(
        loop_ctx.compaction_marks_loaded,
        "the first step must record the one-time load",
    );

    // Between the steps, append a compaction event directly to the
    // store, superseding step 1's assistant turn. Nothing marks it on
    // the loop's tracker — only a per-step store re-walk could pick
    // it up, and the re-walk no longer exists.
    let first_answer_id = store
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::AssistantMessage { base, content, .. } if content == "first" => {
                Some(base.id.clone())
            }
            _ => None,
        })
        .expect("step-1 assistant event persisted");
    store
        .append(SessionEvent::Compaction {
            base: EventBase::new(store.last_event_id()),
            summary: "rogue walk detector".to_string(),
            replaced_event_ids: vec![first_answer_id],
        })
        .expect("append rogue compaction");

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 2);
    let contains = |req: &crate::provider::request::ProviderRequest, needle: &str| {
        req.messages
            .iter()
            .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
    };

    // Step 1: resume load hid the superseded history, summary present.
    assert!(
        !contains(&requests[0], "old answer"),
        "step 1 must hide history superseded before resume: {:?}",
        requests[0].messages,
    );
    assert!(contains(&requests[0], "seeded summary"));

    // Step 2: marks from the one-time load still hold...
    assert!(
        !contains(&requests[1], "old answer"),
        "step 2 must keep the resume-loaded marks: {:?}",
        requests[1].messages,
    );
    // ...and the raw between-steps compaction was NOT re-walked in:
    // its replaced event is still visible.
    assert!(
        contains(&requests[1], "first"),
        "a per-step store re-walk crept back in — step 2 hid an event \
         that only apply_persisted_compactions could have marked: {:?}",
        requests[1].messages,
    );
}

/// Seam I2-1, mid-session half: a compaction that fires *during* a
/// step marks supersession at commit time on the loop's own tracker,
/// so the following step of the same loop context still sees the
/// compacted view — with no per-step re-walk to fall back on.
#[tokio::test]
async fn mid_session_compaction_marks_survive_into_the_next_step() {
    let store = EventStore::new();
    for i in 0..6 {
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: format!("seed question {i} {}", "x".repeat(200)),
            })
            .expect("seed user");
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: format!("seed answer {i} {}", "y".repeat(200)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: None,
            })
            .expect("seed assistant");
    }

    // Step 1 fires auto-compaction (summarization call + main call);
    // step 2 runs with generous limits so no second trigger fires.
    let provider = MockProvider::new(vec![
        vec![
            text_delta("LLM summary of the seed turns"),
            done_event(StopReason::EndTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
        vec![text_delta("later"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let compacting_config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &compacting_config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let relaxed_config = AgentLoopConfig {
        context_window_limit: Some(1_000_000),
        auto_compact_reserve_tokens: Some(10_000),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &relaxed_config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 3, "summarization + step 1 + step 2");
    let step_two = &requests[2];
    assert!(
        !step_two.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "the step-1 compaction's marks must persist into step 2 without \
         any store re-walk: {:?}",
        step_two.messages,
    );
    assert!(
        step_two.messages.iter().any(|m| {
            matches!(m.role, MessageRole::Developer)
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("LLM summary of the seed turns"))
        }),
        "the compaction summary must ride into step 2",
    );
}

/// Resume with a compaction summary in history while dynamic context
/// appears mid-step (environment section): pre-fix, the `(Some, Some)`
/// arm overwrote the summary with the dynamic context. Post-fix the
/// dynamic context gets its own message and the summary survives.
#[tokio::test]
async fn dynamic_context_does_not_overwrite_history_summary() {
    let store = EventStore::new();
    store
        .append(SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "older history summary".to_string(),
            replaced_event_ids: Vec::new(),
        })
        .expect("seed compaction");

    let provider = MockProvider::new(vec![vec![
        text_delta("hi"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    // Environment sections are injected at the top of each iteration,
    // i.e. AFTER the initial prompt was built without dynamic context —
    // exactly the resume shape that triggered the overwrite.
    loop_ctx.environment = Some(crate::system_prompt::environment::EnvironmentConfig {
        session_id: Some("sess-h2".to_owned()),
        model: "test-model".to_owned(),
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    let developer_contents: Vec<&str> = requests[0]
        .messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Developer))
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert!(
        developer_contents
            .iter()
            .any(|c| c.contains("older history summary")),
        "history summary must survive: {developer_contents:?}",
    );
    assert!(
        developer_contents
            .iter()
            .any(|c| c.contains("# Environment")),
        "dynamic context must be present in its own message: {developer_contents:?}",
    );
    assert!(
        !developer_contents
            .iter()
            .any(|c| c.contains("older history summary") && c.contains("# Environment")),
        "summary and dynamic context must be separate messages: {developer_contents:?}",
    );
}

// -- REVIEW item 4: RepeatedFailure monitor fires on real failures -----

#[tokio::test]
async fn repeated_tool_failures_fire_monitor() {
    let failing_call = |id: &str| {
        vec![
            tool_call_delta(id, Some("read_file"), r#"{"path":"f"}"#),
            done_event(StopReason::ToolUse),
        ]
    };
    let provider = MockProvider::new(vec![
        failing_call("tc1"),
        failing_call("tc2"),
        vec![text_delta("giving up"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_| {
            Err(crate::error::ToolError::ExecutionFailed {
                reason: "permission denied at line 42".to_string(),
            })
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.iteration_monitor = Some(crate::r#loop::IterationMonitorConfig {
        context_window_tokens: 0,
        warn_threshold_pct: 1.0,
        handoff_threshold_pct: 1.0,
        handoff_guidance: String::new(),
        failure_repeat_window: 2,
        hedging_patterns: Vec::new(),
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let repeated_failure = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "iteration.repeated_failure" => Some(data),
        _ => None,
    });
    let data = repeated_failure
        .expect("RepeatedFailure signal must fire after two identical tool failures");
    assert_eq!(data["consecutive_count"], 2);
    let signature = data["error_signature"].as_str().unwrap_or_default();
    assert!(
        signature.contains("permission denied"),
        "signature must reflect the repeated error: {signature}",
    );
}

// -- Step timeout: accumulated usage rides the TimedOut outcome --------

/// Provider whose first call streams a complete tool-call turn (with
/// usage) and whose second call hangs forever, forcing the step
/// timeout to fire mid-run.
struct HangsOnSecondCall {
    calls: std::sync::atomic::AtomicUsize,
}

impl crate::provider::traits::Provider for HangsOnSecondCall {
    fn stream(
        &self,
        _request: ProviderRequest,
    ) -> Result<crate::provider::traits::ProviderStream, crate::error::ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            Ok(Box::pin(futures_util::stream::iter(
                vec![
                    tool_call_delta("tc1", Some("read_file"), "{}"),
                    done_event(StopReason::ToolUse),
                ]
                .into_iter()
                .map(Ok),
            )))
        } else {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }
}

/// The timed-out outcome must carry the usage accumulated by the
/// provider calls that completed before the budget elapsed — it was
/// previously zeroed because the outer timeout wrapper had no access
/// to the loop's running total.
#[tokio::test(start_paused = true)]
async fn timed_out_carries_accumulated_usage_and_partial_state() {
    let provider = HangsOnSecondCall {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let executor = MockToolExecutor::new(read_file_handlers());
    let store = EventStore::new();
    let config = AgentLoopConfig {
        step_timeout: Some(std::time::Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("timeout is a stop outcome, not an error");

    match result {
        AgentStepResult::TimedOut {
            iterations, usage, ..
        } => {
            assert_eq!(iterations, 2, "second iteration was in flight");
            // The first provider call completed and reported
            // input 10 / output 5 (see `done_event`).
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("expected AgentStepResult::TimedOut, got {other:?}"),
    }
}

// -- REVIEW item 5: truncation must not masquerade as Completed --------

async fn run_truncation_step(
    provider: &MockProvider,
    store: &EventStore,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
}

/// REVIEW item 5 (Phase 2 shape): a `max_tokens` stop with no tool
/// calls in no-schema mode is a *stopped run*, not a `Completed` one
/// and not an error. It returns the typed `Truncated` outcome carrying
/// the partial text, iteration count, and accumulated usage — making
/// the truncation impossible to mistake for success while keeping the
/// partial output on the return value. (Replaces the Phase 1 stopgap
/// that returned `ProviderError::Truncated`; truncation can no longer
/// reach the retry classifier at all, so the never-retry property is
/// structural.)
#[tokio::test]
async fn max_tokens_truncation_is_a_typed_stop_not_completed() {
    let provider = MockProvider::new(vec![vec![
        text_delta("partial answ"),
        done_event(StopReason::MaxTokens),
    ]]);
    let store = EventStore::new();

    let result = run_truncation_step(&provider, &store)
        .await
        .expect("truncation is a stop outcome, not an error");

    match result {
        AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            ..
        } => {
            assert_eq!(kind, TruncationKind::MaxTokens);
            assert_eq!(partial_text.as_deref(), Some("partial answ"));
            assert_eq!(iterations, 1);
            assert!(
                usage.input_tokens > 0 || usage.output_tokens > 0,
                "accumulated usage must ride the truncated outcome: {usage:?}"
            );
        }
        other => panic!("expected AgentStepResult::Truncated, got {other:?}"),
    }

    // Partial text + stop reason persisted for recovery.
    let assistant = store.events().into_iter().find_map(|e| match e {
        SessionEvent::AssistantMessage {
            content,
            stop_reason,
            ..
        } => Some((content, stop_reason)),
        _ => None,
    });
    let (content, stop_reason) = assistant.expect("assistant message persisted");
    assert_eq!(content, "partial answ");
    assert_eq!(stop_reason, "max_tokens");

    let truncated_event = store.events().into_iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == "loop.truncated"
        )
    });
    assert!(truncated_event, "loop.truncated event must be persisted");
}

#[tokio::test]
async fn content_filter_truncation_is_a_typed_stop_not_completed() {
    let provider = MockProvider::new(vec![vec![done_event(StopReason::ContentFilter)]]);
    let store = EventStore::new();

    let result = run_truncation_step(&provider, &store)
        .await
        .expect("content-filter stop is a stop outcome, not an error");

    match result {
        AgentStepResult::Truncated {
            kind, partial_text, ..
        } => {
            assert_eq!(kind, TruncationKind::ContentFilter);
            assert!(
                partial_text.is_none(),
                "no text was produced, so no partial text: {partial_text:?}"
            );
        }
        other => panic!("expected AgentStepResult::Truncated, got {other:?}"),
    }
}

/// With a schema present, truncation funnels into the existing nudge
/// path: budget is consumed and the step terminates `SchemaUnreachable` —
/// never a silent Completed.
#[tokio::test]
async fn truncation_with_schema_consumes_budget() {
    let provider = MockProvider::new(vec![vec![
        text_delta("partial"),
        done_event(StopReason::MaxTokens),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config_with_budget(1),
        None,
    )
    .await;

    let (_, _, attempts, _) = assert_schema_unreachable(result);
    assert_eq!(attempts, 1);
}

// -- REVIEW item 6b: compaction must affect the in-flight request ------

#[tokio::test]
async fn auto_compaction_applies_to_in_flight_request() {
    let store = EventStore::new();
    // Seed enough chunky history that the estimate crosses the
    // threshold on the very first iteration.
    for i in 0..6 {
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: format!("seed question {i} {}", "x".repeat(200)),
            })
            .expect("seed user");
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: format!("seed answer {i} {}", "y".repeat(200)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: None,
            })
            .expect("seed assistant");
    }

    // First scripted response answers the summarization call, the
    // second answers the main (compacted) request.
    let provider = MockProvider::new(vec![
        vec![
            text_delta("LLM summary of the seed turns"),
            done_event(StopReason::EndTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (_, usage) = assert_completed(result);
    // Track L finding 1: the summarization call's usage (10/5 from the
    // first scripted response) is accounted alongside the main call's.
    assert_eq!(usage.input_tokens, 20, "summarization input tokens vanish");
    assert_eq!(
        usage.output_tokens, 10,
        "summarization output tokens vanish"
    );

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(
        requests.len(),
        2,
        "expected the summarization call plus the main call",
    );
    // The summarization request is isolated: untooled and unthreaded.
    let summarization = &requests[0];
    assert!(summarization.tools.is_empty());
    assert!(summarization.previous_response_id.is_none());
    assert!(!summarization.store);
    assert!(
        summarization.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "the summarization prompt must cover the elided history",
    );

    // The compaction must have hit the FIRST main request (in-flight),
    // not just the next step: compacted turns absent, summary present.
    let main = &requests[1];
    assert!(
        !main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "compacted history must be absent from the in-flight request",
    );
    let summary_present = main.messages.iter().any(|m| {
        matches!(m.role, MessageRole::Developer)
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("LLM summary of the seed turns"))
    });
    assert!(
        summary_present,
        "in-flight request must carry the LLM-written compaction summary",
    );
    // The most recent seeded turn survives (keep_recent_turns = 1).
    assert!(
        main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed answer 5"))
        }),
        "kept turns must remain in the in-flight request",
    );
    // And the persisted state agrees for the next step: the compaction
    // record carries the LLM summary as its content.
    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    assert_eq!(
        persisted_summary.as_deref(),
        Some("LLM summary of the seed turns"),
        "the compaction record's content must be the LLM summary",
    );
    // The summarization audit event is persisted with its usage.
    let audit = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "loop.compaction_summarization" => Some(data),
        _ => None,
    });
    let audit = audit.expect("loop.compaction_summarization event persisted");
    assert_eq!(audit["summary_kind"], "llm_summary");
    assert_eq!(audit["usage"]["input_tokens"], 10);
    assert_eq!(audit["usage"]["output_tokens"], 5);
}

/// C4: a fired auto-compaction broadcasts a live [`AgentCompaction`]
/// event carrying honest accounting (reclaimed-token estimate plus the
/// summarization call's real usage); the summarization sub-call's own
/// provider stream (its `Done` / text deltas) must NOT leak onto the
/// agent event channel; and the persisted `SessionEvent::Compaction` is
/// unchanged.
#[tokio::test]
async fn auto_compaction_broadcasts_live_event_and_hides_summarization_stream() {
    use crate::provider::agent_event::{AgentEvent, AgentEventKind, CompactionSummaryKind};

    let store = EventStore::new();
    for i in 0..6 {
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: format!("seed question {i} {}", "x".repeat(200)),
            })
            .expect("seed user");
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: format!("seed answer {i} {}", "y".repeat(200)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: None,
            })
            .expect("seed assistant");
    }

    // First scripted response answers the summarization call, the second
    // answers the main (compacted) request.
    let provider = MockProvider::new(vec![
        vec![
            text_delta("LLM summary of the seed turns"),
            done_event(StopReason::EndTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };

    let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(256);
    let sender = AgentEventSender::new(tx, uuid::Uuid::nil(), "root".to_string());

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: Some(&sender),
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let _ = assert_completed(result);

    let mut compactions = Vec::new();
    let mut provider_dones = 0usize;
    let mut leaked_summary = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.event {
            AgentEventKind::Compaction(compaction) => compactions.push(compaction),
            AgentEventKind::Provider(ProviderEvent::Done { .. }) => provider_dones += 1,
            AgentEventKind::Provider(
                ProviderEvent::TextDelta { text } | ProviderEvent::TextComplete { text },
            ) if text.contains("LLM summary of the seed turns") => {
                leaked_summary = true;
            }
            _ => {}
        }
    }

    assert_eq!(
        compactions.len(),
        1,
        "exactly one live compaction event must broadcast",
    );
    let compaction = &compactions[0];
    assert!(
        compaction.events_compacted > 0,
        "the event must report the hidden turns",
    );
    assert!(
        compaction.tokens_before > compaction.tokens_after,
        "reclaim must be positive: {} -> {}",
        compaction.tokens_before,
        compaction.tokens_after,
    );
    assert!(matches!(
        compaction.summary_source,
        CompactionSummaryKind::Llm
    ));
    let usage = compaction
        .summarization_usage
        .as_ref()
        .expect("summarization usage must be carried");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);

    assert!(
        !leaked_summary,
        "the summarization sub-call's text must never leak onto the agent stream",
    );
    assert_eq!(
        provider_dones, 1,
        "only the main call's Done may broadcast — the summarization sub-call's must not",
    );

    // The session-store Compaction event is unchanged by the live broadcast.
    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    assert_eq!(
        persisted_summary.as_deref(),
        Some("LLM summary of the seed turns"),
    );
}

/// Track L finding 1 (failure policy): a failed summarization call
/// must not abort the step — the compaction still fires with the
/// mechanical digest, explicitly marked as a non-semantic fallback.
#[tokio::test]
async fn summarization_failure_falls_back_without_aborting_the_step() {
    let store = EventStore::new();
    for i in 0..6 {
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: format!("seed question {i} {}", "x".repeat(200)),
            })
            .expect("seed user");
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: format!("seed answer {i} {}", "y".repeat(200)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: None,
            })
            .expect("seed assistant");
    }

    // A truncated summarization response (MaxTokens) is unusable; the
    // main call then succeeds. Its usage must still be accounted.
    let provider = MockProvider::new(vec![
        vec![text_delta("cut off"), done_event(StopReason::MaxTokens)],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let (_, usage) = assert_completed(result);
    assert_eq!(
        usage.input_tokens, 20,
        "rejected summarization tokens were still spent and must be accounted",
    );

    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    let summary = persisted_summary.expect("compaction still fires on fallback");
    let parsed: serde_json::Value =
        serde_json::from_str(&summary).expect("fallback digest is JSON");
    assert_eq!(parsed["summary_kind"], "mechanical_digest_fallback");
    assert!(
        parsed["summarization_error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "the fallback must carry why the LLM summary was unavailable: {parsed}",
    );

    let audit = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "loop.compaction_summarization" => Some(data),
        _ => None,
    });
    let audit = audit.expect("audit event persisted on fallback too");
    assert_eq!(audit["summary_kind"], "mechanical_digest_fallback");
}

/// Track L finding 2: when compaction fires under provider-side
/// response threading, the thread anchor must be dropped so the main
/// request replays the full compacted conversation instead of
/// pointing at an uncompacted server-side thread.
#[tokio::test]
async fn compaction_drops_response_thread_anchor() {
    let store = EventStore::new();
    for i in 0..6 {
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: format!("seed question {i} {}", "x".repeat(200)),
            })
            .expect("seed user");
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: format!("seed answer {i} {}", "y".repeat(200)),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_string(),
                response_id: Some(format!("resp_seed_{i}")),
            })
            .expect("seed assistant");
    }

    let provider = MockProvider::with_capabilities(
        vec![
            vec![
                text_delta("LLM summary of the seed turns"),
                done_event(StopReason::EndTurn),
            ],
            vec![text_delta("done"), done_event(StopReason::EndTurn)],
        ],
        crate::provider::tools::ProviderCapabilities::openai_responses(),
    );
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        conversation_state: crate::r#loop::config::ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 2);
    let main = &requests[1];
    assert_eq!(
        main.previous_response_id, None,
        "a fired compaction cannot shrink a server-side thread: the \
         anchor must be dropped so the full compacted conversation is sent",
    );
    // Full replay: the kept turn and the summary ride on the request
    // itself rather than living only in the server-side thread.
    assert!(
        main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed answer 5"))
        }),
        "kept history must be replayed in full after the anchor drop",
    );
    assert!(
        main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("LLM summary of the seed turns"))
        }),
        "the compaction summary must ride on the full replay",
    );
    assert!(
        !main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "compacted history must not be replayed",
    );
}

// -- Provider tool surface: wire and prompt recomputed per request
//    from the live provider's capabilities --------------------------

fn web_search_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        description: "Search the public web.".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

#[tokio::test]
async fn hosted_capability_swaps_wire_tool_and_injects_surface_section() {
    use crate::provider::tools::{
        HostedToolDefinition, ProviderCapabilities, ProviderToolDefinition,
    };

    let provider = MockProvider::with_capabilities(
        vec![vec![text_delta("done"), done_event(StopReason::EndTurn)]],
        ProviderCapabilities {
            hosted_web_search: true,
            ..ProviderCapabilities::default()
        },
    );
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def(), web_search_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    let request = &requests[0];
    assert!(
        matches!(
            request.tools.as_slice(),
            [
                ProviderToolDefinition::Function(read),
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_)),
            ] if read.name == "read_file"
        ),
        "hosted-capable provider must receive the hosted replacement: {:?}",
        request.tools,
    );
    // The per-iteration surface note rides on the dynamic-context
    // Developer message, never on the cache-stable System message.
    assert!(
        request.messages.iter().any(|m| {
            m.role == MessageRole::Developer
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "the hosted surface note must reach the request's developer context",
    );
    assert!(
        !request.messages.iter().any(|m| {
            m.role == MessageRole::System
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "the surface note is dynamic — the System message stays cache-stable",
    );
}

#[tokio::test]
async fn function_capability_keeps_wire_tool_and_omits_surface_section() {
    use crate::provider::tools::ProviderToolDefinition;

    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def(), web_search_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    let request = &requests[0];
    assert!(
        request
            .tools
            .iter()
            .all(|tool| matches!(tool, ProviderToolDefinition::Function(_))),
        "without the capability every tool is a callable function: {:?}",
        request.tools,
    );
    assert!(
        request.tools.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Function(function) if function.name == "web_search"
        )),
        "web_search stays on the wire as a function tool",
    );
    assert!(
        !request.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "function mode needs no surface correction",
    );
}

// -- W3.6: pre-loop child-result drain folds children_usage ----------

/// Child results already buffered when the step starts are drained
/// by the runner's pre-loop sweep; each drained result's
/// `subtree_usage` must be folded into the step's `children_usage`
/// (summed across the batch) while the step's own `usage` stays
/// own-calls-only — the two never mix.
#[tokio::test]
async fn buffered_child_results_fold_into_children_usage_at_step_start() {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, rx) = tokio::sync::mpsc::channel(4);
    for (input, output) in [(7_u64, 3_u64), (11, 6)] {
        tx.send(ChildAgentResult {
            agent_id: Uuid::new_v4(),
            agent_role: "spawn/worker".to_string(),
            succeeded: true,
            formatted_message: "child done".to_string(),
            error: None,
            stop: None,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
            subtree_usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
        })
        .await
        .expect("send buffered result");
    }
    drop(tx);

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("step completes");

    let AgentStepResult::Completed {
        usage,
        children_usage,
        ..
    } = result
    else {
        panic!("expected Completed");
    };
    assert_eq!(usage.input_tokens, 10, "own usage is own calls only");
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(
        children_usage.input_tokens, 18,
        "both buffered subtrees fold exactly once: 7 + 11",
    );
    assert_eq!(children_usage.output_tokens, 9, "3 + 6");
}

/// Child results that arrive while a parent is executing tools must
/// be injected before the next provider request, not held until the
/// parent reaches a would-stop boundary.
#[tokio::test]
async fn child_results_arriving_during_tool_iteration_reach_next_request() {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let provider = MockProvider::new(vec![
        vec![
            tool_call_delta("tc1", Some("send_child_result"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "send_child_result".to_string(),
        Box::new(move |_| {
            tx.try_send(ChildAgentResult {
                agent_id: Uuid::new_v4(),
                agent_role: "spawn/worker".to_string(),
                succeeded: true,
                formatted_message: "child finished during tool batch".to_string(),
                error: None,
                stop: None,
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                },
                subtree_usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                },
            })
            .expect("child result send");
            Ok(serde_json::json!({ "queued_child_result": true }))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "send_child_result".to_string(),
        description: "queue a child result".to_string(),
        parameters: serde_json::json!({}),
    }];

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &tools,
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let AgentStepResult::Completed { children_usage, .. } = result else {
        panic!("expected Completed");
    };
    assert_eq!(children_usage.input_tokens, 7);
    assert_eq!(children_usage.output_tokens, 3);

    let requests = provider.requests().expect("requests");
    assert_eq!(requests.len(), 2, "tool result should force a second turn");
    let second_request_text = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("<agent_result from=\"spawn/worker\"")
            && second_request_text.contains("child finished during tool batch"),
        "second request must include the prompt child result: {second_request_text}",
    );
}

/// REVIEW W3.6 HIGH-1 regression: every step's `children_usage`
/// covers ONLY the results delivered into that step. A reused
/// `LoopContext` (interactive sessions run many steps over one
/// context) must not carry step 1's children into step 2's
/// snapshot — pre-fix, the accumulator was monotonic for the
/// context's lifetime and did exactly that.
#[tokio::test]
async fn reused_loop_context_reports_each_steps_children_only() {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let child_result = |input: u64, output: u64| ChildAgentResult {
        agent_id: Uuid::new_v4(),
        agent_role: "spawn/worker".to_string(),
        succeeded: true,
        formatted_message: "child done".to_string(),
        error: None,
        stop: None,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
        subtree_usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
    };

    let provider = MockProvider::new(vec![
        vec![text_delta("turn one"), done_event(StopReason::EndTurn)],
        vec![text_delta("turn two"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    tx.send(child_result(7, 3)).await.expect("send step 1");
    let step_one = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "first",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("step one completes");
    let AgentStepResult::Completed { children_usage, .. } = step_one else {
        panic!("expected Completed");
    };
    assert_eq!(children_usage.input_tokens, 7, "step 1 sees its child");

    tx.send(child_result(11, 6)).await.expect("send step 2");
    let step_two = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "second",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("step two completes");
    let AgentStepResult::Completed { children_usage, .. } = step_two else {
        panic!("expected Completed");
    };
    assert_eq!(
        children_usage.input_tokens, 11,
        "step 2 reports ONLY step 2's delivery — 18 here means \
         step 1's child leaked across the reset boundary",
    );
    assert_eq!(children_usage.output_tokens, 6);
}

// -- Post-batch steer drains in the schema arms ------------------------

/// Handlers that deliver an inbound message *while the tool batch is
/// executing*, so the drain under test is the post-batch one (the
/// step-start flush has already run by then).
fn handlers_sending_inbound(
    tx: crate::r#loop::inbound::InboundSender,
    content: &'static str,
    kind: crate::r#loop::inbound::MessageKind,
) -> std::collections::HashMap<String, ToolHandler> {
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(move |_| {
            tx.try_send(make_channel_message("mid-batch-sender", content, kind, 0))
                .expect("mid-batch send fits the buffer");
            Ok(serde_json::json!({"content": "file data"}))
        }),
    );
    handlers
}

fn request_carries_frame(request: &ProviderRequest, needle: &str) -> bool {
    request.messages.iter().any(|m| {
        m.content
            .as_deref()
            .is_some_and(|c| c.contains("<agent_message") && c.contains(needle))
    })
}

/// Regression (steer drain missing from the `SchemaInvalid` arm): a
/// steer arriving during the pre-schema tool batch of a failed
/// validation attempt must be injected before the retry's provider
/// request — "immediately after the current tool batch" — not parked
/// until some later boundary.
#[tokio::test]
async fn steer_during_schema_invalid_batch_reaches_the_retry_request() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"wrong":1}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema2",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let schema = simple_schema();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "steer during invalid batch",
        crate::r#loop::inbound::MessageKind::Steer,
    ));

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 2);
    assert!(
        request_carries_frame(&requests[1], "steer during invalid batch"),
        "the schema-retry request must already carry the framed steer",
    );
}

/// Contract pin for the `ToolsAndSchemaValid` arm: a steer arriving
/// during the pre-schema batch is injected once every call has its
/// result, and the loop continues so the next provider request sees
/// it (the model must never stop past an undelivered steer).
#[tokio::test]
async fn steer_during_tools_and_schema_valid_batch_reaches_the_model() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"early"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema2",
            Some("structured_output"),
            r#"{"answer":"final"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let schema = simple_schema();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "steer during valid batch",
        crate::r#loop::inbound::MessageKind::Steer,
    ));

    let result = run_step_full(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        None,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(
        output["answer"], "final",
        "the loop must continue past the steer instead of returning the pre-steer output",
    );

    let requests = provider.requests().expect("requests recorded");
    assert_eq!(requests.len(), 2);
    assert!(
        request_carries_frame(&requests[1], "steer during valid batch"),
        "the continuation request must carry the framed steer",
    );
}

// -- Undelivered follow-up (Update) re-queue on abnormal exits ---------

fn requeue_loop_ctx(
    agent_id: uuid::Uuid,
) -> (
    LoopContext,
    std::sync::Arc<crate::agent::PendingAgentMessages>,
) {
    let pending = std::sync::Arc::new(crate::agent::PendingAgentMessages::new());
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.agent_id = Some(agent_id);
    loop_ctx.pending_agent_messages = Some(std::sync::Arc::clone(&pending));
    (loop_ctx, pending)
}

#[track_caller]
fn assert_update_requeued(
    pending: &crate::agent::PendingAgentMessages,
    agent_id: uuid::Uuid,
    store: &EventStore,
    content: &str,
) {
    let queued = pending.messages_for_delivery(agent_id);
    assert_eq!(
        queued.len(),
        1,
        "the undelivered Update must be re-queued for this agent",
    );
    assert_eq!(queued[0].content, content);
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(
        audited,
        "the re-queue must append an agent_message.queued audit event"
    );
}

/// Regression (follow-up buffer dropped on abnormal exits): an Update
/// drained mid-step buffers for the next stop boundary; a
/// `MaxIterations` exit never reaches one, so the message must land in
/// the durable pending store instead of vanishing.
#[tokio::test]
async fn max_iterations_exit_requeues_buffered_updates() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1]);
    let store = EventStore::new();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi from mid-batch",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);
    let config = AgentLoopConfig {
        max_iterations: Some(1),
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    assert!(matches!(
        result,
        AgentStepResult::MaxIterationsReached { .. }
    ));
    assert_update_requeued(&pending, agent_id, &store, "fyi from mid-batch");
}

/// Regression: a hard provider error (here an in-band typed Error
/// event ending the second turn) propagates as the step's `Err` —
/// and the Update buffered during turn 1's batch must still be
/// re-queued durably, not dropped with the failed future.
#[tokio::test]
async fn provider_error_exit_requeues_buffered_updates() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![ProviderEvent::Error {
        error: ProviderError::QuotaExceeded,
    }];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi before the crash",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);

    let err = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect_err("the typed in-band provider error fails the step");
    assert!(
        matches!(err, NornError::Provider(ProviderError::QuotaExceeded)),
        "the step must surface the provider's typed error, got {err:?}",
    );
    assert_update_requeued(&pending, agent_id, &store, "fyi before the crash");
}

/// Regression: a `step_timeout` cut drops the inner future wherever
/// it is suspended; the Update buffered before the cut must survive
/// into the durable pending store.
#[tokio::test(start_paused = true)]
async fn step_timeout_exit_requeues_buffered_updates() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        text_delta("never observed"),
        done_event(StopReason::EndTurn),
    ];
    // 100ms before each turn's first event: turn 1 completes inside
    // the 150ms budget, turn 2 cannot.
    let provider = DelayedProvider::new(vec![turn1, turn2], Duration::from_millis(100));
    let store = EventStore::new();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi before the timeout",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(150)),
        ..AgentLoopConfig::default()
    };

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("a timed-out step returns Ok(TimedOut)");
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));
    assert_update_requeued(&pending, agent_id, &store, "fyi before the timeout");
}

/// Provider that pushes a message into the recipient's own inbound
/// channel the moment it is called, then fails the turn with a typed
/// in-band error — modelling a send the channel accepted (and whose
/// sender was told `delivered: true`) after the loop's final inbound
/// drain: the deregistration message-loss window.
struct SendThenFailProvider {
    tx: crate::r#loop::inbound::InboundSender,
    content: &'static str,
    kind: crate::r#loop::inbound::MessageKind,
}

impl Provider for SendThenFailProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.tx
            .try_send(make_channel_message(
                "late-sender",
                self.content,
                self.kind,
                0,
            ))
            .expect("test channel has capacity");
        Ok(Box::pin(stream::iter(vec![Ok(ProviderEvent::Error {
            error: ProviderError::QuotaExceeded,
        })])))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// Regression (deregistration message-loss window): a message the
/// inbound channel accepted after the loop's final drain — pushed
/// here during the failing provider call, so no sweep ever ran after
/// it — must be re-queued into the durable pending store at step
/// exit, where the next step's flush and `wake_agent` eligibility
/// both see it. The message kind survives the round trip.
#[tokio::test]
async fn exit_sweeps_undrained_channel_messages_into_pending_store() {
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let provider = SendThenFailProvider {
        tx,
        content: "steer accepted mid-call",
        kind: crate::r#loop::inbound::MessageKind::Steer,
    };
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);

    let err = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect_err("the in-band provider error fails the step");
    assert!(
        matches!(err, NornError::Provider(ProviderError::QuotaExceeded)),
        "the step surfaces the typed provider error, got {err:?}",
    );

    let queued = pending.messages_for_delivery(agent_id);
    assert_eq!(
        queued.len(),
        1,
        "the undrained channel message must be re-queued durably",
    );
    assert_eq!(queued[0].content, "steer accepted mid-call");
    assert_eq!(queued[0].kind, crate::r#loop::inbound::MessageKind::Steer);
    assert_eq!(
        queued[0].to_id, agent_id,
        "redelivery is re-targeted to this loop's agent",
    );
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(
        audited,
        "the sweep must append an agent_message.queued audit event",
    );
}

/// A step that ends through a stop boundary has already flushed its
/// follow-ups into the conversation; nothing may be re-queued.
#[tokio::test]
async fn boundary_exit_leaves_nothing_to_requeue() {
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ];
    // The boundary flush injects the buffered update and continues.
    let turn2 = vec![text_delta("done"), done_event(StopReason::EndTurn)];
    let turn3 = vec![text_delta("final"), done_event(StopReason::EndTurn)];
    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let executor = MockToolExecutor::new(handlers_sending_inbound(
        tx,
        "fyi delivered at stop",
        crate::r#loop::inbound::MessageKind::Update,
    ));
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_file_tool_def()],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: Some(&mut rx),
        },
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output, Value::String("final".to_string()));
    assert!(
        pending.is_empty(),
        "a boundary-delivered follow-up must not be re-queued",
    );
    let delivered = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::UserMessage { content, .. }
                if content.contains("fyi delivered at stop"),
        )
    });
    assert!(
        delivered,
        "the boundary flush must have injected the update"
    );
}

/// Regression (partial inbound-injection failure dropped acknowledged
/// messages): when the persistence sink fails midway through injecting a
/// drained steer batch, the failing message and every not-yet-injected
/// message after it must be re-queued into the durable pending store on
/// the step-exit sweep — not dropped inside the moved batch. The
/// successfully-injected prefix stays delivered and is never re-queued.
#[tokio::test]
async fn inbound_injection_sink_failure_requeues_undelivered_remainder() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::error::SessionError;
    use crate::session::persistence::SessionPersistError;
    use crate::session::store::PersistenceSink;

    // Fails the append of any `UserMessage` whose framed content carries
    // the marker; every other event persists normally.
    struct FailOnMarkerSink {
        marker: &'static str,
        tripped: Arc<AtomicBool>,
    }
    impl PersistenceSink for FailOnMarkerSink {
        fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
            if let SessionEvent::UserMessage { content, .. } = event
                && content.contains(self.marker)
            {
                self.tripped.store(true, Ordering::SeqCst);
                return Err(SessionPersistError::Io(std::io::Error::other(
                    "sink refused the second steer",
                )));
            }
            Ok(())
        }
    }

    // read_file sends three steers mid-batch; sorted by (no-seq, timestamp)
    // they keep send order, so the sink lets steer-1 through and fails
    // steer-2, leaving steer-3 never attempted.
    let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(move |_| {
            for marker in ["steer-1", "steer-2", "steer-3"] {
                tx.try_send(make_channel_message(
                    "mid-batch-sender",
                    marker,
                    crate::r#loop::inbound::MessageKind::Steer,
                    0,
                ))
                .expect("mid-batch send fits the buffer");
            }
            Ok(serde_json::json!({"content": "file data"}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let tripped = Arc::new(AtomicBool::new(false));
    let store = EventStore::with_sink(Box::new(FailOnMarkerSink {
        marker: "steer-2",
        tripped: Arc::clone(&tripped),
    }));

    let provider = MockProvider::new(vec![vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path":"f"}"#),
        done_event(StopReason::ToolUse),
    ]]);
    let agent_id = uuid::Uuid::new_v4();
    let (mut loop_ctx, pending) = requeue_loop_ctx(agent_id);

    let err = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[read_file_tool_def()],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect_err("the mid-injection sink failure fails the step");
    assert!(
        matches!(err, NornError::Session(SessionError::StorageError { .. })),
        "the step must surface the sink's storage error, got {err:?}",
    );
    assert!(
        tripped.load(Ordering::SeqCst),
        "the sink must actually have failed a steer append",
    );

    // steer-2 (the failed append) and steer-3 (never attempted) must both
    // be durably re-queued; steer-1 was delivered and must not re-appear.
    let queued = pending.messages_for_delivery(agent_id);
    let contents: std::collections::BTreeSet<String> =
        queued.iter().map(|m| m.content.clone()).collect();
    let expected: std::collections::BTreeSet<String> =
        ["steer-2".to_string(), "steer-3".to_string()]
            .into_iter()
            .collect();
    assert_eq!(
        contents, expected,
        "the failed and un-injected steers must be re-queued, got {contents:?}",
    );
    let audited = store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. }
                if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE,
        )
    });
    assert!(
        audited,
        "the re-queue must append agent_message.queued audit events",
    );
}

/// Regression (fired Before-timing injection dropped on gate exit): a
/// Before-timing rule that fires in the final tool batch, when the step
/// then hits `max_iterations` before the next `build_request`, must still
/// leave its `SessionEvent::RuleInjection` audit event persisted — the
/// invariant that a fired rule is recorded regardless of delivery mode.
#[tokio::test]
async fn max_iterations_after_before_fire_persists_the_rule_injection() {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming};

    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    // Before-timing: fired at batch time, buffered for the next request
    // build that `max_iterations` prevents from ever running.
    let rule = Rule {
        id: RuleId::from("rs-before-rule"),
        name: "rs before".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::ContextInjection,
        timing: TriggerTiming::Before,
        body: "before-rule fired".to_string(),
        shell_source: None,
    };

    let provider = MockProvider::new(vec![vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        done_event(StopReason::ToolUse),
    ]]);
    let store = EventStore::new();
    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    let config = AgentLoopConfig {
        max_iterations: Some(1),
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[write_tool],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert!(
        matches!(result, AgentStepResult::MaxIterationsReached { .. }),
        "the step must hit the iteration cap right after the batch, got {result:?}",
    );

    let rule_events: Vec<(String, TriggerTiming)> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::RuleInjection {
                rule_id, timing, ..
            } => Some((rule_id, timing)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rule_events.len(),
        1,
        "the fired Before-timing rule must leave exactly one persisted \
         RuleInjection audit event, got {rule_events:?}",
    );
    assert_eq!(rule_events[0].0, "rs-before-rule");
    assert!(
        matches!(rule_events[0].1, TriggerTiming::Before),
        "the persisted injection keeps its Before timing",
    );
}

/// F2 regression (fired Before-timing injection dropped on the step-timeout
/// drop path): a Before-timing rule that fires in a tool batch reaching a
/// completion boundary, where the `step_timeout` then elapses during the
/// linger await before `StepMachine::run` can return, must still leave its
/// `SessionEvent::RuleInjection` audit event persisted. The timeout drops the
/// inner future — so the run-exit persist never runs — and only the buffer
/// hoisted into `run_agent_step_common` (persisted there) keeps the firing on
/// the record. Before the fix the buffer lived inside the dropped future and
/// the fired rule vanished without an audit event.
///
/// Determinism (paused clock): a `ToolsAndSchemaValid` response fires the
/// Before rule in its pre-schema batch and heads straight to the stop
/// boundary (no second `build_request` ever consumes the buffer). A long
/// linger deadline holds the step *inside* the inner future, so the short
/// `step_timeout` reliably cuts it post-fire.
#[tokio::test(start_paused = true)]
async fn step_timeout_after_before_fire_persists_the_rule_injection() {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming};

    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    // Before-timing: fired at batch time, buffered for a build_request the
    // step-timeout drop path prevents from ever running.
    let rule = Rule {
        id: RuleId::from("rs-before-rule"),
        name: "rs before".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::ContextInjection,
        timing: TriggerTiming::Before,
        body: "before-rule fired".to_string(),
        shell_source: None,
    };

    // Pre-schema write (fires the Before rule) + a valid schema call in one
    // response → ToolsAndSchemaValid → run the batch, accept the schema, head
    // to the stop boundary. The 10ms first-event delay lands inside the 100ms
    // budget; the boundary's 10s linger then holds the step until the budget
    // elapses.
    let turn = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = DelayedProvider::new(vec![turn], Duration::from_millis(10));
    let store = EventStore::new();
    let schema = simple_schema();
    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_millis(100)),
        linger: Some(crate::r#loop::linger::LingerPolicy {
            deadline: Duration::from_secs(10),
        }),
        ..AgentLoopConfig::default()
    };

    // A never-triggered cancel token keeps the linger wake set non-empty so
    // it actually sleeps toward its deadline (an empty wake set short-circuits
    // to expire immediately). The 100ms budget then cuts the linger sleep,
    // dropping the inner future with the fired Before injection still buffered.
    let cancel = CancellationToken::new();
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[write_tool],
        output_schema: Some(&schema),
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: Some(cancel),
    })
    .await
    .expect("a timed-out step returns Ok(TimedOut)");
    assert!(
        matches!(result, AgentStepResult::TimedOut { .. }),
        "the linger await must be cut by the step timeout, got {result:?}",
    );

    let rule_events: Vec<(String, TriggerTiming)> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::RuleInjection {
                rule_id, timing, ..
            } => Some((rule_id, timing)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rule_events.len(),
        1,
        "the fired Before-timing rule must leave exactly one persisted \
         RuleInjection audit event even on the timeout drop path, got {rule_events:?}",
    );
    assert_eq!(rule_events[0].0, "rs-before-rule");
    assert!(
        matches!(rule_events[0].1, TriggerTiming::Before),
        "the persisted injection keeps its Before timing",
    );
}

// -- Usage-floor anchor (owner incident 2026-07) ------------------------

/// After a successful provider call the loop records the provider-reported
/// spend (`input + output`) as the usage floor on the `ContextEdits`
/// tracker, so the next preflight anchors its token warning and compaction
/// trigger on `max(estimate, floor)`.
#[tokio::test]
async fn provider_step_records_usage_floor_from_reported_usage() {
    let store = EventStore::new();
    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let config = AgentLoopConfig::default();
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    // `done_event` reports input 10 + output 5.
    assert_eq!(
        loop_ctx
            .context_edits
            .as_ref()
            .and_then(crate::session::context_edit::ContextEdits::usage_floor),
        Some(15),
        "the step must record the provider-reported spend as the usage floor",
    );
}

/// The advisory `loop.token_warning` anchors on `max(estimate, floor)` and
/// its payload carries all three numbers (`estimated`, `usage_floor`,
/// `effective`) plus the limit, for observability. Here the estimate alone
/// is far below the limit — only the floor pushes the effective count over.
#[tokio::test]
async fn token_warning_fires_on_the_usage_floor_and_carries_all_numbers() {
    let store = EventStore::new();
    let provider = MockProvider::new(vec![vec![
        text_delta("ok"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    let mut edits = crate::session::context_edit::ContextEdits::new();
    edits.set_usage_floor(9_999);
    loop_ctx.context_edits = Some(edits);
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    // Small window; the reserve trigger is disabled so only the advisory
    // warning path is exercised.
    let config = AgentLoopConfig {
        context_window_limit: Some(5_000),
        auto_compact_reserve_tokens: None,
        ..AgentLoopConfig::default()
    };
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let warnings: Vec<Value> = store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "loop.token_warning" => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one loop.token_warning expected, got {warnings:?}",
    );
    let data = &warnings[0];
    let estimated = data["estimated"].as_u64().expect("estimated present");
    assert!(
        estimated < 5_000,
        "the estimate alone must be under the limit (got {estimated}) — \
         the warning fired on the floor",
    );
    assert_eq!(data["usage_floor"].as_u64(), Some(9_999));
    assert_eq!(data["effective"].as_u64(), Some(9_999));
    assert_eq!(data["limit"].as_u64(), Some(5_000));
}
