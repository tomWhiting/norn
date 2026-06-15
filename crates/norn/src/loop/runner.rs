//! Agent loop runner: prompt-tool cycle with schema enforcement.
//!
//! This module contains the core agent loop that drives a single step of
//! agent execution. It sends prompts to a provider, collects streaming
//! events, executes tool calls, and enforces structured output schemas
//! with a bounded retry budget.
//!
//! Configuration types ([`AgentLoopConfig`], [`AgentStepResult`],
//! [`ToolExecutor`]) live in the sibling [`super::config`] module and are
//! re-exported here for backward compatibility.

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::{HookType, NornError, ProviderError, SchemaError};
use crate::integration::diagnostics::NornDiagnostic;
use crate::integration::hooks::{HookOutcome, LlmCallSummary};
use crate::r#loop::classify::{ResponseClass, call_provider, classify_response, record_truncation};
use crate::r#loop::compaction::CompactionState;
use crate::r#loop::conversation_state::ConversationRequestState;
use crate::r#loop::dev_context::ManagedDevMessage;
use crate::r#loop::expansion::{expand_system_instruction, expand_tool_descriptions};
use crate::r#loop::failure_tracking::collect_tool_failures;
use crate::r#loop::inbound::{ChannelMessage, InboundChannel};
use crate::r#loop::inflight_compaction::{
    InFlightPromptLayout, PreflightArgs, run_context_preflight,
};
use crate::r#loop::iteration::{IterationMonitorState, evaluate_iteration};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::retry_with_backoff;
use crate::r#loop::schema::{
    build_schema_tool, check_reserved_envelope_keys, format_nudge, format_validation_feedback,
};
use crate::provider::agent_event::AgentEventSender;
use crate::provider::events::StopReason;
use crate::provider::request::{
    AssistantToolCall, Message, MessageRole, ProviderRequest, ToolDefinition,
};
use crate::provider::surface::{ResolvedToolSurface, hosted_tools_prompt_section};
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::rules::types::RuleInjection;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;

use super::delivery::{drain_and_partition, drain_child_results, inject_inbound_messages};
use super::helpers::{
    ToolBatchRequest, ToolResultRecord, accept_schema_tool_call, append_and_notify,
    append_tool_result, apply_rule_injections, build_initial_messages,
    ensure_tool_results_complete, execute_tool_batch, handle_iteration_signals,
    inject_post_tool_batch_notifications, reject_post_schema_tools,
};
use super::linger::{BoundaryOutcome, StopBoundary, resolve_stop_boundary};

use crate::integration::hooks::HookRegistry;

pub use crate::r#loop::config::{AgentLoopConfig, AgentStepResult, ToolExecutor, TruncationKind};

