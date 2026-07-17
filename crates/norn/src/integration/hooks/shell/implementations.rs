//! Hook-trait adapters for the shared shell execution engine.

use async_trait::async_trait;

use super::{ShellCommandHook, session_event_variant_name};
use crate::integration::hooks::config::HookEventType;
use crate::integration::hooks::matchers::HookMatcher;
use crate::integration::hooks::new_traits::{
    CompactionHook, PostToolFailureHook, SessionLifecycleHook, StopHook, SubagentHook,
    UserPromptHook,
};
use crate::integration::hooks::traits::{
    HookOutcome, LlmCallSummary, PostLlmHook, PostToolHook, PreLlmHook, PreToolHook,
    SessionEventHook,
};
use crate::provider::request::ProviderRequest;
use crate::session::events::SessionEvent;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

#[async_trait]
impl PreToolHook for ShellCommandHook {
    async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
        if !self.should_fire(Some(envelope.tool_name.as_str())) {
            return HookOutcome::Proceed;
        }
        let mut input = self.base_input();
        input.tool_name = Some(envelope.tool_name.clone());
        input.tool_input = Some(envelope.model_args.clone());
        input.tool_call_id = Some(envelope.tool_call_id.clone());
        self.execute(input).await
    }
}

#[async_trait]
impl PostToolHook for ShellCommandHook {
    async fn after_tool(&self, envelope: &ToolEnvelope, output: &ToolOutput, _ctx: &ToolContext) {
        if !self.should_fire(Some(envelope.tool_name.as_str())) {
            return;
        }
        let mut input = self.tool_result_input(envelope, output);
        input.tool_is_error = Some(output.is_error());
        let _ = self.execute(input).await;
    }
}

#[async_trait]
impl PreLlmHook for ShellCommandHook {
    async fn before_llm(&self, request: &ProviderRequest) -> HookOutcome {
        if !self.should_fire(Some(request.model.as_str())) {
            return HookOutcome::Proceed;
        }
        let mut input = self.base_input();
        input.model = Some(request.model.clone());
        input.message_count = Some(request.messages.len());
        self.execute(input).await
    }
}

#[async_trait]
impl PostLlmHook for ShellCommandHook {
    async fn after_llm(&self, _summary: &LlmCallSummary) {
        // The current summary does not carry a model identifier, so a
        // concrete model matcher cannot be evaluated honestly.
        if matches!(self.matcher, HookMatcher::Pattern(_)) {
            return;
        }
        let _ = self.execute(self.base_input()).await;
    }
}

#[async_trait]
impl SessionEventHook for ShellCommandHook {
    async fn on_event(&self, event: &SessionEvent) {
        let variant = session_event_variant_name(event);
        if !self.should_fire(Some(variant)) {
            return;
        }
        let mut input = self.base_input();
        input.tool_name = Some(variant.to_owned());
        let hook = self.clone();
        tokio::spawn(async move {
            let _ = hook.execute(input).await;
        });
    }
}

#[async_trait]
impl UserPromptHook for ShellCommandHook {
    async fn on_user_prompt(&self, prompt: &str, session_id: &str) -> HookOutcome {
        let mut input = self.base_input();
        session_id.clone_into(&mut input.session_id);
        input.final_text = Some(prompt.to_owned());
        self.execute(input).await
    }
}

#[async_trait]
impl StopHook for ShellCommandHook {
    async fn on_stop(&self, final_text: &str) -> HookOutcome {
        let mut input = self.base_input();
        input.final_text = Some(final_text.to_owned());
        self.execute(input).await
    }
}

#[async_trait]
impl SubagentHook for ShellCommandHook {
    async fn on_subagent_start(&self, agent_id: &str, agent_type: &str) {
        if self.event_type != HookEventType::SubagentStart || !self.should_fire(Some(agent_type)) {
            return;
        }
        let mut input = self.base_input();
        input.subagent_id = Some(agent_id.to_owned());
        input.subagent_type = Some(agent_type.to_owned());
        let _ = self.execute(input).await;
    }

    async fn on_subagent_stop(&self, agent_id: &str, agent_type: &str) -> HookOutcome {
        if self.event_type != HookEventType::SubagentStop || !self.should_fire(Some(agent_type)) {
            return HookOutcome::Proceed;
        }
        let mut input = self.base_input();
        input.subagent_id = Some(agent_id.to_owned());
        input.subagent_type = Some(agent_type.to_owned());
        self.execute(input).await
    }
}

#[async_trait]
impl SessionLifecycleHook for ShellCommandHook {
    async fn on_session_start(&self, session_id: &str) {
        if self.event_type != HookEventType::SessionStart {
            return;
        }
        let mut input = self.base_input();
        session_id.clone_into(&mut input.session_id);
        let _ = self.execute(input).await;
    }

    async fn on_session_end(&self, session_id: &str) {
        if self.event_type != HookEventType::SessionEnd {
            return;
        }
        let mut input = self.base_input();
        session_id.clone_into(&mut input.session_id);
        let _ = self.execute(input).await;
    }
}

#[async_trait]
impl CompactionHook for ShellCommandHook {
    async fn before_compaction(&self, event_count: usize) -> HookOutcome {
        let mut input = self.base_input();
        input.message_count = Some(event_count);
        self.execute(input).await
    }
}

#[async_trait]
impl PostToolFailureHook for ShellCommandHook {
    async fn after_tool_failure(
        &self,
        envelope: &ToolEnvelope,
        output: &ToolOutput,
        _ctx: &ToolContext,
    ) {
        if !self.should_fire(Some(envelope.tool_name.as_str())) {
            return;
        }
        let mut input = self.tool_result_input(envelope, output);
        input.tool_is_error = Some(output.is_error());
        let _ = self.execute(input).await;
    }
}
