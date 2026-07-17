//! Provider-call phase: the pre/post-LLM hooks, the (cancellable)
//! provider call itself, usage accounting, response persistence, and the
//! iteration monitor.

use serde_json::Value;

use crate::error::{HookType, NornError, SessionError};
use crate::integration::hooks::{HookOutcome, LlmCallSummary};
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::classify::call_provider_with_retry;
use crate::r#loop::config::AgentStepResult;
use crate::r#loop::helpers::{append_and_notify, handle_iteration_signals};
use crate::r#loop::iteration::evaluate_iteration;
use crate::r#loop::programmatic_calling::validate_programmatic_callers;
use crate::provider::events::StopReason;
use crate::provider::request::{AssistantToolCall, Message, MessageRole, ProviderRequest};
use crate::session::ResponseAudioArtifactLink;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

use super::machine::{StepFlow, StepMachine, StepState};

impl StepMachine<'_> {
    /// Call the provider (racing cancellation when a token is supplied),
    /// persist the assistant response, and feed the iteration monitor.
    pub(super) async fn call_provider(
        &mut self,
        request: ProviderRequest,
    ) -> Result<StepFlow, NornError> {
        if let Some(hooks) = self.loop_context.hooks.as_deref()
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
            let provider_fut = call_provider_with_retry(
                &self.loop_context.retry_policy,
                self.provider,
                request,
                self.event_tx,
                // Mirror in-flight deltas into the shared timeout state so
                // a hard cut (step timeout dropping this future, or the
                // cancel arm below winning the select) leaves the partial
                // content recoverable for the exit path's
                // `loop.partial_output` record (Gap 7).
                Some(&self.timeout_state),
                self.store.response_audio(),
            );
            match self.cancel.as_ref() {
                Some(token) => tokio::select! {
                    biased;
                    () = token.cancelled() => {
                        return Ok(StepFlow::Done(AgentStepResult::Cancelled {
                            usage: std::mem::take(&mut self.total_usage),
                            children_usage: self.loop_context.children_usage.snapshot(),
                        }));
                    }
                    result = provider_fut => result?,
                },
                None => provider_fut.await?,
            }
        };

        self.total_usage += response.usage.clone();
        {
            // Keep the timeout snapshot's usage in lock-step with the
            // running total so a timed-out step reports real spend.
            let mut snapshot = self.timeout_state.lock();
            snapshot.usage = self.total_usage.clone();
            if !response.text.is_empty() {
                snapshot.last_assistant_text = Some(response.text.clone());
            }
        }

        validate_programmatic_callers(&self.messages, &response)?;

        if let Some(hooks) = self.loop_context.hooks.as_deref() {
            let summary = LlmCallSummary {
                stop_reason: Some(response.stop_reason.clone()),
                usage: response.usage.clone(),
                event_count: u64::try_from(response.tool_calls.len()).unwrap_or(u64::MAX),
                error: None,
            };
            hooks.run_post_llm(&summary).await;
        }

        self.persist_assistant_turn(&response).await?;
        self.monitor_iteration(&response).await?;

        Ok(StepFlow::Next(StepState::Dispatch(Box::new(response))))
    }

    /// Append the `AssistantMessage` session event and mirror the turn
    /// into the live conversation.
    async fn persist_assistant_turn(
        &mut self,
        response: &AssembledResponse,
    ) -> Result<(), NornError> {
        // Usage-floor anchor: the provider's reported spend for this call
        // (input + output) is a truthful lower bound for the next request,
        // which contains at least this one plus the new turn — content the
        // client-side character estimate cannot always see (e.g. replayed
        // encrypted reasoning items on stateless Responses backends). The
        // next preflight anchors its token warning and auto-compaction
        // trigger on `max(estimate, floor)`; `ContextEdits` clears the
        // floor whenever the prompt view shrinks.
        if let Some(edits) = self.loop_context.context_edits.as_mut() {
            edits.set_usage_floor(
                response
                    .usage
                    .input_tokens
                    .saturating_add(response.usage.output_tokens),
            );
        }
        let assistant_tool_calls: Vec<AssistantToolCall> = response
            .tool_calls
            .iter()
            .map(|tc| AssistantToolCall {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                kind: tc.kind,
                caller: tc.caller.clone(),
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
                caller: tc.caller.clone(),
            })
            .collect();

        let content = response.text.clone();
        let thinking = response.thinking.clone();
        let message_content = if content.is_empty() {
            None
        } else {
            Some(content.clone())
        };

        let parent_id = self.store.last_event_id();
        let (assistant_base, audio_link_event) = match response.response_audio {
            Some(reference) => {
                let link_base = EventBase::new(parent_id);
                let assistant_base = EventBase::new(Some(link_base.id.clone()));
                let link = ResponseAudioArtifactLink::new(
                    assistant_base.id.clone(),
                    reference,
                    response.response_id.clone(),
                );
                let event = link.into_custom_event(link_base).map_err(|_source| {
                    SessionError::StorageError {
                        reason: "failed to encode the response-audio artifact link".to_owned(),
                    }
                })?;
                (assistant_base, Some(event))
            }
            None => (EventBase::new(parent_id), None),
        };

        // Persist the sealed-artifact association first. A crash between the
        // two appends leaves an explicit orphan precursor; it cannot leave a
        // durable assistant turn whose audio association vanished silently.
        if let Some(link_event) = audio_link_event {
            append_and_notify(self.store, link_event, self.loop_context.hooks.as_deref()).await?;
        }

        append_and_notify(
            self.store,
            SessionEvent::AssistantMessage {
                response_items: response.response_items.clone(),
                base: assistant_base,
                content,
                thinking: thinking.clone(),
                // Persist the captured reasoning items so a resumed session
                // rebuilds this assistant turn with its reasoning intact
                // (encrypted-content items are what the Responses serializer
                // replays across tool iterations on stateless backends).
                reasoning: response.reasoning.clone(),
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
            self.loop_context.hooks.as_deref(),
        )
        .await?;

        // The turn is durable: only now may the Gap 7 capture disarm.
        // Clearing at assembly time (inside `call_provider`) would open a
        // window — the `run_post_llm` hooks between assembly and this
        // append run arbitrary user shell hooks — where a step timeout or
        // cancellation loses the complete response from the durable log.
        // Cross-call staleness is guarded by the per-attempt reset at the
        // top of `call_provider`.
        self.timeout_state.lock().in_flight_partial = None;

        self.messages.push(Message {
            response_items: response.response_items.clone(),
            role: MessageRole::Assistant,
            content: message_content,
            thinking,
            // Structured reasoning items ride on the local replay message so
            // stateless backends (`response_threading: false`) can replay
            // encrypted reasoning across tool-call iterations.
            reasoning: response.reasoning.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        });
        self.conversation_state
            .observe_response(response.response_id.as_deref(), self.messages.len());
        Ok(())
    }

    /// Feed the iteration monitor (when configured) with this turn's text
    /// and the failures the previous iteration produced.
    async fn monitor_iteration(&mut self, response: &AssembledResponse) -> Result<(), NornError> {
        if let Some(monitor_cfg) = self.loop_context.iteration_monitor.as_ref() {
            let latest_text = if response.text.is_empty() {
                None
            } else {
                Some(response.text.as_str())
            };
            // REVIEW item 4: drain the failures the previous iteration
            // produced (tool errors, schema-validation failures) into the
            // monitor so RepeatedFailure detection has real input.
            let failures = std::mem::take(&mut self.latest_failures);
            let signals = evaluate_iteration(
                &mut self.iteration_state,
                &self.total_usage,
                latest_text,
                Some(&failures),
                monitor_cfg,
            );
            handle_iteration_signals(
                self.store,
                &mut self.messages,
                signals,
                self.loop_context.hooks.as_deref(),
            )
            .await?;
        }
        Ok(())
    }
}
