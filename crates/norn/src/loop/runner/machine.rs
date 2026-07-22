//! Explicit state machine that drives one agent step.
//!
//! [`StepMachine`] owns every piece of mutable step state; each phase of
//! the loop is a named [`StepState`] handled by a dedicated method
//! (iteration gate, request build, provider call, response dispatch, and
//! stop-boundary resolution — the latter four live in the sibling
//! [`prompt`](super::prompt), [`provider_call`](super::provider_call),
//! [`dispatch`](super::dispatch), and [`stop`](super::stop) modules).
//! [`StepMachine::run`] loops over the transitions until a phase finishes
//! the step with an [`AgentStepResult`].

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::NornError;
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::compaction::{CompactionState, SharedTimeoutState};
use crate::r#loop::config::{
    AgentLoopConfig, AgentStepResult, ToolExecutionSnapshot, ToolExecutor,
};
use crate::r#loop::conversation_state::ConversationRequestState;
use crate::r#loop::delivery::{drain_child_results, flush_active_inputs};
use crate::r#loop::dev_context::ManagedDevMessage;
use crate::r#loop::helpers::persist_before_injection_audit;
use crate::r#loop::inbound::{ChannelMessage, InboundChannel};
use crate::r#loop::iteration::IterationMonitorState;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::{Message, ProviderRequest, ToolDefinition};
use crate::provider::traits::Provider;
use crate::provider::turn::ProviderTurnContext;
use crate::provider::usage::Usage;
use crate::rules::types::RuleInjection;
use crate::session::events::EventId;
use crate::session::store::EventStore;

use super::stop::StopOutput;

/// Discrete phases of one agent step, in transition order.
pub(super) enum StepState {
    /// Top-of-iteration checks: cooperative cancellation, the iteration
    /// budget, and the child-result / active-input drains.
    Gate,
    /// Assemble the dynamic prompt sections, run the context preflight
    /// (token estimation and auto-compaction), and build the provider
    /// request.
    BuildRequest,
    /// Fire the pre/post-LLM hooks around the provider call, persist the
    /// assistant response, and feed the iteration monitor.
    CallProvider(Box<ProviderRequest>),
    /// Route the classified response: tool batches, schema enforcement
    /// (acceptance, validation feedback, nudges), and truncation.
    Dispatch(Box<AssembledResponse>),
    /// Resolve the would-stop boundary (linger, Stop hook, completion
    /// envelope); injected work transitions back to [`StepState::Gate`].
    ResolveStop(StopOutput),
}

/// Transition produced by one phase of the machine.
pub(super) enum StepFlow {
    /// Enter the next phase.
    Next(StepState),
    /// The step is finished with this result.
    Done(AgentStepResult),
}

/// Prompt-command output prepared for the first request or pending execution
/// at a later request boundary.
pub(super) enum PromptCommandContextState {
    /// Exact expanded Developer content prepared before prompt persistence.
    /// Threaded providers validate it as part of the seed at that boundary;
    /// stateless providers validate their complete managed tail after preflight.
    Prepared(Option<String>),
    /// Later iterations evaluate the current command definition on demand.
    Pending,
}

/// Result of validated step setup.
pub(super) enum StepInitialization<'a> {
    /// Setup completed and the provider loop may start.
    Ready(Box<StepMachine<'a>>),
    /// A pre-cancelled or zero-iteration-budget step cannot issue a request.
    Done(AgentStepResult),
}

