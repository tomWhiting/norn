//! Integration-style tests for the agent step runner.

use std::sync::atomic::{AtomicUsize, Ordering};
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
use crate::provider::response_item::{
    ResponseContentPart, ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

mod abnormal_stops;
mod cancellation;
mod children_usage;
mod d3_anchor_transitions;
mod d3_legacy_upgrade;
mod d3_provenance_validation;
mod d3_replay_guard;
mod d3_state;
mod explicit_continue;
mod failure_timeout;
mod follow_up;
mod hook_outcomes;
mod inbound_steering;
mod inbound_sweep;
mod iteration_monitor;
mod llm_event_hooks;
mod local_compaction;
mod managed_context;
mod pending_inbound;
mod persisted_compaction;
mod post_batch_steer;
mod provider_tool_surface;
mod refusal_matrix;
mod request_customization;
mod response_publication_timeout;
mod responses_replay_matrix;
mod rule_injection_persistence;
mod rule_tool_hooks;
mod schema_budget;
mod schema_tools;
mod stored_anchor_failure;
mod streaming_nudge;
mod truncation;
mod turn_basics;
mod update_requeue;
mod usage_floor;

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

fn refusal_delta(refusal: &str) -> ProviderEvent {
    ProviderEvent::RefusalDelta {
        item_id: "msg_refusal".to_owned(),
        output_index: 0,
        content_index: 0,
        refusal: refusal.to_owned(),
    }
}

fn completed_message_item(
    id: &str,
    content: &Value,
) -> Result<ProviderEvent, crate::provider::ResponseItemError> {
    let item = ResponseItem::from_value(serde_json::json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": "completed",
        "content": content,
    }))?;
    Ok(ProviderEvent::ResponseItemDone {
        item: ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance {
                item_id: Some(id.to_owned()),
                output_index: Some(0),
                content_index: None,
                sequence_number: Some(1),
            },
        },
    })
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

fn make_channel_message(
    author: &str,
    content: &str,
    kind: crate::r#loop::inbound::MessageKind,
    offset_secs: i64,
) -> crate::r#loop::inbound::ChannelMessage {
    let timestamp = chrono::Utc::now() + chrono::Duration::milliseconds(offset_secs);
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
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("mid-batch fixture could not enqueue its message: {error}"),
                })?;
            Ok(serde_json::json!({"content": "file data"}))
        }),
    );
    handlers
}

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
