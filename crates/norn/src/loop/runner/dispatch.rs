//! Response-dispatch phase: route the classified provider response
//! through tool batches, schema enforcement (acceptance, validation
//! feedback, nudges), and truncation handling.

use serde_json::Value;

use crate::error::{NornError, ProviderError, SchemaError};
use crate::integration::diagnostics::NornDiagnostic;
use crate::r#loop::assembly::AssembledResponse;
use crate::r#loop::classify::{ResponseClass, classify_response, record_truncation};
use crate::r#loop::config::{AgentStepResult, TruncationKind};
use crate::r#loop::delivery::{drain_post_batch_inbound, flush_active_inputs};
use crate::r#loop::failure_tracking::collect_tool_failures;
use crate::r#loop::helpers::{
    ToolBatchRequest, ToolResultRecord, accept_schema_tool_call, append_and_notify,
    append_tool_result, execute_tool_batch, inject_post_tool_batch_notifications,
    reject_post_schema_tools,
};
use crate::r#loop::schema::{format_nudge, format_validation_feedback};
use crate::provider::request::{Message, MessageRole};
use crate::session::events::{EventBase, SessionEvent};

use super::machine::{StepFlow, StepMachine, StepState};
use super::stop::StopOutput;

impl StepMachine<'_> {
    /// Classify the provider response and route it to the matching arm.
    pub(super) async fn dispatch(
        &mut self,
        response: AssembledResponse,
    ) -> Result<StepFlow, NornError> {
        match classify_response(&response, self.output_schema, &self.config.schema_tool_name) {
            ResponseClass::SchemaValid { output } => self.on_schema_valid(response, output).await,
            ResponseClass::SchemaInvalid {
                output,
                errors,
                schema_call_index,
            } => {
                self.on_schema_invalid(&response, output, errors, schema_call_index)
                    .await
            }
            ResponseClass::ToolsOnly { tool_calls } => {
                self.on_tools_only(&response, tool_calls).await
            }
            ResponseClass::TextStopNoSchema => self.on_text_stop(response).await,
            ResponseClass::Truncated { kind } => self.on_truncated(&response, kind).await,
            ResponseClass::ToolsAndSchemaValid {
                pre_schema_tools,
                output,
            } => {
                self.on_tools_and_schema(response, pre_schema_tools, output)
                    .await
            }
        }
    }

    /// The response is a lone valid schema call: accept it and head to
    /// the stop boundary with the structured output.
    async fn on_schema_valid(
        &mut self,
        response: AssembledResponse,
        output: Value,
    ) -> Result<StepFlow, NornError> {
        accept_schema_tool_call(
            self.store,
            &mut self.messages,
            &response,
            &self.config.schema_tool_name,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
            self.inline_char_limit,
        )
        .await?;

        Ok(StepFlow::Next(StepState::ResolveStop(
            StopOutput::Structured {
                output,
                text: response.text,
            },
        )))
    }

    /// The schema call failed validation: consume one attempt, answer
    /// every tool call, and retry (or give up when the budget is spent).
    async fn on_schema_invalid(
        &mut self,
        response: &AssembledResponse,
        output: Value,
        errors: Vec<String>,
        schema_call_index: usize,
    ) -> Result<StepFlow, NornError> {
        self.budget_consumed += 1;
        self.best_attempt = Some(output.clone());
        if self.loop_context.iteration_monitor.is_some() {
            self.latest_failures.extend(errors.iter().cloned());
        }

        if let Some(collector) = self.loop_context.diagnostics.as_ref() {
            let schema_err = SchemaError::ValidationFailed {
                schema: self.output_schema.cloned().unwrap_or(Value::Null),
                output: output.clone(),
                errors: errors.clone(),
            };
            collector.report(NornDiagnostic::from_schema_error(&schema_err));
        }

        if self.budget_consumed >= self.config.schema_attempt_budget {
            return Ok(StepFlow::Done(self.schema_unreachable_result(errors)));
        }

        self.run_tool_batch(response, (0..schema_call_index).collect())
            .await?;

        let schema = self.output_schema.ok_or_else(|| {
            NornError::Provider(ProviderError::StreamError {
                reason: "schema unexpectedly missing".to_string(),
                transient: None,
            })
        })?;
        let feedback = format_validation_feedback(schema, &output, &errors);
        let schema_tc = &response.tool_calls[schema_call_index];
        append_tool_result(
            self.store,
            &mut self.messages,
            ToolResultRecord {
                tool_call_id: &schema_tc.call_id,
                tool_name: &self.config.schema_tool_name,
                kind: schema_tc.kind,
                output: &Value::String(feedback),
                duration_ms: 0,
                inline_char_limit: self.inline_char_limit,
            },
            self.loop_context.hooks.as_deref(),
            self.event_tx,
        )
        .await?;

        // REVIEW H3: tool calls the model placed *after* the schema
        // call must also receive exactly one result each, mirroring
        // the `ToolsAndSchemaValid` arm — otherwise the next request
        // carries unanswered calls and the provider rejects it,
        // permanently wedging the retry loop.
        reject_post_schema_tools(
            self.store,
            &mut self.messages,
            response,
            &self.config.schema_tool_name,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
            self.inline_char_limit,
        )
        .await?;

        // Steers inject immediately after the batch (inbound
        // contract), once every call has its result — the retry
        // iteration this arm continues into must already see
        // them, exactly as the ToolsOnly arm does.
        drain_post_batch_inbound(
            self.store,
            &mut self.messages,
            self.inbound.as_deref_mut(),
            &mut *self.follow_up_buffer,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
        )
        .await?;

        Ok(StepFlow::Next(StepState::Gate))
    }

    /// Plain tool batch: execute it, run the post-batch injections, and
    /// continue the loop.
    async fn on_tools_only(
        &mut self,
        response: &AssembledResponse,
        tool_calls: Vec<usize>,
    ) -> Result<StepFlow, NornError> {
        self.run_tool_batch(response, tool_calls).await?;

        let static_executor = self.executor;
        let leased_executor = self.cycle_executor();
        let executor = leased_executor.as_ref().map_or(static_executor, |leased| {
            leased as &dyn crate::r#loop::config::ToolExecutor
        });
        inject_post_tool_batch_notifications(executor, false).await;

        drain_post_batch_inbound(
            self.store,
            &mut self.messages,
            self.inbound.as_deref_mut(),
            &mut *self.follow_up_buffer,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
        )
        .await?;
        flush_active_inputs(
            self.store,
            &mut self.messages,
            self.loop_context.active_input_rx.as_mut(),
            self.loop_context.hooks.as_deref(),
        )
        .await?;

        Ok(StepFlow::Next(StepState::Gate))
    }

    /// The model stopped with plain text: complete (no schema) or nudge
    /// it toward the schema tool (schema configured).
    async fn on_text_stop(&mut self, response: AssembledResponse) -> Result<StepFlow, NornError> {
        if self.output_schema.is_none() {
            return Ok(StepFlow::Next(StepState::ResolveStop(StopOutput::Text(
                response.text,
            ))));
        }

        self.budget_consumed += 1;

        if self.budget_consumed >= self.config.schema_attempt_budget {
            return Ok(StepFlow::Done(self.schema_unreachable_result(vec![
                "model stopped without calling schema tool".to_string(),
            ])));
        }

        let schema = self.output_schema.ok_or_else(|| {
            NornError::Provider(ProviderError::StreamError {
                reason: "schema unexpectedly missing during nudge".to_string(),
                transient: None,
            })
        })?;
        let nudge_text = format_nudge(&self.config.schema_tool_name, schema);

        append_and_notify(
            self.store,
            SessionEvent::UserMessage {
                base: EventBase::new(self.store.last_event_id()),
                content: nudge_text.clone(),
            },
            self.loop_context.hooks.as_deref(),
        )
        .await?;

        self.messages.push(Message {
            role: MessageRole::User,
            content: Some(nudge_text),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });

        Ok(StepFlow::Next(StepState::Gate))
    }

    /// REVIEW item 5: a `MaxTokens`/`ContentFilter` stop with no
    /// tool calls in no-schema mode is an incomplete fragment.
    /// Returning it as `Completed` made truncation indistinguishable
    /// from success. A truncated run is a *stopped run with partial
    /// output*, not a transport error, so it returns the typed
    /// `Truncated` stop outcome carrying the partial text and the
    /// accumulated usage; the full fragment and stop reason are also
    /// persisted on the `AssistantMessage` and `loop.truncated`
    /// events.
    async fn on_truncated(
        &mut self,
        response: &AssembledResponse,
        kind: TruncationKind,
    ) -> Result<StepFlow, NornError> {
        record_truncation(
            self.store,
            self.loop_context.hooks.as_deref(),
            kind,
            &response.text,
            self.iterations,
        )
        .await?;
        Ok(StepFlow::Done(AgentStepResult::Truncated {
            kind,
            partial_text: (!response.text.is_empty()).then(|| response.text.clone()),
            iterations: self.iterations,
            usage: std::mem::take(&mut self.total_usage),
            children_usage: self.loop_context.children_usage.snapshot(),
        }))
    }

    /// Tools preceding a valid schema call: execute them, answer the
    /// schema call and any post-schema calls, then head to the stop
    /// boundary unless a steer injected new work.
    async fn on_tools_and_schema(
        &mut self,
        response: AssembledResponse,
        pre_schema_tools: Vec<usize>,
        output: Value,
    ) -> Result<StepFlow, NornError> {
        self.run_tool_batch(&response, pre_schema_tools).await?;

        accept_schema_tool_call(
            self.store,
            &mut self.messages,
            &response,
            &self.config.schema_tool_name,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
            self.inline_char_limit,
        )
        .await?;

        reject_post_schema_tools(
            self.store,
            &mut self.messages,
            &response,
            &self.config.schema_tool_name,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
            self.inline_char_limit,
        )
        .await?;

        // Steers inject immediately after the batch (inbound
        // contract), once every call has its result. An injected
        // steer must reach the model, so the loop continues
        // instead of resolving the stop boundary — mirroring the
        // boundary's own Continue on injected work.
        if drain_post_batch_inbound(
            self.store,
            &mut self.messages,
            self.inbound.as_deref_mut(),
            &mut *self.follow_up_buffer,
            self.loop_context.hooks.as_deref(),
            self.event_tx,
        )
        .await?
        {
            return Ok(StepFlow::Next(StepState::Gate));
        }

        Ok(StepFlow::Next(StepState::ResolveStop(
            StopOutput::Structured {
                output,
                text: response.text,
            },
        )))
    }

    /// Execute one tool batch and fold its side channels into the step
    /// state: Before-timing rule injections buffer for the next request
    /// build, and tool failures feed the iteration monitor.
    async fn run_tool_batch(
        &mut self,
        response: &AssembledResponse,
        tool_indices: Vec<usize>,
    ) -> Result<(), NornError> {
        let failure_watermark = self.store.len();
        let static_executor = self.executor;
        let leased_executor = self.cycle_executor();
        let executor = leased_executor.as_ref().map_or(static_executor, |leased| {
            leased as &dyn crate::r#loop::config::ToolExecutor
        });
        let before = execute_tool_batch(ToolBatchRequest {
            provider: None,
            executor,
            store: self.store,
            messages: &mut self.messages,
            response,
            tool_indices,
            config: self.config,
            loop_context: &mut *self.loop_context,
            event_tx: self.event_tx,
        })
        .await?;
        self.pending_before_injections.extend(before);
        if self.loop_context.iteration_monitor.is_some() {
            self.latest_failures
                .extend(collect_tool_failures(self.store, failure_watermark));
        }
        Ok(())
    }

    /// Build the `SchemaUnreachable` result from the step accumulators.
    fn schema_unreachable_result(&mut self, validation_errors: Vec<String>) -> AgentStepResult {
        AgentStepResult::SchemaUnreachable {
            best_attempt: self.best_attempt.take(),
            validation_errors,
            attempts: self.budget_consumed,
            usage: std::mem::take(&mut self.total_usage),
            children_usage: self.loop_context.children_usage.snapshot(),
        }
    }
}
