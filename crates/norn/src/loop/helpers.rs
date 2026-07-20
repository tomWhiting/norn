//! Internal helpers for the agent loop: initial message building,
//! schema-flow helpers, iteration-signal handling, and event-store
//! append+notify.
//!
//! Tool execution pipeline functions live in the sibling
//! [`super::tool_dispatch`] module and are re-exported here so that
//! `runner.rs` keeps its existing single-import block. Inbound-message
//! and child-result injection live in [`super::delivery`].

use std::sync::Arc;

use serde_json::Value;

use crate::error::{NornError, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::commands::{PreprocessResult, preprocess_input};
use crate::r#loop::config::{AgentLoopConfig, ToolExecutor};
use crate::r#loop::context::construct_prompt;
use crate::r#loop::conversation_state::{
    ResponseThreadAnchor, latest_response_anchor_for_prompt_view,
};
use crate::r#loop::iteration::{IterationSignal, format_handoff_message};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::notifications::ToolBatchNotificationHook;
use crate::provider::request::{Message, MessageRole};
use crate::provider::traits::Provider;
use crate::rules::types::{RuleInjection, RuntimeEvent};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

// Re-export tool-dispatch functions so runner.rs imports stay unchanged.
pub(super) use super::tool_dispatch::{
    PlannedBatchRequest, ToolResultRecord, append_tool_result, execute_planned_tool_batch,
    installed_inline_char_limit,
};

pub(super) use crate::r#loop::rule_wiring::{
    apply_rule_injections, build_runtime_events, dedup_injections_by_rule,
    partition_injections_by_timing, persist_before_injection_audit,
};

/// Initial prompt messages plus the index where the new user input starts.
pub(super) struct InitialMessages {
    /// Full local prompt view used for manual replay.
    pub(super) messages: Vec<Message>,
    /// End of the live System prefix before persisted history. The managed
    /// dynamic-context Developer message is no longer part of the prefix — it
    /// is attached at the tail on the first `build_request` — so this is
    /// exactly the System message (`1`).
    pub(super) prefix_len: usize,
    /// Latest provider response anchor visible in the prompt history.
    pub(super) response_thread_anchor: Option<ResponseThreadAnchor>,
    /// Number of trailing messages the new user input occupies: 1 for a
    /// literal prompt, N for a slash-command expansion. Used by in-flight
    /// compaction to map the persisted prompt event onto its local message
    /// span (REVIEW 6b).
    pub(super) new_input_len: usize,
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
    user_prompt: Option<&str>,
    loop_context: &LoopContext,
    store: &EventStore,
) -> Result<InitialMessages, NornError> {
    let slash_expansion: Option<Vec<Message>> = match loop_context.slash_commands.as_ref() {
        Some(registry) => match user_prompt {
            Some(prompt) => match preprocess_input(prompt, registry)? {
                PreprocessResult::Expanded { messages } => Some(messages),
                PreprocessResult::Passthrough(_) => None,
            },
            None => None,
        },
        None => None,
    };

    let new_msg_count = match (&slash_expansion, user_prompt) {
        (Some(expansion), _) => expansion.len(),
        (None, Some(_)) => 1,
        (None, None) => 0,
    };

    let (history_events, include_compactions) =
        if let Some(edits) = loop_context.context_edits.as_ref() {
            let view = construct_prompt(store, edits);
            (view.events, true)
        } else {
            (store.events(), false)
        };
    let mut messages = Vec::with_capacity(1 + history_events.len() + new_msg_count);
    messages.push(Message {
        response_items: Vec::new(),
        role: MessageRole::System,
        content: Some(loop_context.base_system_instruction()),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
    });

    // The managed dynamic-context Developer message is NOT placed here: it is
    // attached at the tail by the first `build_request` so the System message
    // plus history form one stable, cacheable prefix. The prefix is therefore
    // exactly the System message.
    let prefix_len = messages.len();
    let response_thread_anchor = latest_response_anchor_for_prompt_view(
        &history_events,
        store,
        prefix_len,
        include_compactions,
    );

    let history = if include_compactions {
        crate::session::conversion::prompt_events_to_messages(&history_events)
    } else {
        crate::session::conversion::events_to_messages(&history_events)
    };

    messages.extend(history);

    let input_start = messages.len();
    if let Some(expansion) = slash_expansion {
        messages.extend(expansion);
    } else if let Some(prompt) = user_prompt {
        messages.push(Message {
            response_items: Vec::new(),
            role: MessageRole::User,
            content: Some(prompt.to_string()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        });
    }
    let new_input_len = messages.len() - input_start;

    Ok(InitialMessages {
        messages,
        prefix_len,
        response_thread_anchor,
        new_input_len,
    })
}

/// Append a session event without stalling other tasks on the executor
/// thread.
///
/// [`EventStore::append`] performs file I/O under the sink mutex — and
/// under `DurabilityPolicy::FsyncPerEvent` an fsync plus the index
/// critical section — so on a multi-thread runtime the append runs
/// inside [`tokio::task::block_in_place`]: the worker hands its task
/// queue back to the runtime before blocking, so no other task waits
/// behind the write. The calling task itself still waits, which is the
/// point — appends stay strictly ordered per session, and the `Result`
/// surfaces exactly as an inline append would. `spawn_blocking` (the
/// [`EventStore::checkpoint_off_executor`] shape) is not usable here
/// because the step API hands the loop `&EventStore`, not an owned
/// handle; `block_in_place` is the borrowed-data form of the same
/// offload.
///
/// On a current-thread runtime — where `block_in_place` panics by
/// contract and there are no sibling workers to protect — the append
/// runs inline, exactly the semantics that runtime already had.
pub(crate) fn append_off_executor(
    store: &EventStore,
    event: SessionEvent,
) -> Result<EventId, SessionError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| store.append(event))
        }
        _ => store.append(event),
    }
}

