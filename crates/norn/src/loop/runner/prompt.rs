//! Request-build phase: per-iteration dynamic prompt sections, session
//! variable expansion, the context preflight (token estimation and
//! auto-compaction), and provider request construction.

use crate::error::NornError;
use crate::r#loop::expansion::{expand_system_instruction, expand_tool_descriptions};
use crate::r#loop::helpers::apply_rule_injections;
use crate::r#loop::inflight_compaction::{
    InFlightPromptLayout, PreflightArgs, run_context_preflight,
};
use crate::provider::agent_event::AgentUsageEstimate;
use crate::provider::request::ProviderRequest;
use crate::provider::surface::{
    ResolvedToolSurface, hosted_tools_prompt_section, reframe_prompt_entries,
};
use crate::system_prompt::builder::build_tool_prompt_section;

use super::machine::{StepFlow, StepMachine, StepState};

impl StepMachine<'_> {
    /// Assemble the dynamic prompt view and build the provider request.
    pub(super) async fn build_request(&mut self) -> Result<StepFlow, NornError> {
        // Rules cleared at the top of each iteration so re-firings produce
        // fresh dynamic sections rather than accumulating duplicates.
        self.loop_context.clear_dynamic_sections();

        // Capture one coherent executable/model-facing generation at the
        // request boundary. The snapshot remains installed until dispatch of
        // this response completes; only the next request build may replace it.
        self.tool_snapshot = self.executor.execution_snapshot();
        self.all_tools = self.tool_snapshot.as_ref().map_or_else(
            || self.static_tools.clone(),
            |snapshot| snapshot.definitions.as_ref().to_vec(),
        );
        self.all_tools.extend(self.schema_tool.iter().cloned());

        if let Some(snapshot) = self.tool_snapshot.as_ref() {
            let entries = reframe_prompt_entries(
                snapshot.dynamic_prompt_entries.as_ref().to_vec(),
                self.provider.capabilities(),
            );
            if let Some(section) = build_tool_prompt_section(&entries) {
                self.loop_context.append_system_section(section);
            }
        }

        // NX-005 R6: re-stat the always-on NORN.md layers between
        // `clear_dynamic_sections` and `evaluate_prompt_commands`. When
        // staleness is detected, rebuild `system_sections[0]` so the
        // freshly-read content takes effect this iteration. The two
        // `stat` syscalls per iteration are unconditional but cheap; an
        // absent loader short-circuits inside `refresh_context_if_stale`.
        if self.loop_context.refresh_context_if_stale() {
            self.loop_context.rebuild_base_section();
        }

        self.loop_context.inject_environment_section();
        self.loop_context.inject_collaboration_mode();

        // N-007 R3 / N-017 R3: SystemContextAppend rules persist "for the
        // remainder of the session" by being re-materialized from their
        // persisted RuleInjection events every iteration — after the
        // per-iteration wipe above, before the developer message is synced
        // below — rather than surviving the wipe in place. A rule whose
        // event has been compacted out simply stops re-materializing and
        // re-fires on its next trigger.
        self.loop_context
            .materialize_system_context_rules(self.store);

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

        // Evaluate runtime prompt commands before applying Before-timing
        // rule injections so their stdout becomes part of the dynamic
        // section stack the rest of this iteration builds on. Failures are
        // logged inside the helper and produce no section.
        self.loop_context
            .evaluate_prompt_commands(self.config.prompt_command_timeout)
            .await;

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

        // Detach the previous iteration's managed dynamic-context Developer
        // message BEFORE the preflight (PROMPT-CACHE fix). While detached the
        // live conversation past the System prefix maps 1:1 to persisted
        // events, which the token estimate and the in-flight compaction walk
        // rely on. The message is re-attached at the tail AFTER the preflight
        // (below), so messages[0] (System) + history form one stable,
        // fully-cacheable prefix and the volatile dynamic context is the last
        // message the model sees — never overwriting a history Developer
        // compaction summary, which lives in history and is left untouched.
        self.dev_message
            .detach(&mut self.messages, &mut self.conversation_state);

        // Build the fresh managed message content from the current dynamic
        // context, expanding session-variable placeholders (R5). The System
        // message (messages[0]) is NOT expanded so it stays byte-stable for
        // caching. Empty dynamic context yields no message — an empty
        // Developer message would read to the model as a prompt — so this
        // iteration simply attaches nothing.
        let dev_tail_content: Option<String> = match self.loop_context.dynamic_context() {
            Some(content) => Some(match self.loop_context.variables.as_ref() {
                Some(var_store) => expand_system_instruction(&content, var_store).await,
                None => content,
            }),
            None => None,
        };

        // The managed message goes over the wire at the tail, after the
        // preflight — so it is absent from `self.messages` during estimation.
        // Feed its token cost in explicitly so the token warning and the
        // auto-compaction trigger account for what actually ships.
        let dev_tail_tokens = match (
            self.loop_context.token_estimator.as_ref(),
            dev_tail_content.as_ref(),
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
            dev_tail_tokens,
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

        // Provider-threaded Responses state uses top-level `instructions` for
        // the managed context. The API does not inherit prior instructions
        // through `previous_response_id`, so each request replaces the old
        // dynamic context without dropping the provider-owned thread. The
        // stateless Codex path retains the cache-friendly tail Developer item.
        let managed_instructions = if self.conversation_state.store() {
            dev_tail_content
        } else {
            if let Some(content) = dev_tail_content {
                self.dev_message.attach(content, &mut self.messages);
            }
            None
        };

        let request_messages = self
            .conversation_state
            .request_messages_with_managed_instructions(&self.messages, managed_instructions);

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
}