/// On a [`HookOutcome::Block`] from `outcome`, inject the supplied
/// reason as a follow-up user message — both into the session event
/// store and into the live `messages` vec — so the next iteration
/// re-runs the model with the block reason in context. Returns `true`
/// when a Block was injected (caller must `continue` instead of
/// returning), `false` otherwise.
///
/// Threaded through every Completed-return path in
/// [`run_agent_step_inner`] so the literal
/// [`HookRegistry::run_stop`](crate::integration::hooks::HookRegistry::run_stop)
/// call appears once per return path (per `DESIGN.md` D5 wiring and
/// NH-006 R4 acceptance).
async fn inject_stop_block(
    outcome: HookOutcome,
    hooks: &HookRegistry,
    store: &EventStore,
    messages: &mut Vec<Message>,
) -> Result<bool, NornError> {
    let HookOutcome::Block { reason } = outcome else {
        return Ok(false);
    };
    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: reason.clone(),
        },
        Some(hooks),
    )
    .await?;
    messages.push(Message {
        role: MessageRole::User,
        content: Some(reason),
        thinking: String::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(true)
}
#[cfg(any(test, feature = "test-utils"))]
pub use crate::r#loop::config::{MockToolExecutor, ToolHandler};

/// Runs one complete agent loop step.
///
/// Sends the system instruction (assembled from `loop_context.system_sections`)
/// and user prompt to the provider, executes tool calls, enforces the output
/// schema (if provided), and returns the final result. Events are appended
/// to the store and optionally broadcast for real-time streaming.
///
/// Rules and hooks live on `loop_context` and fire at their respective
/// points: pre/post tool around each tool execution, pre/post LLM around
/// every provider call, and session-event hooks on every store append.
///
/// When `config.step_timeout` is set, the entire loop body is wrapped in
/// [`tokio::time::timeout`] and elapsing the budget produces
/// [`AgentStepResult::TimedOut`] with whatever partial output the model
/// produced before cancellation.
///
/// When `cancel` is `Some`, the loop participates in cooperative
/// cancellation: it is checked at the top of each iteration and raced
/// against the in-flight provider call. On cancellation the loop
/// returns [`AgentStepResult::Cancelled`] with usage accumulated so far;
/// any tool already executing finishes in full before this is returned
/// (cancellation is at the loop level, not inside tools). When `cancel`
/// is `None` the loop has no cancellation overhead.
///
/// # Errors
///
/// Returns [`NornError`] on provider failures, event store failures,
/// `PreLlmHook` blocks ([`NornError::HookBlocked`]), or unrecoverable tool
/// errors.
pub struct AgentStepRequest<'a> {
    /// The model provider that issues completion requests for this step.
    pub provider: &'a dyn Provider,
    /// Executes tool calls requested by the model during the step.
    pub executor: &'a dyn ToolExecutor,
    /// Event store that persists the conversation and usage events.
    pub store: &'a EventStore,
    /// The user prompt that opens this step.
    pub user_prompt: &'a str,
    /// Tool definitions advertised to the model for this step.
    pub tools: &'a [ToolDefinition],
    /// Optional JSON schema constraining the model's structured output.
    pub output_schema: Option<&'a Value>,
    /// Identifier of the model to invoke.
    pub model: &'a str,
    /// Loop configuration (timeouts, iteration limits, and hooks).
    pub config: &'a AgentLoopConfig,
    /// Optional sender for streaming agent events to observers.
    pub event_tx: Option<&'a AgentEventSender>,
    /// Optional channel for inbound messages delivered mid-step.
    pub inbound: Option<&'a mut InboundChannel>,
    /// Mutable per-loop context threaded through the step (diagnostics,
    /// compaction state, and accumulated metadata).
    pub loop_context: &'a mut LoopContext,
    /// Optional cancellation token; when triggered the loop returns
    /// [`AgentStepResult::Cancelled`] after the in-flight tool finishes.
    pub cancel: Option<CancellationToken>,
}

/// Drive a single agent step to completion.
///
/// Runs [`run_agent_step_inner`] under the configured step timeout (if any),
/// returning [`AgentStepResult::TimedOut`] with the partial progress captured
/// in the shared timeout state when the budget is exceeded. With no timeout
/// configured the inner loop runs directly.
///
/// # Errors
///
/// Propagates any [`NornError`] surfaced by the inner step loop. A
/// no-schema response truncated by the provider
/// (`MaxTokens`/`ContentFilter` with no tool calls) is **not** an error:
/// it returns [`AgentStepResult::Truncated`] carrying the partial text and
/// accumulated usage, with the full fragment and stop reason persisted on
/// the `AssistantMessage` event and the accompanying `loop.truncated`
/// Custom event.
pub async fn run_agent_step(request: AgentStepRequest<'_>) -> Result<AgentStepResult, NornError> {
    let timeout_state = crate::r#loop::compaction::shared_timeout_state();
    let started = std::time::Instant::now();
    let store = request.store;
    // Cheap-clone handle to the loop's children-usage accumulator,
    // captured before `request` moves into the inner future: when the
    // timeout fires, the inner future (and its `&mut LoopContext`) is
    // dropped, but the shared accumulator still reports every child
    // subtree folded before the budget elapsed (W3.6).
    let children_usage = request.loop_context.children_usage.clone();
    // Per-step contract (REVIEW W3.6 HIGH-1): every result arm reports
    // the children delivered into THIS step only. A reused LoopContext
    // (interactive surfaces run many steps over one context) would
    // otherwise leak turn 1's children into every later snapshot.
    children_usage.reset();
    let result = if let Some(budget) = request.config.step_timeout {
        let inner = run_agent_step_inner(request, Arc::clone(&timeout_state));
        if let Ok(result) = tokio::time::timeout(budget, inner).await {
            result
        } else {
            let snapshot = timeout_state.lock();
            Ok(AgentStepResult::TimedOut {
                elapsed: started.elapsed(),
                iterations: snapshot.iterations,
                partial_output: snapshot.last_assistant_text.clone().map(Value::String),
                usage: snapshot.usage.clone(),
                children_usage: children_usage.snapshot(),
            })
        }
    } else {
        run_agent_step_inner(request, timeout_state).await
    };

    ensure_tool_results_complete(store).await;
    result
}

