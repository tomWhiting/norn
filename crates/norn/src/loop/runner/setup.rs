//! Pre-loop setup for one agent step: schema-tool synthesis, the
//! `UserPromptHook`, initial conversation assembly, prompt persistence,
//! and the pending/seeded/inbound message flushes that precede the first
//! provider call.

use crate::error::{ConfigError, HookType, NornError, ProviderError, SessionError};
use crate::integration::hooks::HookOutcome;
use crate::r#loop::compaction::{CompactionState, SharedTimeoutState};
use crate::r#loop::conversation_state::{
    ConversationRequestState, validate_response_state_provenance,
};
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
use crate::r#loop::loop_context::DEFAULT_PROMPT_COMMAND_TIMEOUT;
use crate::r#loop::schema::{build_schema_tool, check_reserved_envelope_keys};
use crate::provider::request::ToolDefinition;
use crate::provider::turn::ProviderTurnContext;
use crate::provider::usage::Usage;
use crate::rules::types::RuleInjection;
use crate::session::SessionPersistError;
use crate::session::events::{EventBase, EventId, SessionEvent};

use super::entry::AgentStepRunRequest;
use super::machine::{PromptCommandContextState, StepInitialization, StepMachine};
use super::prompt::developer_message;

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
        seed_messages: &mut Vec<ChannelMessage>,
        mut inbound: Option<&'a mut InboundChannel>,
        follow_up_buffer: &'a mut Vec<ChannelMessage>,
        pending_before_injections: &'a mut Vec<RuleInjection>,
        step_started: std::time::Instant,
    ) -> Result<StepInitialization<'a>, NornError> {
        let provider = request.provider;
        let executor = request.executor;
        let store = request.store;
        let user_prompt = request.user_prompt;
        let wake_from_external = request.wake_from_external;
        let tools = request.tools;
        let output_schema = request.output_schema;
        let config = request.config;
        let event_tx = request.event_tx;
        let loop_context = request.loop_context;
        let provider_state_identity = provider.state_identity();
        let provider_capabilities = provider.capabilities();
        let provider_threaded = ConversationRequestState::validate_setup(
            config,
            provider_capabilities,
            provider_state_identity,
        )?;

        // Reserved provider-state records are untrusted durable input. Check
        // them before affinity adoption can stamp an otherwise-unbound store.
        validate_response_state_provenance(&store.events())?;
        validate_or_bind_store_identity(store, provider_state_identity)?;

        if let Some(result) = no_request_result(config, request.cancel.as_ref(), loop_context) {
            persist_accepted_messages_without_request(
                store,
                loop_context,
                event_tx,
                seed_messages,
                inbound.as_deref_mut(),
                follow_up_buffer,
            )
            .await?;
            return Ok(StepInitialization::Done(result));
        }

        // The embedder-installed ToolOutputBudget governs the model-facing
        // inline size of every tool result this step persists; resolved once
        // because the executor's shared context is fixed for the step.
        let inline_char_limit = installed_inline_char_limit(executor);
        let static_tools: Vec<ToolDefinition> = tools.to_vec();
        let schema_tool = if let Some(schema) = output_schema {
            // Backstop for every schema source (embedder config, fork, rhai;
            // spawn_agent also rejects at its argument boundary): a schema
            // declaring a reserved envelope key would be unsatisfiable or
            // silently lossy after the pre-validation envelope split — refuse
            // it typed before the loop spends a single provider call.
            check_reserved_envelope_keys(schema).map_err(NornError::Schema)?;
            Some(build_schema_tool(&config.schema_tool_name, schema))
        } else {
            None
        };
        let mut all_tools = static_tools.clone();
        all_tools.extend(schema_tool.iter().cloned());

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

        // Freeze the first request's stable prompt snapshot before command
        // execution. File changes after this point are intentionally picked up
        // at the next request boundary, so setup validation and first dispatch
        // cannot observe different stable authority.
        loop_context.clear_dynamic_sections();
        if loop_context.refresh_context_if_stale() {
            loop_context.rebuild_base_section();
        }

        // Build the initial conversation, splicing in any slash-command
        // expansion in place of the literal user input. The raw user input is
        // still recorded as a UserMessage session event for audit (CO7). This
        // fallible expansion precedes prompt commands so an invalid slash
        // invocation cannot trigger their side effects.
        let mut initial_messages = build_initial_messages(user_prompt, loop_context, store)?;

        // Narrow the cooperative-cancellation window before spawning a shell.
        // Cancellation that arrives after this point may race an already-live
        // command, but a token observed here still causes no command or prompt
        // persistence.
        if let Some(result) = no_request_result(config, request.cancel.as_ref(), loop_context) {
            persist_accepted_messages_without_request(
                store,
                loop_context,
                event_tx,
                seed_messages,
                inbound.as_deref_mut(),
                follow_up_buffer,
            )
            .await?;
            return Ok(StepInitialization::Done(result));
        }

        // The no-request gate and every provider-state/configuration check have
        // passed. Prepare the first request's exact trusted Developer context
        // once, then carry it into `build_request` rather than executing the
        // command a second time.
        let prompt_command_context = loop_context
            .prepare_prompt_command_context(effective_prompt_command_timeout(config, step_started))
            .await;
        let mut initial_prompt_seed = initial_messages.prompt_seed_fingerprint;
        if provider_threaded && let Some(content) = prompt_command_context.as_ref() {
            initial_prompt_seed = initial_prompt_seed.with_operator_runtime_context(content);
            initial_messages.messages.insert(
                initial_messages.prefix_len,
                developer_message(content.clone()),
            );
            initial_messages.prefix_len = initial_messages.prefix_len.saturating_add(1);
            if let Some(anchor) = initial_messages.response_thread_anchor.as_mut() {
                anchor.note_stable_prefix_insertion();
            }
            for anchor in &mut initial_messages.legacy_response_thread_anchors {
                anchor.note_stable_prefix_insertion();
            }
        }

        let mut messages = initial_messages.messages;
        let mut conversation_state = ConversationRequestState::with_prompt_seed(
            config,
            provider_capabilities,
            initial_messages.prefix_len,
            initial_prompt_seed,
            initial_messages.response_thread_anchor,
        )?;
        conversation_state.require_state_identity(provider_state_identity)?;
        // Validate the exact stable-plus-command seed before the operator's
        // new prompt becomes durable. A changed seed has already cut its V2
        // anchor here, so provider-specific replay rejection cannot leave an
        // unsent UserMessage behind.
        if let Err(error) =
            provider.validate_replay(&conversation_state.request_messages(&messages))
        {
            if !matches!(&error, ProviderError::ProviderStateReplayUnavailable) {
                return Err(error.into());
            }
            let mut recovered = None;
            for candidate in initial_messages.legacy_response_thread_anchors {
                let Some(witness) = candidate.witness_message(&messages) else {
                    continue;
                };
                match provider.validate_replay(std::slice::from_ref(witness)) {
                    Err(ProviderError::ProviderStateReplayUnavailable) => {}
                    Ok(()) => continue,
                    Err(candidate_error) => return Err(candidate_error.into()),
                }
                let suffix =
                    conversation_state.request_messages_for_legacy_anchor(&messages, &candidate);
                match provider.validate_replay(&suffix) {
                    Ok(()) => {
                        recovered = Some(candidate);
                        break;
                    }
                    Err(ProviderError::ProviderStateReplayUnavailable) => {}
                    Err(candidate_error) => return Err(candidate_error.into()),
                }
            }
            let Some(anchor) = recovered else {
                return Err(error.into());
            };
            if !conversation_state.adopt_legacy_anchor(anchor) {
                return Err(error.into());
            }
        }
        // A stateless request attaches its managed-context Developer projection
        // at the tail during `build_request`; threaded requests use top-level
        // instructions. The compatibility-tail tracker therefore starts absent.
        let dev_message = ManagedDevMessage::new();
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

        match inject_inbound_messages(
            store,
            &mut messages,
            seed_messages,
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
                follow_up_buffer.append(seed_messages);
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

        let provider_turn_context = ProviderTurnContext::new(
            loop_context
                .variables
                .as_ref()
                .map(|variables| variables.session_id().to_owned()),
            prompt_event_id.as_str().to_owned(),
        );
        if let Some(identity) = provider_state_identity {
            provider_turn_context.bind_state_identity(identity)?;
        }

        Ok(StepInitialization::Ready(Box::new(StepMachine {
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
            static_tools,
            schema_tool,
            tool_snapshot: None,
            messages,
            conversation_state,
            prompt_command_context: PromptCommandContextState::Prepared(prompt_command_context),
            dev_message,
            new_input_len,
            prompt_event_id,
            provider_turn_context,
            total_usage: Usage::default(),
            iteration_state: IterationMonitorState::default(),
            budget_consumed: 0,
            iterations: 0,
            best_attempt: None,
            pending_before_injections,
            compaction_state: CompactionState::new(),
            latest_failures: Vec::new(),
        })))
    }
}

