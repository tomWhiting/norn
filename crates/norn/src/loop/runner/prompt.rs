//! Request-build phase: per-iteration dynamic prompt sections, session
//! variable expansion, the context preflight (token estimation and
//! auto-compaction), and provider request construction.

use std::sync::Arc;

use crate::agent::fork::ParentPromptPlan;
use crate::error::NornError;
use crate::r#loop::expansion::{expand_system_instruction, expand_tool_descriptions};
use crate::r#loop::helpers::apply_rule_injections;
use crate::r#loop::inflight_compaction::{
    InFlightPromptLayout, PreflightArgs, run_context_preflight,
};
use crate::provider::agent_event::AgentUsageEstimate;
use crate::provider::request::{Message, MessageRole, ProviderRequest, ToolCallCaller};
use crate::provider::surface::{ResolvedToolSurface, hosted_tools_prompt_section};
use crate::system_prompt::{PromptSeedFingerprint, PromptSource};

use super::machine::{PromptCommandContextState, StepFlow, StepMachine, StepState};

impl StepMachine<'_> {
    /// Assemble the dynamic prompt view and build the provider request.
    pub(super) async fn build_request(&mut self) -> Result<StepFlow, NornError> {
        // Rules cleared at the top of each iteration so re-firings produce
        // fresh dynamic sections rather than accumulating duplicates.
        self.loop_context.clear_dynamic_sections();

        // Detach the previous iteration's managed tail before any stable
        // prefix replacement can shift its recorded index. The detached live
        // conversation maps 1:1 to stable prompt plus persisted events for
        // both seed reconciliation and preflight compaction.
        self.dev_message
            .detach(&mut self.messages, &mut self.conversation_state);

        // Capture one coherent executable/model-facing generation at the
        // request boundary. The snapshot remains installed until dispatch of
        // this response completes; only the next request build may replace it.
        self.tool_snapshot = self.executor.execution_snapshot();
        self.all_tools = self.tool_snapshot.as_ref().map_or_else(
            || self.static_tools.clone(),
            |snapshot| snapshot.definitions.as_ref().to_vec(),
        );
        self.all_tools.extend(self.schema_tool.iter().cloned());

        let prompt_command_state = std::mem::replace(
            &mut self.prompt_command_context,
            PromptCommandContextState::Pending,
        );
        let first_request = matches!(
            &prompt_command_state,
            PromptCommandContextState::Prepared(_)
        );

        // NX-005 R6: re-stat the always-on NORN.md layers between
        // `clear_dynamic_sections` and `evaluate_prompt_commands`. When
        // staleness is detected, rebuild `system_sections[0]` so the
        // freshly-read content takes effect this iteration. The two
        // `stat` syscalls per iteration are unconditional but cheap; an
        // absent loader short-circuits inside `refresh_context_if_stale`.
        // Setup already froze and validated the first request's stable source
        // snapshot. Re-stat only on later iterations, when a detected change
        // is reconciled and replay-validated before any further dispatch.
        if !first_request && self.loop_context.refresh_context_if_stale() {
            self.loop_context.rebuild_base_section();
        }

        // Child launch tools read their inherited authority from the executor
        // context, not directly from LoopContext. Keep both the outer executor
        // and this request's exact generation lease synchronized before any
        // response can dispatch a fork or spawn. Fork identity is request-local
        // authority and must never become the next generation's inherited base.
        self.publish_parent_prompt_plan();

        self.loop_context.inject_environment_section();
        self.loop_context.inject_collaboration_mode();

        // Provider tool surface, recomputed every iteration from the live
        // provider's capabilities — the same cadence as the wire resolution
        // below — so a provider rebind (or a launch path whose static
        // prompt was assembled before the provider was bound) can never
        // leave a stale function-style framing of a hosted tool standing.
        // `None` (nothing hosted) injects nothing: function mode is what
        // the static tools section already describes.
        if let Some(section) =
            hosted_tools_prompt_section(&self.all_tools, self.provider.capabilities())
        {
            self.loop_context.append_system_section(section);
        }

        // Setup prepared the first request's exact value and, for threaded
        // providers, replay-validated it as part of the seed before prompt
        // persistence. Stateless providers validate the complete managed tail
        // below after preflight. Later requests evaluate the then-current
        // definition after their iteration gate. No request executes a command
        // more than once.
        let managed_developer_context = match prompt_command_state {
            PromptCommandContextState::Prepared(content) => content,
            PromptCommandContextState::Pending => {
                self.loop_context
                    .prepare_prompt_command_context(self.config.prompt_command_timeout)
                    .await
            }
        };

        // Apply any Before-timing injections accumulated by the previous
        // iteration's tool batch. These must hit the prompt before the next
        // provider call.
        if !self.pending_before_injections.is_empty() {
            let injections = std::mem::take(&mut *self.pending_before_injections);
            apply_rule_injections(
                &mut *self.loop_context,
                injections,
                &mut self.messages,
                self.store,
            )
            .await?;
        }

        // Build the two fresh managed-context authority channels, expanding
        // session-variable placeholders (R5). Stable prompt fragments are not
        // expanded and remain byte-stable for caching. Norn-owned runtime
        // policy stays System; trusted prompt-command output stays Developer.
        let managed_system_context = match self.loop_context.managed_system_context() {
            Some(content) => Some(match self.loop_context.variables.as_ref() {
                Some(var_store) => expand_system_instruction(&content, var_store).await,
                None => content,
            }),
            None => None,
        };

        // Re-materialize the exact prefix at every request boundary. Stable
        // Developer/User fragments and current operator command output bind the
        // threaded seed. A change cuts V2 and replays under the new authority;
        // unchanged output is sent once and inherited by the provider anchor.
        // System-only changes remain request-local instructions and preserve
        // the anchor. Stateless transports keep all volatile content at the
        // tail, so their cacheable prefix is unaffected.
        let mut prompt_seed_fingerprint = self.loop_context.stable_prompt_plan().map_or_else(
            PromptSeedFingerprint::empty,
            PromptSeedFingerprint::from_plan,
        );
        let mut stable_prefix = self.loop_context.stable_prompt_messages();
        if self.conversation_state.store()
            && let Some(content) = managed_developer_context.as_ref()
        {
            prompt_seed_fingerprint =
                prompt_seed_fingerprint.with_operator_runtime_context(content);
            stable_prefix.push(developer_message(content.clone()));
        }
        self.conversation_state.sync_stable_prefix(
            &mut self.messages,
            stable_prefix,
            prompt_seed_fingerprint,
        );
        self.provider
            .validate_replay(&self.conversation_state.request_messages(&self.messages))?;

        let stateless_managed_tail = (!self.conversation_state.store())
            .then(|| {
                join_managed_context(
                    managed_system_context.as_deref(),
                    managed_developer_context.as_deref(),
                )
            })
            .flatten();

        // Request-local Responses instructions and the stateless tail remain out
        // of `self.messages` during preflight, so account for their token cost
        // explicitly. Threaded Developer context is already in the prefix and
        // is counted by the ordinary message estimator.
        let out_of_band_context = if self.conversation_state.store() {
            managed_system_context.as_ref()
        } else {
            stateless_managed_tail.as_ref()
        };
        let managed_context_tokens = match (
            self.loop_context.token_estimator.as_ref(),
            out_of_band_context,
        ) {
            (Some(estimator), Some(content)) => estimator.estimate(content),
            _ => 0,
        };

        // R5: expand tool descriptions before the request is built.
        let iteration_tools = if let Some(var_store) = self.loop_context.variables.as_ref() {
            expand_tool_descriptions(&self.all_tools, var_store).await
        } else {
            self.all_tools.clone()
        };

        let provider_tools =
            ResolvedToolSurface::resolve(&iteration_tools, self.provider.capabilities())
                .provider_definitions();

        // R3 + R4 + REVIEW 6b: token estimation, the token-warning event,
        // the auto-compaction trigger (including the LLM summarization
        // call), and in-flight application of a fired compaction. The
        // request message list is built *after* the preflight so the
        // current provider call already sees the compacted view and any
        // dropped response-thread anchor, not just the next step.
        //
        // Read into a local before the args block mutably borrows the state.
        let layout_prefix_len = self.conversation_state.prefix_len();
        let preflight = run_context_preflight(PreflightArgs {
            store: self.store,
            provider: self.provider,
            model: self.model,
            messages: &mut self.messages,
            iteration_tools: &iteration_tools,
            conversation_state: &mut self.conversation_state,
            loop_context: &mut *self.loop_context,
            config: self.config,
            compaction_state: &mut self.compaction_state,
            layout: InFlightPromptLayout {
                prefix_len: layout_prefix_len,
                prompt_event_id: self.prompt_event_id.clone(),
                prompt_message_len: self.new_input_len,
            },
            managed_context_tokens,
            cancel: self.cancel.as_ref(),
            event_tx: self.event_tx,
        })
        .await?;
        // Summarization tokens are real provider spend: account them
        // exactly like any other provider call in this step.
        if let Some(usage) = preflight.summarization_usage {
            self.total_usage += usage;
            self.timeout_state.lock().usage = self.total_usage.clone();
        }
        if let (Some(sender), Some(input_tokens)) =
            (self.event_tx, preflight.request_input_estimate)
        {
            sender.send_usage_estimate(AgentUsageEstimate {
                input_tokens: u64::try_from(input_tokens).unwrap_or(u64::MAX),
            });
        }

        // Provider-threaded Responses projects only Norn-owned managed policy
        // through top-level `instructions`; it does not inherit prior
        // instructions through `previous_response_id`. Operator command output
        // remains a seed-bound Developer prefix item. Stateless transports
        // explicitly lower both channels into one cache-friendly Developer
        // tail and preserve their order without promoting either channel.
        let managed_instructions = if self.conversation_state.store() {
            managed_system_context
        } else {
            if let Some(content) = stateless_managed_tail {
                self.dev_message.attach(content, &mut self.messages);
            }
            None
        };

        let request_messages = self
            .conversation_state
            .request_messages_with_managed_instructions(&self.messages, managed_instructions);
        self.provider.validate_replay(&request_messages)?;

        let request = ProviderRequest {
            messages: request_messages,
            tools: provider_tools,
            model: self.model.to_string(),
            reasoning_effort: self.loop_context.reasoning_effort,
            reasoning_summary: self.loop_context.reasoning_summary.clone(),
            service_tier: self.loop_context.service_tier,
            config: None,
            cache_key: self.config.cache_key.clone(),
            previous_response_id: self.conversation_state.previous_response_id(),
            store: self.conversation_state.store(),
            context_management: self.conversation_state.context_management(self.config),
        };

        Ok(StepFlow::Next(StepState::CallProvider(Box::new(request))))
    }

    fn publish_parent_prompt_plan(&self) {
        let mut inherited = ParentPromptPlan::from_loop_context(self.loop_context)
            .plan()
            .clone();
        inherited.remove(PromptSource::ForkAgentPolicy);
        let parent_prompt = ParentPromptPlan::new(inherited);

        if let Some(shared) = self.executor.shared_context() {
            shared.insert_extension(Arc::new(parent_prompt.clone()));
        }
        if let Some(shared) = self
            .tool_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.executor.shared_context())
        {
            shared.insert_extension(Arc::new(parent_prompt));
        }
    }
}

pub(super) fn developer_message(content: String) -> Message {
    Message {
        response_items: Vec::new(),
        role: MessageRole::Developer,
        content: Some(content),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: ToolCallCaller::Absent,
    }
}

fn join_managed_context(system: Option<&str>, developer: Option<&str>) -> Option<String> {
    match (system, developer) {
        (Some(system), Some(developer)) => Some(format!("{system}\n\n{developer}")),
        (Some(system), None) => Some(system.to_owned()),
        (None, Some(developer)) => Some(developer.to_owned()),
        (None, None) => None,
    }
}