async fn run_agent_step_inner(
    request: AgentStepRequest<'_>,
    timeout_state: crate::r#loop::compaction::SharedTimeoutState,
) -> Result<AgentStepResult, NornError> {
    let provider = request.provider;
    let executor = request.executor;
    let store = request.store;
    let user_prompt = request.user_prompt;
    let tools = request.tools;
    let output_schema = request.output_schema;
    let model = request.model;
    let config = request.config;
    let event_tx = request.event_tx;
    let mut inbound = request.inbound;
    let loop_context = request.loop_context;
    let cancel = request.cancel;
    let mut all_tools: Vec<ToolDefinition> = tools.to_vec();
    if let Some(schema) = output_schema {
        // Backstop for every schema source (embedder config, fork, rhai;
        // spawn_agent also rejects at its argument boundary): a schema
        // declaring a reserved envelope key would be unsatisfiable or
        // silently lossy after the pre-validation envelope split — refuse
        // it typed before the loop spends a single provider call.
        check_reserved_envelope_keys(schema).map_err(NornError::Schema)?;
        all_tools.push(build_schema_tool(&config.schema_tool_name, schema));
    }

    // NH-006 R3: UserPromptHook fires before the prompt enters the agent
    // loop, ahead of slash-command expansion and the initial UserMessage
    // session-event append. A Block returns immediately so the prompt is
    // never recorded and the loop never runs — the orchestrator sees the
    // typed `HookBlocked { hook_type: UserPrompt, .. }` error.
    if let Some(hooks) = loop_context.hooks.as_deref() {
        let session_id = config.cache_key.as_deref().unwrap_or("");
        if let HookOutcome::Block { reason } = hooks.run_user_prompt(user_prompt, session_id).await
        {
            return Err(NornError::HookBlocked {
                hook_type: HookType::UserPrompt,
                reason,
            });
        }
    }

    // Build the initial conversation, splicing in any slash-command
    // expansion in place of the literal user input. The raw user input is
    // still recorded as a UserMessage session event for audit (CO7).
    if let Some(edits) = loop_context.context_edits.as_mut() {
        edits.apply_persisted_compactions(store);
    }

    let initial_messages = build_initial_messages(user_prompt, loop_context, store)?;
    let mut messages = initial_messages.messages;
    let mut conversation_state = ConversationRequestState::new(
        config,
        provider.capabilities(),
        initial_messages.prefix_len,
        initial_messages.response_thread_anchor,
    )?;
    // REVIEW H2: the dynamic-context Developer message is tracked by
    // explicit index, never located by first-role matching, so resumed
    // histories containing Developer-role compaction summaries are safe.
    let mut dev_message = ManagedDevMessage::new(initial_messages.managed_developer_index);
    let new_input_len = initial_messages.new_input_len;

    let prompt_event_id = append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: user_prompt.to_string(),
        },
        loop_context.hooks.as_deref(),
    )
    .await?;

    drain_child_results(
        store,
        &mut messages,
        loop_context.child_result_rx.as_mut(),
        loop_context.hooks.as_deref(),
        None,
        &loop_context.children_usage,
    )
    .await?;

    let mut total_usage = Usage::default();
    let mut iteration_state = IterationMonitorState::default();
    let mut budget_consumed: u32 = 0;
    let mut iterations: u32 = 0;
    let mut best_attempt: Option<Value> = None;
    let mut follow_up_buffer: Vec<ChannelMessage> = Vec::new();
    let mut pending_before_injections: Vec<RuleInjection> = Vec::new();
    let mut compaction_state = CompactionState::new();
    // REVIEW item 4: failures produced by each iteration (tool errors and
    // schema-validation failures), drained into the iteration monitor at
    // the top of the next iteration so RepeatedFailure can actually fire.
    let mut latest_failures: Vec<String> = Vec::new();

    loop {
        // Cooperative cancellation gate: checked before every provider
        // call so an operator-triggered cancel becomes visible within one
        // iteration boundary (S1). Any tool batch from the previous
        // iteration has already returned by the time we land here, so
        // tools complete in full before this returns Cancelled.
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Ok(AgentStepResult::Cancelled {
                usage: total_usage,
                children_usage: loop_context.children_usage.snapshot(),
            });
        }

        if config.max_iterations.is_some_and(|max| iterations >= max) {
            return Ok(AgentStepResult::MaxIterationsReached {
                usage: total_usage,
                children_usage: loop_context.children_usage.snapshot(),
            });
        }
        iterations += 1;
        timeout_state.lock().iterations = iterations as usize;

        // Rules cleared at the top of each iteration so re-firings produce
        // fresh dynamic sections rather than accumulating duplicates.
        loop_context.clear_dynamic_sections();

        // NX-005 R6: re-stat the always-on NORN.md layers between
        // `clear_dynamic_sections` and `evaluate_prompt_commands`. When
        // staleness is detected, rebuild `system_sections[0]` so the
        // freshly-read content takes effect this iteration. The two
        // `stat` syscalls per iteration are unconditional but cheap; an
        // absent loader short-circuits inside `refresh_context_if_stale`.
        if loop_context.refresh_context_if_stale() {
            loop_context.rebuild_base_section();
        }

        loop_context.inject_environment_section();
        loop_context.inject_collaboration_mode();

        // Provider tool surface, recomputed every iteration from the live
        // provider's capabilities — the same cadence as the wire resolution
        // below — so a provider rebind (or a launch path whose static
        // prompt was assembled before the provider was bound) can never
        // leave a stale function-style framing of a hosted tool standing.
        // `None` (nothing hosted) injects nothing: function mode is what
        // the static tools section already describes.
        if let Some(section) = hosted_tools_prompt_section(&all_tools, provider.capabilities()) {
            loop_context.append_system_section(section);
        }

        // Evaluate runtime prompt commands before applying Before-timing
        // rule injections so their stdout becomes part of the dynamic
        // section stack the rest of this iteration builds on. Failures are
        // logged inside the helper and produce no section.
        loop_context.evaluate_prompt_commands().await;

        // Apply any Before-timing injections accumulated by the previous
        // iteration's tool batch. These must hit the prompt before the next
        // provider call.
        if !pending_before_injections.is_empty() {
            let injections = std::mem::take(&mut pending_before_injections);
            apply_rule_injections(loop_context, injections, &mut messages, store).await?;
        }

        // Sync the managed dynamic-context Developer message (REVIEW H2).
        // messages[0] (System) stays stable for prefix caching, and only
        // the tracked slot is ever written — history Developer messages
        // (compaction summaries) are never overwritten or deleted. An
        // empty developer message would be confused for a prompt, so the
        // slot is removed when there is no content.
        dev_message.sync(
            loop_context.dynamic_context(),
            &mut messages,
            &mut conversation_state,
        );

        // R5: expand session-variable placeholders in the managed
        // Developer message and tool descriptions before the request is
        // built. The System message (messages[0]) is NOT expanded so the
        // instructions field stays stable for caching.
        let iteration_tools = if let Some(var_store) = loop_context.variables.as_ref() {
            if let Some(idx) = dev_message.index()
                && let Some(content) = messages[idx].content.as_ref()
            {
                messages[idx].content = Some(expand_system_instruction(content, var_store).await);
            }
            expand_tool_descriptions(&all_tools, var_store).await
        } else {
            all_tools.clone()
        };

        let provider_tools =
            ResolvedToolSurface::resolve(&iteration_tools, provider.capabilities())
                .provider_definitions();

        // R3 + R4 + REVIEW 6b: token estimation, the token-warning event,
        // the auto-compaction trigger (including the LLM summarization
        // call), and in-flight application of a fired compaction. The
        // request message list is built *after* the preflight so the
        // current provider call already sees the compacted view and any
        // dropped response-thread anchor, not just the next step.
        let preflight = run_context_preflight(PreflightArgs {
            store,
            provider,
            model,
            messages: &mut messages,
            iteration_tools: &iteration_tools,
            conversation_state: &mut conversation_state,
            loop_context,
            config,
            compaction_state: &mut compaction_state,
            layout: InFlightPromptLayout {
                prefix_len: dev_message.prefix_len(),
                prompt_event_id: prompt_event_id.clone(),
                prompt_message_len: new_input_len,
            },
        })
        .await?;
        // Summarization tokens are real provider spend: account them
        // exactly like any other provider call in this step.
        if let Some(usage) = preflight.summarization_usage {
            total_usage += usage;
            timeout_state.lock().usage = total_usage.clone();
        }
        let request_messages = conversation_state.request_messages(&messages);

        let request = ProviderRequest {
            messages: request_messages,
            tools: provider_tools,
            model: model.to_string(),
            reasoning_effort: loop_context.reasoning_effort.clone(),
            reasoning_summary: loop_context.reasoning_summary.clone(),
            service_tier: loop_context.service_tier,
            config: None,
            cache_key: config.cache_key.clone(),
            previous_response_id: conversation_state.previous_response_id(),
            store: conversation_state.store(),
            context_management: ConversationRequestState::context_management(config),
        };

        if let Some(hooks) = loop_context.hooks.as_deref()
            && let HookOutcome::Block { reason } = hooks.run_pre_llm(&request).await
        {
            return Err(NornError::HookBlocked {
                hook_type: HookType::PreLlm,
                reason,
            });
        }

        // Race the provider call (including its retry-with-backoff
        // wrapper) against cancellation when a token is supplied. The
        // `biased` select gives the cancel arm priority so a token that
        // fires while the provider future is also ready resolves as
        // cancellation. Dropping the provider future cleanly aborts the
        // in-flight HTTP stream (reqwest is cancel-safe). When `cancel`
        // is `None` the call falls through to a direct await with no
        // select overhead (R3 acceptance).
        let response = {
            let policy = &loop_context.retry_policy;
            let request_template = request.clone();
            let tx_ref = event_tx;
            let provider_fut = retry_with_backoff(policy, || {
                let req = request_template.clone();
                async move { call_provider(provider, req, tx_ref).await }
            });
            match cancel.as_ref() {
                Some(token) => tokio::select! {
                    biased;
                    () = token.cancelled() => {
                        return Ok(AgentStepResult::Cancelled {
                            usage: total_usage,
                            children_usage: loop_context.children_usage.snapshot(),
                        });
                    }
                    result = provider_fut => result?,
                },
                None => provider_fut.await?,
            }
        };

        total_usage += response.usage.clone();
        {
            // Keep the timeout snapshot's usage in lock-step with the
            // running total so a timed-out step reports real spend.
            let mut snapshot = timeout_state.lock();
            snapshot.usage = total_usage.clone();
            if !response.text.is_empty() {
                snapshot.last_assistant_text = Some(response.text.clone());
            }
        }

        if let Some(hooks) = loop_context.hooks.as_deref() {
            let summary = LlmCallSummary {
                stop_reason: Some(response.stop_reason.clone()),
                usage: response.usage.clone(),
                event_count: u64::try_from(response.tool_calls.len()).unwrap_or(u64::MAX),
                error: None,
            };
            hooks.run_post_llm(&summary).await;
        }

        let assistant_tool_calls: Vec<AssistantToolCall> = response
            .tool_calls
            .iter()
            .map(|tc| AssistantToolCall {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                kind: tc.kind,
            })
            .collect();

        let tool_call_events: Vec<ToolCallEvent> = response
            .tool_calls
            .iter()
            .map(|tc| ToolCallEvent {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                arguments: serde_json::from_str(&tc.arguments)
                    .unwrap_or_else(|_| Value::String(tc.arguments.clone())),
                kind: tc.kind,
            })
            .collect();

        let content = response.text.clone();
        let thinking = response.thinking.clone();
        let message_content = if content.is_empty() {
            None
        } else {
            Some(content.clone())
        };

        append_and_notify(
            store,
            SessionEvent::AssistantMessage {
                base: EventBase::new(store.last_event_id()),
                content,
                thinking: thinking.clone(),
                tool_calls: tool_call_events,
                usage: EventUsage {
                    input_tokens: response.usage.input_tokens,
                    output_tokens: response.usage.output_tokens,
                    cache_read_tokens: response.usage.cache_read_tokens,
                    cache_write_tokens: response.usage.cache_write_tokens,
                    cost_usd: response.usage.cost_usd,
                },
                stop_reason: match &response.stop_reason {
                    StopReason::EndTurn => "end_turn",
                    StopReason::ToolUse => "tool_use",
                    StopReason::MaxTokens => "max_tokens",
                    StopReason::ContentFilter => "content_filter",
                }
                .to_string(),
                response_id: response.response_id.clone(),
            },
            loop_context.hooks.as_deref(),
        )
        .await?;

        messages.push(Message {
            role: MessageRole::Assistant,
            content: message_content,
            thinking,
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
        conversation_state.observe_response(response.response_id.as_deref(), messages.len());

        if let Some(monitor_cfg) = loop_context.iteration_monitor.as_ref() {
            let latest_text = if response.text.is_empty() {
                None
            } else {
                Some(response.text.as_str())
            };
            // REVIEW item 4: drain the failures the previous iteration
            // produced (tool errors, schema-validation failures) into the
            // monitor so RepeatedFailure detection has real input.
            let failures = std::mem::take(&mut latest_failures);
            let signals = evaluate_iteration(
                &mut iteration_state,
                &total_usage,
                latest_text,
                Some(&failures),
                monitor_cfg,
            );
            handle_iteration_signals(store, &mut messages, signals, loop_context.hooks.as_deref())
                .await?;
        }

        let classification = classify_response(&response, output_schema, &config.schema_tool_name);

        match classification {
            ResponseClass::SchemaValid { output } => {
                accept_schema_tool_call(
                    store,
                    &mut messages,
                    &response,
                    &config.schema_tool_name,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;

                match resolve_stop_boundary(StopBoundary {
                    store,
                    messages: &mut messages,
                    inbound: inbound.as_deref_mut(),
                    follow_up_buffer: &mut follow_up_buffer,
                    loop_context: &mut *loop_context,
                    linger: config.linger,
                    cancel: cancel.as_ref(),
                    event_tx,
                })
                .await?
                {
                    BoundaryOutcome::Continue => continue,
                    BoundaryOutcome::Cancelled => {
                        return Ok(AgentStepResult::Cancelled {
                            usage: total_usage,
                            children_usage: loop_context.children_usage.snapshot(),
                        });
                    }
                    BoundaryOutcome::Stop => {}
                }

                if let Some(hooks) = loop_context.hooks.as_deref() {
                    let outcome = hooks.run_stop(response.text.as_str()).await;
                    if inject_stop_block(outcome, hooks, store, &mut messages).await? {
                        continue;
                    }
                }

                return Ok(AgentStepResult::Completed {
                    output,
                    usage: total_usage,
                    children_usage: loop_context.children_usage.snapshot(),
                });
            }

            ResponseClass::SchemaInvalid {
                output,
                errors,
                schema_call_index,
            } => {
                budget_consumed += 1;
                best_attempt = Some(output.clone());
                if loop_context.iteration_monitor.is_some() {
                    latest_failures.extend(errors.iter().cloned());
                }

                if let Some(collector) = loop_context.diagnostics.as_ref() {
                    let schema_err = SchemaError::ValidationFailed {
                        schema: output_schema.cloned().unwrap_or(Value::Null),
                        output: output.clone(),
                        errors: errors.clone(),
                    };
                    collector.report(NornDiagnostic::from_schema_error(&schema_err));
                }

                if budget_consumed >= config.schema_attempt_budget {
                    return Ok(AgentStepResult::SchemaUnreachable {
                        best_attempt,
                        validation_errors: errors,
                        attempts: budget_consumed,
                        usage: total_usage,
                        children_usage: loop_context.children_usage.snapshot(),
                    });
                }

                let failure_watermark = store.len();
                let before = execute_tool_batch(ToolBatchRequest {
                    provider: None,
                    executor,
                    store,
                    messages: &mut messages,
                    response: &response,
                    tool_indices: (0..schema_call_index).collect(),
                    config,
                    loop_context,
                    event_tx,
                })
                .await?;
                pending_before_injections.extend(before);
                if loop_context.iteration_monitor.is_some() {
                    latest_failures.extend(collect_tool_failures(store, failure_watermark));
                }

                let schema = output_schema.ok_or_else(|| {
                    NornError::Provider(ProviderError::StreamError {
                        reason: "schema unexpectedly missing".to_string(),
                    })
                })?;
                let feedback = format_validation_feedback(schema, &output, &errors);
                let schema_tc = &response.tool_calls[schema_call_index];
                append_tool_result(
                    store,
                    &mut messages,
                    ToolResultRecord {
                        tool_call_id: &schema_tc.call_id,
                        tool_name: &config.schema_tool_name,
                        kind: schema_tc.kind,
                        output: &Value::String(feedback),
                        duration_ms: 0,
                    },
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;

                // REVIEW H3: tool calls the model placed *after* the schema
                // call must also receive exactly one result each, mirroring
                // the `ToolsAndSchemaValid` arm — otherwise the next request
                // carries unanswered calls and the provider rejects it,
                // permanently wedging the retry loop.
                reject_post_schema_tools(
                    store,
                    &mut messages,
                    &response,
                    &config.schema_tool_name,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;
            }

            ResponseClass::ToolsOnly { tool_calls } => {
                let failure_watermark = store.len();
                let before = execute_tool_batch(ToolBatchRequest {
                    provider: None,
                    executor,
                    store,
                    messages: &mut messages,
                    response: &response,
                    tool_indices: tool_calls,
                    config,
                    loop_context,
                    event_tx,
                })
                .await?;
                pending_before_injections.extend(before);
                if loop_context.iteration_monitor.is_some() {
                    latest_failures.extend(collect_tool_failures(store, failure_watermark));
                }

                inject_post_tool_batch_notifications(executor, false).await;

                let (steer, follow_up) = drain_and_partition(inbound.as_deref_mut());
                follow_up_buffer.extend(follow_up);
                inject_inbound_messages(
                    store,
                    &mut messages,
                    steer,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;
            }

            ResponseClass::TextStopNoSchema => {
                if output_schema.is_none() {
                    match resolve_stop_boundary(StopBoundary {
                        store,
                        messages: &mut messages,
                        inbound: inbound.as_deref_mut(),
                        follow_up_buffer: &mut follow_up_buffer,
                        loop_context: &mut *loop_context,
                        linger: config.linger,
                        cancel: cancel.as_ref(),
                        event_tx,
                    })
                    .await?
                    {
                        BoundaryOutcome::Continue => continue,
                        BoundaryOutcome::Cancelled => {
                            return Ok(AgentStepResult::Cancelled {
                                usage: total_usage,
                                children_usage: loop_context.children_usage.snapshot(),
                            });
                        }
                        BoundaryOutcome::Stop => {}
                    }

                    if let Some(hooks) = loop_context.hooks.as_deref() {
                        let outcome = hooks.run_stop(response.text.as_str()).await;
                        if inject_stop_block(outcome, hooks, store, &mut messages).await? {
                            continue;
                        }
                    }

                    return Ok(AgentStepResult::Completed {
                        output: Value::String(response.text),
                        usage: total_usage,
                        children_usage: loop_context.children_usage.snapshot(),
                    });
                }

                budget_consumed += 1;

                if budget_consumed >= config.schema_attempt_budget {
                    return Ok(AgentStepResult::SchemaUnreachable {
                        best_attempt,
                        validation_errors: vec![
                            "model stopped without calling schema tool".to_string(),
                        ],
                        attempts: budget_consumed,
                        usage: total_usage,
                        children_usage: loop_context.children_usage.snapshot(),
                    });
                }

                let schema = output_schema.ok_or_else(|| {
                    NornError::Provider(ProviderError::StreamError {
                        reason: "schema unexpectedly missing during nudge".to_string(),
                    })
                })?;
                let nudge_text = format_nudge(&config.schema_tool_name, schema);

                append_and_notify(
                    store,
                    SessionEvent::UserMessage {
                        base: EventBase::new(store.last_event_id()),
                        content: nudge_text.clone(),
                    },
                    loop_context.hooks.as_deref(),
                )
                .await?;

                messages.push(Message {
                    role: MessageRole::User,
                    content: Some(nudge_text),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                });
            }

            // REVIEW item 5: a `MaxTokens`/`ContentFilter` stop with no
            // tool calls in no-schema mode is an incomplete fragment.
            // Returning it as `Completed` made truncation indistinguishable
            // from success. A truncated run is a *stopped run with partial
            // output*, not a transport error, so it returns the typed
            // `Truncated` stop outcome carrying the partial text and the
            // accumulated usage; the full fragment and stop reason are also
            // persisted on the `AssistantMessage` and `loop.truncated`
            // events.
            ResponseClass::Truncated { kind } => {
                record_truncation(
                    store,
                    loop_context.hooks.as_deref(),
                    kind,
                    &response.text,
                    iterations,
                )
                .await?;
                return Ok(AgentStepResult::Truncated {
                    kind,
                    partial_text: (!response.text.is_empty()).then(|| response.text.clone()),
                    iterations,
                    usage: total_usage,
                    children_usage: loop_context.children_usage.snapshot(),
                });
            }

            ResponseClass::ToolsAndSchemaValid {
                pre_schema_tools,
                output,
            } => {
                let failure_watermark = store.len();
                let before = execute_tool_batch(ToolBatchRequest {
                    provider: None,
                    executor,
                    store,
                    messages: &mut messages,
                    response: &response,
                    tool_indices: pre_schema_tools,
                    config,
                    loop_context,
                    event_tx,
                })
                .await?;
                pending_before_injections.extend(before);
                if loop_context.iteration_monitor.is_some() {
                    latest_failures.extend(collect_tool_failures(store, failure_watermark));
                }

                accept_schema_tool_call(
                    store,
                    &mut messages,
                    &response,
                    &config.schema_tool_name,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;

                reject_post_schema_tools(
                    store,
                    &mut messages,
                    &response,
                    &config.schema_tool_name,
                    loop_context.hooks.as_deref(),
                    event_tx,
                )
                .await?;

                match resolve_stop_boundary(StopBoundary {
                    store,
                    messages: &mut messages,
                    inbound: inbound.as_deref_mut(),
                    follow_up_buffer: &mut follow_up_buffer,
                    loop_context: &mut *loop_context,
                    linger: config.linger,
                    cancel: cancel.as_ref(),
                    event_tx,
                })
                .await?
                {
                    BoundaryOutcome::Continue => continue,
                    BoundaryOutcome::Cancelled => {
                        return Ok(AgentStepResult::Cancelled {
                            usage: total_usage,
                            children_usage: loop_context.children_usage.snapshot(),
                        });
                    }
                    BoundaryOutcome::Stop => {}
                }

                if let Some(hooks) = loop_context.hooks.as_deref() {
                    let outcome = hooks.run_stop(response.text.as_str()).await;
                    if inject_stop_block(outcome, hooks, store, &mut messages).await? {
                        continue;
                    }
                }

                return Ok(AgentStepResult::Completed {
                    output,
                    usage: total_usage,
                    children_usage: loop_context.children_usage.snapshot(),
                });
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;

    // -- Helpers ----------------------------------------------------------

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
    fn assert_schema_unreachable(
        result: AgentStepResult,
    ) -> (Option<Value>, Vec<String>, u32, Usage) {
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
        provider: &'a MockProvider,
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
            .filter(|e| {
                matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read_file")
            })
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
                AgentEventKind::Subagent(_) | AgentEventKind::Message(_) => {
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

        let provider = MockProvider::new(vec![turn1, turn2]);
        let store = EventStore::new();
        let executor = MockToolExecutor::empty();
        let schema = simple_schema();

        let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
        tx.send(make_channel_message(
            "operator",
            "any more thoughts?",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");

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

        let provider = MockProvider::new(vec![turn1, turn2]);
        let store = EventStore::new();
        let executor = MockToolExecutor::empty();

        let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
        tx.send(make_channel_message(
            "operator",
            "say more",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");

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

        let provider = MockProvider::new(vec![turn1, turn2]);
        let store = EventStore::new();
        let executor = MockToolExecutor::empty();
        let schema = simple_schema();

        let (tx, mut rx) = crate::r#loop::inbound::inbound_channel(8);
        tx.send(make_channel_message(
            "operator",
            "more please",
            crate::r#loop::inbound::MessageKind::Update,
            0,
        ))
        .await
        .expect("send");

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
            async fn before_tool(
                &self,
                envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> HookOutcome {
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
            async fn before_tool(
                &self,
                envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> HookOutcome {
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
        observed:
            std::sync::Arc<parking_lot::Mutex<Option<crate::provider::request::ReasoningEffort>>>,
    }

    #[async_trait::async_trait]
    impl crate::integration::hooks::PreLlmHook for CaptureReasoning {
        async fn before_llm(
            &self,
            request: &crate::provider::request::ProviderRequest,
        ) -> crate::integration::hooks::HookOutcome {
            *self.observed.lock() = request.reasoning_effort.clone();
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
        let captured = observed.lock().clone();
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
            matches!(m.role, MessageRole::ToolResult)
                && m.tool_call_id.as_deref() == Some("tc_read")
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
            auto_compact_threshold_pct: Some(0.5),
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
            auto_compact_threshold_pct: Some(0.5),
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
            auto_compact_threshold_pct: Some(0.5),
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
}
