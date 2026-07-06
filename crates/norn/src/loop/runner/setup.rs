//! Pre-loop setup for one agent step: schema-tool synthesis, the
//! `UserPromptHook`, initial conversation assembly, prompt persistence,
//! and the pending/seeded/inbound message flushes that precede the first
//! provider call.

use crate::error::{ConfigError, HookType, NornError};
use crate::integration::hooks::HookOutcome;
use crate::r#loop::compaction::{CompactionState, SharedTimeoutState};
use crate::r#loop::conversation_state::ConversationRequestState;
use crate::r#loop::delivery::{
    drain_child_results, flush_inbound_messages, flush_pending_agent_messages,
    inject_inbound_messages,
};
use crate::r#loop::dev_context::ManagedDevMessage;
use crate::r#loop::helpers::{
    append_and_notify, build_initial_messages, installed_inline_char_limit,
};
use crate::r#loop::inbound::{ChannelMessage, InboundChannel};
use crate::r#loop::iteration::IterationMonitorState;
use crate::r#loop::schema::{build_schema_tool, check_reserved_envelope_keys};
use crate::provider::request::ToolDefinition;
use crate::provider::usage::Usage;
use crate::rules::types::RuleInjection;
use crate::session::events::{EventBase, EventId, SessionEvent};

use super::entry::AgentStepRunRequest;
use super::machine::StepMachine;

impl<'a> StepMachine<'a> {
    /// Perform all pre-loop setup and construct the machine ready to run.
    ///
    /// # Errors
    ///
    /// Returns [`NornError::Schema`] when the output schema declares a
    /// reserved envelope key, [`NornError::HookBlocked`] when the
    /// `UserPromptHook` blocks the prompt, [`NornError::Config`] when an
    /// external-message wake step has no pending, seeded, or inbound
    /// messages, and propagates event-store append failures.
    pub(super) async fn initialize(
        request: AgentStepRunRequest<'a>,
        timeout_state: SharedTimeoutState,
        mut inbound: Option<&'a mut InboundChannel>,
        follow_up_buffer: &'a mut Vec<ChannelMessage>,
        pending_before_injections: &'a mut Vec<RuleInjection>,
    ) -> Result<StepMachine<'a>, NornError> {
        let provider = request.provider;
        let executor = request.executor;
        let store = request.store;
        let user_prompt = request.user_prompt;
        let seed_messages = request.initial_messages;
        let wake_from_external = request.wake_from_external;
        let tools = request.tools;
        let output_schema = request.output_schema;
        let config = request.config;
        let event_tx = request.event_tx;
        let loop_context = request.loop_context;

        // The embedder-installed ToolOutputBudget governs the model-facing
        // inline size of every tool result this step persists; resolved once
        // because the executor's shared context is fixed for the step.
        let inline_char_limit = installed_inline_char_limit(executor);
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

        // NH-006 R3: UserPromptHook fires before an operator prompt enters the
        // agent loop, ahead of slash-command expansion and the initial
        // UserMessage session-event append. Inbound-wake steps have no
        // operator prompt; their delivered messages pass through the
        // SessionEventHook path when injected below instead.
        if let (Some(prompt), Some(hooks)) = (user_prompt, loop_context.hooks.as_deref()) {
            let session_id = config.cache_key.as_deref().unwrap_or("");
            if let HookOutcome::Block { reason } = hooks.run_user_prompt(prompt, session_id).await {
                return Err(NornError::HookBlocked {
                    hook_type: HookType::UserPrompt,
                    reason,
                });
            }
        }

        // Persisted context-edit marks (compaction supersession, suppress,
        // inject) load once per loop context, not once per step: a driver
        // that resumes a session with a fresh ContextEdits gets its marks
        // from this single walk, and every mark applied after it lands on
        // the tracker at apply time (via ContextEdits::suppress / inject /
        // summarize / compact / commit_compaction_plan). A per-step re-walk
        // here would be quadratic over a long-running loop context while
        // adding no information.
        if !loop_context.context_marks_loaded
            && let Some(edits) = loop_context.context_edits.as_mut()
        {
            edits.apply_persisted_marks(store);
            loop_context.context_marks_loaded = true;
        }

        // Build the initial conversation, splicing in any slash-command
        // expansion in place of the literal user input. The raw user input is
        // still recorded as a UserMessage session event for audit (CO7).
        let initial_messages = build_initial_messages(user_prompt, loop_context, store)?;
        let mut messages = initial_messages.messages;
        let conversation_state = ConversationRequestState::new(
            config,
            provider.capabilities(),
            initial_messages.prefix_len,
            initial_messages.response_thread_anchor,
        )?;
        // REVIEW H2: the dynamic-context Developer message is tracked by
        // explicit index, never located by first-role matching, so resumed
        // histories containing Developer-role compaction summaries are safe.
        let dev_message = ManagedDevMessage::new(initial_messages.managed_developer_index);
        let mut new_input_len = initial_messages.new_input_len;

        let prompt_event_id = if let Some(prompt) = user_prompt {
            append_and_notify(
                store,
                SessionEvent::UserMessage {
                    base: EventBase::new(store.last_event_id()),
                    content: prompt.to_string(),
                },
                loop_context.hooks.as_deref(),
            )
            .await?
        } else {
            EventId::new()
        };

        let mut injected_event_ids =
            flush_pending_agent_messages(store, &mut messages, loop_context, event_tx).await?;

        let mut seed_messages = seed_messages;
        match inject_inbound_messages(
            store,
            &mut messages,
            &mut seed_messages,
            loop_context.hooks.as_deref(),
            event_tx,
        )
        .await
        {
            Ok(ids) => injected_event_ids.extend(ids),
            Err(error) => {
                // Wake-seed messages have no other durable copy here; hand
                // the un-injected remainder to the step-exit re-queue sweep
                // (`follow_up_buffer` is hoisted into `run_agent_step_common`
                // and swept on every exit, including this setup error) so an
                // acknowledged seed is never silently dropped.
                follow_up_buffer.extend(seed_messages);
                return Err(error.into());
            }
        }

        injected_event_ids.extend(
            flush_inbound_messages(
                store,
                &mut messages,
                inbound.as_deref_mut(),
                follow_up_buffer,
                loop_context.hooks.as_deref(),
                event_tx,
            )
            .await?,
        );

        let prompt_event_id = if wake_from_external {
            let Some(event_id) = injected_event_ids.last().cloned() else {
                return Err(NornError::Config(ConfigError::InvalidConfig {
                    reason:
                        "external-message wake step started without pending or inbound messages"
                            .to_owned(),
                }));
            };
            new_input_len = 1;
            event_id
        } else {
            prompt_event_id
        };

        drain_child_results(
            store,
            &mut messages,
            loop_context.child_result_rx.as_mut(),
            loop_context.hooks.as_deref(),
            None,
            &loop_context.children_usage,
        )
        .await?;

        Ok(StepMachine {
            provider,
            executor,
            store,
            output_schema,
            model: request.model,
            config,
            event_tx,
            loop_context,
            cancel: request.cancel,
            inbound,
            follow_up_buffer,
            timeout_state,
            inline_char_limit,
            all_tools,
            messages,
            conversation_state,
            dev_message,
            new_input_len,
            prompt_event_id,
            total_usage: Usage::default(),
            iteration_state: IterationMonitorState::default(),
            budget_consumed: 0,
            iterations: 0,
            best_attempt: None,
            pending_before_injections,
            compaction_state: CompactionState::new(),
            latest_failures: Vec::new(),
        })
    }
}