fn effective_prompt_command_timeout(
    config: &crate::r#loop::config::AgentLoopConfig,
    step_started: std::time::Instant,
) -> Option<std::time::Duration> {
    let command_timeout = config
        .prompt_command_timeout
        .unwrap_or(DEFAULT_PROMPT_COMMAND_TIMEOUT);
    config
        .step_timeout
        .map_or(config.prompt_command_timeout, |budget| {
            Some(command_timeout.min(budget.saturating_sub(step_started.elapsed())))
        })
}

fn no_request_result(
    config: &crate::r#loop::config::AgentLoopConfig,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    loop_context: &crate::r#loop::loop_context::LoopContext,
) -> Option<crate::r#loop::config::AgentStepResult> {
    if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        return Some(crate::r#loop::config::AgentStepResult::Cancelled {
            usage: Usage::default(),
            children_usage: loop_context.children_usage.snapshot(),
        });
    }
    (config.max_iterations == Some(0)).then(|| {
        crate::r#loop::config::AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
            children_usage: loop_context.children_usage.snapshot(),
        }
    })
}

async fn persist_accepted_messages_without_request(
    store: &crate::session::EventStore,
    loop_context: &crate::r#loop::loop_context::LoopContext,
    event_tx: Option<&crate::provider::agent_event::AgentEventSender>,
    seed_messages: &mut Vec<ChannelMessage>,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
) -> Result<(), NornError> {
    if let Some(inbound) = inbound {
        seed_messages.extend(inbound.drain());
    }
    let mut prompt_messages = Vec::new();
    if let Err(error) = inject_inbound_messages(
        store,
        &mut prompt_messages,
        seed_messages,
        loop_context.hooks.as_deref(),
        event_tx,
    )
    .await
    {
        follow_up_buffer.append(seed_messages);
        return Err(error.into());
    }
    Ok(())
}

fn validate_or_bind_store_identity(
    store: &crate::session::EventStore,
    requested: Option<crate::provider::ProviderStateIdentity>,
) -> Result<(), NornError> {
    let validate = || store.validate_or_bind_provider_state_identity(requested);
    let result = match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(validate)
        }
        _ => validate(),
    };
    match result {
        Ok(()) => Ok(()),
        Err(
            SessionPersistError::ProviderStateIdentityMismatch
            | SessionPersistError::ProviderStateIdentityRequired,
        ) => Err(ProviderError::ProviderStateIdentityMismatch.into()),
        Err(error) => Err(SessionError::from(error).into()),
    }
}