/// Mutable state of one in-flight agent step, threaded through every
/// phase of the [`StepState`] machine.
pub(super) struct StepMachine<'a> {
    // -- Step inputs, fixed for the whole step --
    pub(super) provider: &'a dyn Provider,
    pub(super) executor: &'a dyn ToolExecutor,
    pub(super) store: &'a EventStore,
    pub(super) output_schema: Option<&'a Value>,
    pub(super) model: &'a str,
    pub(super) config: &'a AgentLoopConfig,
    pub(super) event_tx: Option<&'a AgentEventSender>,
    pub(super) loop_context: &'a mut LoopContext,
    pub(super) cancel: Option<CancellationToken>,
    pub(super) inbound: Option<&'a mut InboundChannel>,
    pub(super) follow_up_buffer: &'a mut Vec<ChannelMessage>,
    pub(super) timeout_state: SharedTimeoutState,
    /// Model-facing inline size limit for every tool result this step
    /// persists; resolved once because the executor's shared context is
    /// fixed for the step.
    pub(super) inline_char_limit: usize,
    /// Advertised tools including the synthesized schema tool.
    pub(super) all_tools: Vec<ToolDefinition>,
    /// Caller-supplied tools used when the executor has no live generations.
    pub(super) static_tools: Vec<ToolDefinition>,
    /// Synthesized output-schema tool, advertised but never dispatched.
    pub(super) schema_tool: Option<ToolDefinition>,
    /// Generation leased for the current provider request and its response.
    pub(super) tool_snapshot: Option<ToolExecutionSnapshot>,

    // -- Conversation state --
    pub(super) messages: Vec<Message>,
    pub(super) conversation_state: ConversationRequestState,
    pub(super) prompt_command_context: PromptCommandContextState,
    /// Tracker for the stateless managed-context Developer projection
    /// (REVIEW H2): addressed by explicit index, never located by first-role
    /// matching, so resumed histories containing Developer-role compaction
    /// summaries are safe. Threaded requests leave this tracker empty.
    pub(super) dev_message: ManagedDevMessage,
    /// Number of trailing messages the new user input occupies (1 for a
    /// literal prompt, N for a slash expansion, 1 for an external wake).
    pub(super) new_input_len: usize,
    /// Event ID of the persisted prompt (or last injected wake message).
    pub(super) prompt_event_id: EventId,
    /// Non-persisted transport state shared by provider calls in this step.
    pub(super) provider_turn_context: ProviderTurnContext,

    // -- Per-step accumulators --
    pub(super) total_usage: Usage,
    pub(super) iteration_state: IterationMonitorState,
    pub(super) budget_consumed: u32,
    pub(super) iterations: u32,
    pub(super) best_attempt: Option<Value>,
    /// Before-timing rule injections a tool batch fired but that no
    /// `build_request` has yet consumed. Borrowed from
    /// [`run_agent_step_common`](super::entry) — not owned — so it survives
    /// the step-timeout drop path (which drops this machine mid-flight),
    /// where the outer common frame persists it (F2). On every path where
    /// [`Self::run`] returns, [`Self::persist_undelivered_before_injections`]
    /// drains it first, so the common frame then sees it empty.
    pub(super) pending_before_injections: &'a mut Vec<RuleInjection>,
    pub(super) compaction_state: CompactionState,
    /// Failures produced by each iteration (tool errors and
    /// schema-validation failures), drained into the iteration monitor at
    /// the top of the next iteration so `RepeatedFailure` can actually fire
    /// (REVIEW item 4).
    pub(super) latest_failures: Vec<String>,
}

