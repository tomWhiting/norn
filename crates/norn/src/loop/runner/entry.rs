//! Public entry points for the agent step runner.
//!
//! [`run_agent_step`] and [`run_agent_step_from_messages`] normalize their
//! inputs into the shared [`AgentStepRunRequest`], wrap the step in the
//! optional timeout budget, and guarantee the post-step invariants (tool
//! results completed, undelivered inbound messages re-queued) on every
//! exit path. The step itself is driven by the
//! [`StepMachine`](super::machine::StepMachine) state machine.

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::NornError;
use crate::r#loop::compaction::{SharedTimeoutState, shared_timeout_state};
use crate::r#loop::config::{AgentLoopConfig, AgentStepResult, ToolExecutor};
use crate::r#loop::delivery::{UndeliveredWindow, requeue_undelivered_inbound};
use crate::r#loop::helpers::ensure_tool_results_complete;
use crate::r#loop::inbound::{ChannelMessage, InboundChannel};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::ToolDefinition;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;

use super::machine::StepMachine;

/// Inputs for [`run_agent_step`] — one complete agent loop step opened
/// by an operator/user prompt. The behavioral contract lives on
/// [`run_agent_step`] itself.
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

/// Request for a step woken by already-buffered messages instead of a
/// human/operator prompt.
///
/// This is the idle-agent wake surface for TUI and embedders: the step starts
/// by delivering durable pending messages plus the supplied message batch
/// through the normal `<agent_message>` path, then calls the provider. No
/// synthetic empty user prompt is recorded.
pub struct AgentMessageStepRequest<'a> {
    /// The model provider that issues completion requests for this step.
    pub provider: &'a dyn Provider,
    /// Executes tool calls requested by the model during the step.
    pub executor: &'a dyn ToolExecutor,
    /// Event store that persists the conversation and usage events.
    pub store: &'a EventStore,
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
    /// Messages that woke and seed this step.
    pub initial_messages: Vec<ChannelMessage>,
    /// Optional channel to keep draining for messages that arrive while the
    /// step is running.
    pub inbound: Option<&'a mut InboundChannel>,
    /// Mutable per-loop context threaded through the step.
    pub loop_context: &'a mut LoopContext,
    /// Optional cancellation token.
    pub cancel: Option<CancellationToken>,
}

/// Normalized inputs shared by both public entry points.
pub(super) struct AgentStepRunRequest<'a> {
    pub(super) provider: &'a dyn Provider,
    pub(super) executor: &'a dyn ToolExecutor,
    pub(super) store: &'a EventStore,
    pub(super) user_prompt: Option<&'a str>,
    pub(super) initial_messages: Vec<ChannelMessage>,
    pub(super) wake_from_external: bool,
    pub(super) tools: &'a [ToolDefinition],
    pub(super) output_schema: Option<&'a Value>,
    pub(super) model: &'a str,
    pub(super) config: &'a AgentLoopConfig,
    pub(super) event_tx: Option<&'a AgentEventSender>,
    pub(super) inbound: Option<&'a mut InboundChannel>,
    pub(super) loop_context: &'a mut LoopContext,
    pub(super) cancel: Option<CancellationToken>,
}

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
/// # Timeout vs cancellation
///
/// When `config.step_timeout` is set, the entire loop body is wrapped in
/// [`tokio::time::timeout`] and elapsing the budget produces
/// [`AgentStepResult::TimedOut`] with whatever partial output the model
/// produced. Elapsing is a **hard cut**: the inner future is dropped
/// wherever it is suspended, including mid-tool-batch — in-flight tools
/// do *not* finish. Tool calls left without results are repaired in the
/// event store afterwards
/// ([`ensure_tool_results_complete`](crate::r#loop::ensure_tool_results_complete)
/// synthesizes aborted-result records), but any external side effect a
/// dropped tool had already started is not undone. See
/// [`AgentLoopConfig::step_timeout`] for the config-side statement of
/// this contract.
///
/// When `cancel` is `Some`, the loop participates in *cooperative*
/// cancellation: the token is checked at the top of each iteration and
/// raced against the in-flight provider call. On cancellation the loop
/// returns [`AgentStepResult::Cancelled`] with usage accumulated so far;
/// any tool already executing finishes in full before this is returned
/// (cancellation is at the loop level, not inside tools). When `cancel`
/// is `None` the loop has no cancellation overhead.
///
/// # Undelivered inbound messages
///
/// [`MessageKind::Update`](crate::r#loop::inbound::MessageKind) messages
/// drained mid-step buffer until a would-stop boundary. If the step ends
/// without passing through such a boundary (max-iterations,
/// schema-unreachable, truncation, cancellation, timeout, or a hard error),
/// the buffered messages are re-queued into the loop's durable pending store
/// ([`LoopContext::pending_agent_messages`]) with `agent_message.queued`
/// audit events so an acknowledged delivery is never silently dropped.
/// The same re-queue captures messages still sitting undrained in the
/// step's inbound channel when the step ends — sends the router accepted
/// (and acknowledged) after the loop's final drain — so the durable
/// pending store, which wake eligibility reads, sees every accepted
/// message.
///
/// # Errors
///
/// Propagates any [`NornError`] surfaced by the inner step loop —
/// provider failures, event store failures, `PreLlmHook` blocks
/// ([`NornError::HookBlocked`]), or unrecoverable tool errors. A
/// no-schema response truncated by the provider
/// (`MaxTokens`/`ContentFilter` with no tool calls) is **not** an error:
/// it returns [`AgentStepResult::Truncated`] carrying the partial text and
/// accumulated usage, with the full fragment and stop reason persisted on
/// the `AssistantMessage` event and the accompanying `loop.truncated`
/// Custom event.
pub async fn run_agent_step(request: AgentStepRequest<'_>) -> Result<AgentStepResult, NornError> {
    run_agent_step_common(AgentStepRunRequest {
        provider: request.provider,
        executor: request.executor,
        store: request.store,
        user_prompt: Some(request.user_prompt),
        initial_messages: Vec::new(),
        wake_from_external: false,
        tools: request.tools,
        output_schema: request.output_schema,
        model: request.model,
        config: request.config,
        event_tx: request.event_tx,
        inbound: request.inbound,
        loop_context: request.loop_context,
        cancel: request.cancel,
    })
    .await
}