/// Append a session event and (if hooks are registered) fire all
/// session-event hooks.
///
/// The append itself goes through [`append_off_executor`] so sink I/O
/// never stalls unrelated tasks on the executor thread.
///
/// Returns the new event ID. Hook failures cannot be surfaced because
/// `SessionEventHook::on_event` returns no result; hooks observing the
/// stored event treat the original event slice as authoritative.
pub(super) async fn append_and_notify(
    store: &EventStore,
    event: SessionEvent,
    hooks: Option<&HookRegistry>,
) -> Result<EventId, SessionError> {
    let id = append_off_executor(store, event.clone())?;
    if let Some(reg) = hooks {
        reg.run_on_event(&event).await;
    }
    Ok(id)
}

/// Append an "accepted" tool result for the schema tool call so the
/// conversation remains well-formed when the loop continues past a stop
/// point.
///
/// `inline_char_limit` is the step's resolved model-facing inline limit
/// ([`installed_inline_char_limit`]); the constant acceptance string never
/// approaches it, but every persisted tool result carries the same
/// effective budget.
pub(super) async fn accept_schema_tool_call(
    store: &EventStore,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    schema_tool_name: &str,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&crate::provider::agent_event::AgentEventSender>,
    inline_char_limit: usize,
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
            ToolResultRecord {
                tool_call_id: &schema_tc.call_id,
                tool_name: schema_tool_name,
                kind: schema_tc.kind,
                caller: schema_tc.caller.clone(),
                output: &Value::String("accepted".to_string()),
                duration_ms: 0,
                inline_char_limit,
            },
            hooks,
            event_tx,
        )
        .await?;
    }
    Ok(())
}