impl StepMachine<'_> {
    /// Executor leased alongside the current provider request.
    ///
    /// Before the first request, and for static executors that do not expose
    /// generations, this is the caller-supplied executor.
    pub(super) fn cycle_executor(&self) -> Option<Arc<dyn ToolExecutor>> {
        self.tool_snapshot
            .as_ref()
            .map(|snapshot| Arc::clone(&snapshot.executor))
    }

    /// Drive the state machine until a phase finishes the step.
    ///
    /// # Errors
    ///
    /// Propagates any [`NornError`] surfaced by a phase — provider
    /// failures, event store failures, hook blocks, or unrecoverable
    /// tool errors.
    pub(super) async fn run(mut self) -> Result<AgentStepResult, NornError> {
        let mut state = StepState::Gate;
        let outcome = loop {
            let flow = match state {
                StepState::Gate => self.gate().await,
                StepState::BuildRequest => self.build_request().await,
                StepState::CallProvider(request) => self.call_provider(*request).await,
                StepState::Dispatch(response) => self.dispatch(*response).await,
                StepState::ResolveStop(output) => self.resolve_stop(output).await,
            };
            match flow {
                Ok(StepFlow::Next(next)) => state = next,
                Ok(StepFlow::Done(result)) => break Ok(result),
                Err(error) => break Err(error),
            }
        };
        // Persist any Before-timing rule injection a tool batch fired but no
        // `build_request` will consume — the step terminated first. See
        // [`Self::persist_undelivered_before_injections`]. A no-op on the
        // common path (build_request already drained the buffer). This runs
        // only when `run` RETURNS; the step-timeout branch drops this future
        // mid-flight, so the buffer is borrowed from `run_agent_step_common`
        // and that outer frame persists it on the timeout path (F2).
        self.persist_undelivered_before_injections().await;
        outcome
    }

    /// Persist the audit events for Before-timing rule injections that
    /// fired in a tool batch but were never delivered because the step
    /// terminated before the next [`build_request`](Self::build_request)
    /// could consume them (max-iterations or cancellation at the gate, a
    /// completion/stop boundary reached straight after a batch, or an error
    /// unwind). The step-timeout drop path does not reach here — it drops
    /// this future mid-flight — so the identical persist runs in
    /// [`run_agent_step_common`](super::entry) against the same borrowed
    /// buffer (F2), through the same
    /// [`persist_before_injection_audit`] path.
    ///
    /// A fired rule already executed its (possibly `shell_source`) side
    /// effect, and [`session/events.rs`](crate::session::events) requires a
    /// [`SessionEvent::RuleInjection`](crate::session::events::SessionEvent::RuleInjection)
    /// "persisted for every fired rule regardless of delivery mode". This
    /// persists that event exactly as After-timing does at fire time, so a
    /// fired firing is never discarded without an audit record.
    ///
    /// # Presence semantics
    ///
    /// The persisted event makes the rule "present" on the next step's
    /// presence rebuild (presence is keyed on the visible `RuleInjection`
    /// event, not on delivery — see
    /// [`LoopContext::rebuild_rule_presence`](crate::r#loop::loop_context::LoopContext::rebuild_rule_presence)).
    /// That stays coherent because the next step reconstructs the rule's
    /// delivered content from the *same* event via
    /// [`session/conversion`](crate::session::conversion): the content the
    /// live step never delivered re-enters the conversation as reconstructed
    /// history, so "present" and "in context" agree — identical to
    /// After-timing. Persisting the audit event alone (not the live-delivery
    /// message push) is sufficient and correct here: that live effect targets
    /// a buffer discarded on step exit and is reconstructed from this event
    /// next step — see
    /// [`persist_before_injection_audit`] for the full rationale.
    ///
    /// Best-effort on the store-failure path: a persist failure here is
    /// logged, never silently swallowed, and never rewrites the step's
    /// already-decided result — matching the exit-time
    /// [`requeue_undelivered_inbound`](crate::r#loop::delivery::requeue_undelivered_inbound)
    /// sweep's convention.
    async fn persist_undelivered_before_injections(&mut self) {
        if self.pending_before_injections.is_empty() {
            return;
        }
        let injections = std::mem::take(&mut *self.pending_before_injections);
        let undelivered = injections.len();
        if let Err(error) = persist_before_injection_audit(
            self.store,
            self.loop_context.hooks.as_deref(),
            &injections,
        )
        .await
        {
            tracing::error!(
                %error,
                undelivered,
                "failed to persist fired Before-timing rule injection audit \
                 events on step exit; the firing executed but leaves no \
                 audit record",
            );
        }
    }

    /// Top-of-iteration gate: cancellation, the iteration budget, and the
    /// pre-request drains.
    async fn gate(&mut self) -> Result<StepFlow, NornError> {
        // Cooperative cancellation gate: checked before every provider
        // call so an operator-triggered cancel becomes visible within one
        // iteration boundary (S1). Any tool batch from the previous
        // iteration has already returned by the time we land here, so
        // tools complete in full before this returns Cancelled.
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Ok(StepFlow::Done(self.cancelled_result()));
        }

        if self
            .config
            .max_iterations
            .is_some_and(|max| self.iterations >= max)
        {
            return Ok(StepFlow::Done(AgentStepResult::MaxIterationsReached {
                usage: std::mem::take(&mut self.total_usage),
                children_usage: self.loop_context.children_usage.snapshot(),
            }));
        }
        self.iterations += 1;
        self.timeout_state.lock().iterations = self.iterations as usize;

        // Child/fork completions can arrive while the parent is executing
        // tools from the previous provider response. Drain them here so
        // the very next provider request sees the framed result instead
        // of holding it until a would-stop boundary.
        drain_child_results(
            self.store,
            &mut self.messages,
            self.loop_context.child_result_rx.as_mut(),
            self.loop_context.hooks.as_deref(),
            None,
            &self.loop_context.children_usage,
        )
        .await?;

        flush_active_inputs(
            self.store,
            &mut self.messages,
            self.loop_context.active_input_rx.as_mut(),
            self.loop_context.hooks.as_deref(),
        )
        .await?;

        Ok(StepFlow::Next(StepState::BuildRequest))
    }

    /// Build the `Cancelled` result carrying the usage accumulated so far.
    pub(super) fn cancelled_result(&mut self) -> AgentStepResult {
        AgentStepResult::Cancelled {
            usage: std::mem::take(&mut self.total_usage),
            children_usage: self.loop_context.children_usage.snapshot(),
        }
    }
}