/// Drive one step seeded by inbound messages already buffered for the agent.
///
/// # Errors
///
/// Returns [`NornError::Config`] when no pending, seeded, or inbound messages were
/// actually available to seed the step; otherwise mirrors
/// [`run_agent_step`].
pub async fn run_agent_step_from_messages(
    request: AgentMessageStepRequest<'_>,
) -> Result<AgentStepResult, NornError> {
    run_agent_step_common(AgentStepRunRequest {
        provider: request.provider,
        executor: request.executor,
        store: request.store,
        user_prompt: None,
        initial_messages: request.initial_messages,
        wake_from_external: true,
        tools: request.tools,
        output_schema: request.output_schema,
        model: request.model,
        config: request.config,
        event_tx: request.event_tx,
        inbound: request.inbound,
        loop_context: request.loop_context,
        cancel: request.cancel,
    })
    .await
}

async fn run_agent_step_common(
    mut request: AgentStepRunRequest<'_>,
) -> Result<AgentStepResult, NornError> {
    let timeout_state = shared_timeout_state();
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
    // The follow-up buffer lives out here — not inside the inner future —
    // so buffered Update messages survive every exit path (including the
    // timeout branch dropping the inner future) and can be re-queued
    // durably below. The recipient identity and pending store are cheap
    // handles captured before `request` moves into the inner future.
    let requeue_agent_id = request.loop_context.agent_id;
    let requeue_pending = request.loop_context.pending_agent_messages.clone();
    // The inbound channel is hoisted out of the request for the same
    // reason: the loop's final drain runs *inside* the inner future, so a
    // message the channel accepted (and its sender was told was
    // delivered) after that drain — or before a timeout cut — would sit
    // undrained when the step ends. Holding the channel out here lets the
    // exit path below sweep it into the durable pending store on every
    // exit, including the timeout branch dropping the inner future.
    let mut inbound = request.inbound.take();
    let mut follow_up_buffer: Vec<ChannelMessage> = Vec::new();
    let result = if let Some(budget) = request.config.step_timeout {
        let inner = run_agent_step_inner(
            request,
            Arc::clone(&timeout_state),
            inbound.as_deref_mut(),
            &mut follow_up_buffer,
        );
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
        run_agent_step_inner(
            request,
            timeout_state,
            inbound.as_deref_mut(),
            &mut follow_up_buffer,
        )
        .await
    };

    ensure_tool_results_complete(store).await;
    // Update messages the sender was told were delivered buffer until a
    // would-stop boundary; a step ending anywhere else (max-iterations,
    // schema-unreachable, truncation, cancellation, timeout, or an error
    // return) would otherwise drop them on the floor. The channel sweep
    // additionally captures messages accepted after the loop's final
    // drain, which no exit path ever saw. Re-queue whatever is left into
    // the durable pending store so the next step delivers them and wake
    // eligibility (which reads that store) can see them.
    if let Some(channel) = inbound {
        follow_up_buffer.extend(channel.drain());
    }
    requeue_undelivered_inbound(
        store,
        requeue_agent_id,
        requeue_pending.as_deref(),
        &mut follow_up_buffer,
        UndeliveredWindow::StepExit,
    );
    result
}

/// Initialize the step state machine and drive it to a result.
async fn run_agent_step_inner(
    request: AgentStepRunRequest<'_>,
    timeout_state: SharedTimeoutState,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
) -> Result<AgentStepResult, NornError> {
    // Two statements on purpose: chaining `.await?.run().await` keeps the
    // (completed) `initialize` future alive as a temporary for the whole
    // `run().await`, inflating every embedding future's size.
    let machine =
        StepMachine::initialize(request, timeout_state, inbound, follow_up_buffer).await?;
    // The driver future carries the whole per-step state (conversation,
    // request build, tool dispatch); pinning it on the heap keeps every
    // embedder's future — spawned child steps, the TUI event loop, the
    // CLI drivers — small instead of inlining ~16 KiB of loop state into
    // each of them (`clippy::large_futures`). One allocation per step.
    Box::pin(machine.run()).await
}