/// Reject tool calls that appear after the schema tool by appending error
/// results without executing them.
///
/// The schema tool call itself is deliberately **not** answered here: the
/// caller appends exactly one result for it — an acceptance via
/// [`accept_schema_tool_call`] or validation feedback in the
/// `SchemaInvalid` arm. Appending a second result for the same `call_id`
/// (the pre-fix behaviour, REVIEW H1) produced a duplicate
/// `function_call_output` on the next request and permanently poisoned the
/// persisted session replay.
pub(super) async fn reject_post_schema_tools(
    store: &EventStore,
    messages: &mut Vec<Message>,
    response: &AssembledResponse,
    schema_tool_name: &str,
    hooks: Option<&HookRegistry>,
    event_tx: Option<&crate::provider::agent_event::AgentEventSender>,
    inline_char_limit: usize,
) -> Result<(), SessionError> {
    let schema_idx = response
        .tool_calls
        .iter()
        .position(|tc| tc.name == schema_tool_name);

    if let Some(idx) = schema_idx {
        for tc in &response.tool_calls[idx + 1..] {
            let rejection = serde_json::json!({
                "error": format!(
                    "rejected: {} tool is terminal; tools after it are not executed",
                    schema_tool_name,
                )
            });
            append_tool_result(
                store,
                messages,
                ToolResultRecord {
                    tool_call_id: &tc.call_id,
                    tool_name: &tc.name,
                    kind: tc.kind,
                    caller: tc.caller.clone(),
                    output: &rejection,
                    duration_ms: 0,
                    inline_char_limit,
                },
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
                    response_items: Vec::new(),
                    role: MessageRole::User,
                    content: Some(text),
                    thinking: String::new(),
                    reasoning: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                    tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
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
    // NX-004/NX-005: register nested NORN.md synthetic rules for every path
    // this batch is about to touch, before the engine evaluates the batch,
    // so a freshly-discovered nested rule fires on the same touch that
    // revealed it rather than one tool call later. Derived from the planned
    // calls' arguments (the same source `build_runtime_events` reads).
    if request.loop_context.nested_scanner.is_some() || request.loop_context.rules.is_some() {
        let mut touched_paths = Vec::new();
        for &idx in &request.tool_indices {
            if let Some(tc) = request.response.tool_calls.get(idx) {
                for event in build_runtime_events(&tc.name, &tc.arguments) {
                    if let RuntimeEvent::PathChanged { path, .. } = event {
                        touched_paths.push(path);
                    }
                }
            }
        }
        request.loop_context.scan_nested_norn(&touched_paths);
    }

    // N-007 R7: rebuild the presence set from the current prompt view so
    // `process_event` suppresses rules already in context and re-injects
    // only those evicted by compaction/context editing.
    request.loop_context.rebuild_rule_presence(request.store);

    // Dispatch the whole batch through the effect-based scheduling plan:
    // adjacent ReadOnly/Network calls overlap, Write/Process serialize, and
    // result ordering always matches call order.
    let batch_injections = execute_planned_tool_batch(PlannedBatchRequest {
        provider: request.provider,
        executor: request.executor,
        store: request.store,
        messages: request.messages,
        response: request.response,
        tool_indices: request.tool_indices,
        config: request.config,
        loop_context: request.loop_context,
        event_tx: request.event_tx,
    })
    .await?;
    let batch_injections = dedup_injections_by_rule(batch_injections);
    let (before, after) = partition_injections_by_timing(batch_injections);
    apply_rule_injections(request.loop_context, after, request.messages, request.store).await?;
    Ok(before)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
    use crate::r#loop::conversation_state::ConversationRequestState;
    use crate::provider::tools::ProviderCapabilities;
    use crate::session::context_edit::ContextEdits;
    use crate::session::events::{EventUsage, ProviderEpochBoundaryReason};
    use crate::session::persistence::SessionPersistError;
    use crate::session::store::PersistenceSink;

    fn user_event(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant_event(store: &EventStore, response_id: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(store.last_event_id()),
            response_items: Vec::new(),
            content: format!("answer from {response_id}"),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: Some(response_id.to_owned()),
        }
    }

    fn threaded_state(initial: InitialMessages) -> Result<ConversationRequestState, NornError> {
        Ok(ConversationRequestState::new(
            &AgentLoopConfig {
                conversation_state: ConversationStateMode::ProviderThreaded,
                ..AgentLoopConfig::default()
            },
            ProviderCapabilities::openai_responses(),
            initial.prefix_len,
            initial.response_thread_anchor,
        )?)
    }

    #[test]
    fn context_edited_prompt_respects_provider_epoch_boundaries() -> Result<(), NornError> {
        let store = EventStore::new();
        store.append(assistant_event(&store, "response_before_adoption"))?;
        store.append(SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(store.last_event_id()),
            reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
        })?;
        let mut loop_context = LoopContext::new("system");
        loop_context.context_edits = Some(ContextEdits::new());

        let initial = build_initial_messages(Some("next"), &loop_context, &store)?;
        let state = threaded_state(initial)?;
        assert!(state.previous_response_id().is_none());

        store.append(assistant_event(&store, "response_after_adoption"))?;
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: "queued after the new response".to_owned(),
        })?;
        let initial = build_initial_messages(Some("next"), &loop_context, &store)?;
        let messages = initial.messages.clone();
        let state = threaded_state(initial)?;
        assert_eq!(
            state.previous_response_id().as_deref(),
            Some("response_after_adoption"),
        );
        let request_messages = state.request_messages(&messages);
        assert_eq!(request_messages.len(), 3);
        assert_eq!(request_messages[0].role, MessageRole::System);
        assert_eq!(
            request_messages[1].content.as_deref(),
            Some("queued after the new response"),
        );
        assert_eq!(request_messages[2].content.as_deref(), Some("next"));
        Ok(())
    }

    /// A sink whose `persist` blocks until a handshake task — which can
    /// only run if the executor stays live while the write blocks —
    /// releases it.
    struct HandshakeSink {
        entered: Arc<AtomicBool>,
        release_rx: std::sync::mpsc::Receiver<()>,
    }

    impl PersistenceSink for HandshakeSink {
        fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
            self.entered.store(true, Ordering::SeqCst);
            self.release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|e| {
                    SessionPersistError::Io(std::io::Error::other(format!(
                        "executor stalled: no task released the blocking persist: {e}"
                    )))
                })?;
            Ok(())
        }
    }

    /// The off-executor guarantee: with exactly one worker thread, a
    /// blocking sink write inside `append_and_notify` must not stall the
    /// runtime. The sink blocks until a *separately spawned task* releases
    /// it — with an inline append on the single worker that task could
    /// never run and the handshake would time out; `block_in_place`
    /// hands the worker's queue back to the runtime, so the release task
    /// proceeds while the write blocks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn append_and_notify_does_not_stall_other_tasks_on_the_worker() {
        let entered = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = EventStore::with_sink(Box::new(HandshakeSink {
            entered: Arc::clone(&entered),
            release_rx,
        }));

        let entered_for_task = Arc::clone(&entered);
        let releaser = tokio::spawn(async move {
            while !entered_for_task.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            release_tx.send(()).expect("sink is waiting on the channel");
        });

        let id = append_and_notify(&store, user_event("hello"), None)
            .await
            .expect("append must succeed once the release task runs");
        assert_eq!(store.len(), 1);
        assert_eq!(store.last_event_id(), Some(id));
        releaser.await.expect("release task completes");
    }

    /// Error-surfacing parity: a failing sink persist comes back as the
    /// same typed `StorageError` the inline append produced, and the
    /// event is not in the in-memory store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn append_and_notify_surfaces_sink_errors_unchanged() {
        struct FailingSink;
        impl PersistenceSink for FailingSink {
            fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
                Err(SessionPersistError::Io(std::io::Error::other("disk full")))
            }
        }
        let store = EventStore::with_sink(Box::new(FailingSink));

        let err = append_and_notify(&store, user_event("lost"), None)
            .await
            .expect_err("sink failure must surface");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert!(store.is_empty(), "failed append must not reach memory");
    }

    /// The flavor gate: on a current-thread runtime `block_in_place`
    /// panics by contract, so the append must take the inline arm. A
    /// plain `#[tokio::test]` runs on the current-thread flavor — this
    /// test passing at all proves the gate.
    #[tokio::test]
    async fn append_and_notify_runs_inline_on_current_thread_runtime() {
        let store = EventStore::new();
        append_and_notify(&store, user_event("inline"), None)
            .await
            .expect("inline append on current-thread runtime");
        assert_eq!(store.len(), 1);
    }
}
