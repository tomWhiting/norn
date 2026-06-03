//! Internal helpers for the agent loop: initial message building, inbound
//! message injection, schema-flow helpers, iteration-signal handling, and
//! event-store append+notify.
//!
//! Tool execution pipeline functions live in the sibling
//! [`super::tool_dispatch`] module and are re-exported here so that
//! `runner.rs` keeps its existing single-import block.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::Value;

use crate::error::{NornError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::commands::{PreprocessResult, preprocess_input};
use crate::r#loop::config::{AgentLoopConfig, ToolExecutor};
use crate::r#loop::context::construct_prompt;
use crate::r#loop::conversation_state::{ResponseThreadAnchor, latest_response_anchor};
use crate::r#loop::inbound::{ChannelMessage, DeliveryMode, InboundChannel};
use crate::r#loop::iteration::{IterationSignal, format_handoff_message};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::notifications::ToolBatchNotificationHook;
use crate::provider::request::{Message, MessageRole};
use crate::provider::traits::Provider;
use crate::rules::types::RuleInjection;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

// Re-export tool-dispatch functions so runner.rs imports stay unchanged.
pub(super) use super::tool_dispatch::{append_tool_result, execute_tool_call};

pub(super) use crate::r#loop::rule_wiring::{
    apply_rule_injections, partition_injections_by_timing,
};

/// Initial prompt messages plus the index where the new user input starts.
pub(super) struct InitialMessages {
    /// Full local prompt view used for manual replay.
    pub(super) messages: Vec<Message>,
    /// End of the live system/developer prefix before persisted history.
    pub(super) prefix_len: usize,
    /// Latest provider response anchor visible in the prompt history.
    pub(super) response_thread_anchor: Option<ResponseThreadAnchor>,
}

/// Build the initial conversation: system prompt, conversation history
/// from the event store, and the new user input (or slash expansion).
///
/// History is read from `store` via [`events_to_messages`] and spliced
/// between the system message and the new user message. This ensures
/// the provider sees the full conversation on every turn.
///
/// When `loop_context.slash_commands` is `Some` and the input matches a
/// registered slash command, the expansion replaces the literal user
/// prompt. The original input is still recorded upstream as a
/// [`SessionEvent::UserMessage`] for audit.
///
/// # Errors
///
/// Propagates the error variant of a [`SlashCommandHandler::Custom`]
/// closure or a serialization failure in the
/// [`SlashCommandHandler::Tool`] expansion.
pub(super) fn build_initial_messages(
    user_prompt: &str,
    loop_context: &LoopContext,
    store: &EventStore,
) -> Result<InitialMessages, NornError> {
    let slash_expansion: Option<Vec<Message>> = match loop_context.slash_commands.as_ref() {
        Some(registry) => match preprocess_input(user_prompt, registry)? {
            PreprocessResult::Expanded { messages } => Some(messages),
            PreprocessResult::Passthrough(_) => None,
        },
        None => None,
    };

    let new_msg_count = slash_expansion.as_ref().map_or(1, Vec::len);

    let (history_events, include_compactions) =
        if let Some(edits) = loop_context.context_edits.as_ref() {
            let view = construct_prompt(store, edits);
            (view.events, true)
        } else {
            (store.events(), false)
        };
    let mut messages = Vec::with_capacity(2 + history_events.len() + new_msg_count);
    messages.push(Message {
        role: MessageRole::System,
        content: Some(loop_context.base_system_instruction()),
        thinking: String::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    if let Some(dynamic) = loop_context.dynamic_context() {
        messages.push(Message {
            role: MessageRole::Developer,
            content: Some(dynamic),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
    }

    let prefix_len = messages.len();
    let response_thread_anchor =
        latest_response_anchor(&history_events, prefix_len, include_compactions);

    let history = if include_compactions {
        crate::session::conversion::prompt_events_to_messages(&history_events)
    } else {
        crate::session::conversion::events_to_messages(&history_events)
    };

    messages.extend(history);

    if let Some(expansion) = slash_expansion {
        messages.extend(expansion);
    } else {
        messages.push(Message {
            role: MessageRole::User,
            content: Some(user_prompt.to_string()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
    }

    Ok(InitialMessages {
        messages,
        prefix_len,
        response_thread_anchor,
    })
}

/// Append a session event and (if hooks are registered) fire all
/// session-event hooks.
///
/// Returns the new event ID. Hook failures cannot be surfaced because
/// `SessionEventHook::on_event` returns no result; hooks observing the
/// stored event treat the original event slice as authoritative.
pub(super) async fn append_and_notify(
    store: &EventStore,
    event: SessionEvent,
    hooks: Option<&HookRegistry>,
) -> Result<EventId, SessionError> {
    let id = store.append(event.clone())?;
    if let Some(reg) = hooks {
        reg.run_on_event(&event).await;
    }
    Ok(id)
}

/// Drain the inbound channel (if present) and partition messages by
/// delivery mode. Returns `(steer, follow_up)`.
pub(super) fn drain_and_partition(
    inbound: Option<&mut InboundChannel>,
) -> (Vec<ChannelMessage>, Vec<ChannelMessage>) {
    let Some(ch) = inbound else {
        return (Vec::new(), Vec::new());
    };
    let drained = ch.drain();
    let mut steer = Vec::new();
    let mut follow_up = Vec::new();
    for msg in drained {
        match msg.delivery {
            DeliveryMode::Steer => steer.push(msg),
            DeliveryMode::FollowUp => follow_up.push(msg),
        }
    }
    (steer, follow_up)
}

/// Inject inbound messages as user-role messages into both the event store
/// and the local conversation. Messages are sorted by timestamp ascending
/// before injection so multiple messages drained together appear in order.
///
/// `label` is the attribution prefix (e.g. `"Inbound"` for steer messages,
/// `"Follow-up"` for buffered follow-ups).
pub(super) async fn inject_inbound_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    mut msgs: Vec<ChannelMessage>,
    label: &str,
    hooks: Option<&HookRegistry>,
) -> Result<(), SessionError> {
    if msgs.is_empty() {
        return Ok(());
    }
    msgs.sort_by_key(|m| m.timestamp);
    for msg in msgs {
        let formatted = format!("[{label} from {}]: {}", msg.author, msg.content);
        append_and_notify(
            store,
            SessionEvent::UserMessage {
                base: EventBase::new(store.last_event_id()),
                content: formatted.clone(),
            },
            hooks,
        )
        .await?;
        messages.push(Message {
            role: MessageRole::User,
            content: Some(formatted),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
    }
    Ok(())
}

/// Append an "accepted" tool result for the schema tool call so the
/// conversation remains well-formed when the loop continues past a stop
/// point.
pub(super) async fn accept_schema_tool_call(
    store: &EventStore,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    schema_tool_name: &str,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&crate::provider::agent_event::AgentEventSender>,
) -> Result<(), SessionError> {
    if let Some(idx) = response
        .tool_calls
        .iter()
        .position(|tc| tc.name == schema_tool_name)
    {
        let schema_tc = &response.tool_calls[idx];
        append_tool_result(
            store,
            messages,
            &schema_tc.call_id,
            schema_tool_name,
            schema_tc.kind,
            &Value::String("accepted".to_string()),
            0,
            hooks,
            event_tx,
        )
        .await?;
    }
    Ok(())
}

/// Reject tool calls that appear after the schema tool by appending error
/// results without executing them.
pub(super) async fn reject_post_schema_tools(
    store: &EventStore,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    schema_tool_name: &str,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&crate::provider::agent_event::AgentEventSender>,
) -> Result<(), SessionError> {
    let schema_idx = response
        .tool_calls
        .iter()
        .position(|tc| tc.name == schema_tool_name);

    if let Some(idx) = schema_idx {
        let schema_tc = &response.tool_calls[idx];
        append_tool_result(
            store,
            messages,
            &schema_tc.call_id,
            schema_tool_name,
            schema_tc.kind,
            &Value::String("accepted".to_string()),
            0,
            hooks,
            event_tx,
        )
        .await?;

        for i in (idx + 1)..response.tool_calls.len() {
            let tc = &response.tool_calls[i];
            let rejection = serde_json::json!({
                "error": format!(
                    "rejected: {} tool is terminal; tools after it are not executed",
                    schema_tool_name,
                )
            });
            append_tool_result(
                store,
                messages,
                &tc.call_id,
                &tc.name,
                tc.kind,
                &rejection,
                0,
                hooks,
                event_tx,
            )
            .await?;
        }
    }
    Ok(())
}

/// Translate iteration-monitor signals into session events and (for
/// `HandoffTriggered`) a wrap-up user-role message appended to the running
/// conversation.
pub(super) async fn handle_iteration_signals(
    store: &EventStore,
    messages: &mut Vec<Message>,
    signals: Vec<IterationSignal>,
    hooks: Option<&HookRegistry>,
) -> Result<(), NornError> {
    for signal in signals {
        match signal {
            IterationSignal::None => {}
            IterationSignal::TokenWarning { used, limit, pct } => {
                append_and_notify(
                    store,
                    SessionEvent::Custom {
                        base: EventBase::new(store.last_event_id()),
                        event_type: "iteration.token_warning".to_string(),
                        data: serde_json::json!({
                            "used": used,
                            "limit": limit,
                            "pct": pct,
                        }),
                    },
                    hooks,
                )
                .await?;
            }
            IterationSignal::HandoffTriggered { .. } => {
                let text = format_handoff_message(&signal);
                append_and_notify(
                    store,
                    SessionEvent::UserMessage {
                        base: EventBase::new(store.last_event_id()),
                        content: text.clone(),
                    },
                    hooks,
                )
                .await?;
                messages.push(Message {
                    role: MessageRole::User,
                    content: Some(text),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                });
            }
            IterationSignal::RepeatedFailure {
                error_signature,
                consecutive_count,
            } => {
                append_and_notify(
                    store,
                    SessionEvent::Custom {
                        base: EventBase::new(store.last_event_id()),
                        event_type: "iteration.repeated_failure".to_string(),
                        data: serde_json::json!({
                            "error_signature": error_signature,
                            "consecutive_count": consecutive_count,
                        }),
                    },
                    hooks,
                )
                .await?;
            }
            IterationSignal::QualityWarning { signals } => {
                let serialised: Vec<serde_json::Value> = signals
                    .iter()
                    .map(|s| match s {
                        crate::r#loop::iteration::QualitySignal::Hedging {
                            matched_pattern,
                            text_excerpt,
                        } => serde_json::json!({
                            "kind": "hedging",
                            "matched_pattern": matched_pattern,
                            "text_excerpt": text_excerpt,
                        }),
                        crate::r#loop::iteration::QualitySignal::PrematureCompletion {
                            text_excerpt,
                        } => serde_json::json!({
                            "kind": "premature_completion",
                            "text_excerpt": text_excerpt,
                        }),
                    })
                    .collect();
                append_and_notify(
                    store,
                    SessionEvent::Custom {
                        base: EventBase::new(store.last_event_id()),
                        event_type: "iteration.quality_warning".to_string(),
                        data: serde_json::Value::Array(serialised),
                    },
                    hooks,
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Execute a batch of tool calls by index, partition the resulting rule
/// injections into "before" and "after" timing groups, apply the "after"
/// injections immediately, and return the "before" injections for the
/// caller to apply at the top of the next iteration.
pub(super) async fn inject_post_tool_batch_notifications(executor: &dyn ToolExecutor, skip: bool) {
    if skip {
        return;
    }
    let Some(shared_context) = executor.shared_context() else {
        return;
    };
    let Some(hook) = shared_context.get_extension::<ToolBatchNotificationHook>() else {
        return;
    };
    hook.inject_after_tool_batch().await;
}

pub(super) struct ToolBatchRequest<'a> {
    pub(super) provider: Option<Arc<dyn Provider>>,
    pub(super) executor: &'a dyn ToolExecutor,
    pub(super) store: &'a EventStore,
    pub(super) messages: &'a mut Vec<Message>,
    pub(super) response: &'a AssembledResponse,
    pub(super) tool_indices: Vec<usize>,
    pub(super) config: &'a AgentLoopConfig,
    pub(super) loop_context: &'a mut LoopContext,
    pub(super) event_tx: Option<&'a crate::provider::agent_event::AgentEventSender>,
}

pub(super) async fn execute_tool_batch(
    request: ToolBatchRequest<'_>,
) -> Result<Vec<RuleInjection>, NornError> {
    let mut batch_injections = Vec::new();
    for tc_index in request.tool_indices {
        batch_injections.extend(
            execute_tool_call(
                request.provider.as_ref().map(Arc::clone),
                request.executor,
                request.store,
                request.messages,
                request.response,
                tc_index,
                request.config,
                request.loop_context,
                request.event_tx,
            )
            .await?,
        );
    }
    let (before, after) = partition_injections_by_timing(batch_injections);
    apply_rule_injections(request.loop_context, after, request.messages, request.store).await?;
    Ok(before)
}

/// Drain the inbound channel, then inject steer messages and any buffered
/// follow-ups into the conversation. Returns `true` if any messages were
/// injected (indicating the loop should continue rather than return).
pub(super) async fn flush_inbound_messages(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
    hooks: Option<&HookRegistry>,
) -> Result<bool, SessionError> {
    let (steer, follow_up) = drain_and_partition(inbound);
    follow_up_buffer.extend(follow_up);

    if steer.is_empty() && follow_up_buffer.is_empty() {
        return Ok(false);
    }

    inject_inbound_messages(store, messages, steer, "Inbound", hooks).await?;
    let buffered = std::mem::take(follow_up_buffer);
    inject_inbound_messages(store, messages, buffered, "Follow-up", hooks).await?;
    Ok(true)
}

/// Drain pending child-agent results and inject them as developer
/// messages. Returns `true` if any results were injected.
///
/// Child results are formatted as structured developer context so the
/// model sees them as runtime information, not user input. Each result
/// is persisted as a `UserMessage` event (for the session record) but
/// sent to the provider with `MessageRole::Developer`.
pub(super) async fn drain_child_results(
    store: &EventStore,
    messages: &mut Vec<Message>,
    rx: &mut tokio::sync::mpsc::Receiver<crate::agent::result_channel::ChildAgentResult>,
    hooks: Option<&HookRegistry>,
) -> Result<bool, SessionError> {
    let mut batch = Vec::new();
    while let Ok(r) = rx.try_recv() {
        batch.push(r);
    }
    if batch.is_empty() {
        return Ok(false);
    }

    let formatted = if batch.len() == 1 {
        format!(
            "[Agent result from {}]: {}",
            batch[0].agent_role, batch[0].formatted_message,
        )
    } else {
        let mut out = format!("Results from {} completed agents:\n\n", batch.len());
        for r in &batch {
            let _ = write!(out, "--- {} ---\n{}\n\n", r.agent_role, r.formatted_message);
        }
        out
    };

    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: formatted.clone(),
        },
        hooks,
    )
    .await?;
    messages.push(Message {
        role: MessageRole::User,
        content: Some(formatted),
        thinking: String::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(true)
}

/// Ensure every tool call in the last `AssistantMessage` has a matching
/// `ToolResult` in the store. Appends synthetic cancelled results for
/// any that are missing.
///
/// Called after `run_agent_step` returns (all exit paths) and after
/// external cancellation (e.g. Ctrl+C drops the step future). This
/// guarantees the store is always in a valid state where no tool call
/// is orphaned — the provider never sees a tool call without a result.
pub async fn ensure_tool_results_complete(store: &EventStore) {
    let events = store.events();

    let last_assistant = events.iter().rposition(|e| {
        matches!(e, SessionEvent::AssistantMessage { tool_calls, .. } if !tool_calls.is_empty())
    });
    let Some(assistant_idx) = last_assistant else {
        return;
    };
    let SessionEvent::AssistantMessage { tool_calls, .. } = &events[assistant_idx] else {
        return;
    };
    if tool_calls.is_empty() {
        return;
    }

    let results_after: Vec<&str> = events[assistant_idx..]
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();

    for tc in tool_calls {
        if !results_after.contains(&tc.call_id.as_str()) {
            let event = SessionEvent::ToolResult {
                base: EventBase::new(store.last_event_id()),
                tool_call_id: tc.call_id.clone(),
                tool_name: tc.name.clone(),
                output: serde_json::json!({
                    "error": "execution cancelled before completion"
                }),
                duration_ms: 0,
            };
            if let Err(e) = store.append(event) {
                tracing::error!(
                    tool_call_id = %tc.call_id,
                    error = %e,
                    "failed to append cancelled tool result",
                );
            }
        }
    }
}
