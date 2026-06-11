//! Registry merging — combine two [`HookRegistry`] instances without
//! `Arc::try_unwrap` tricks or silent hook loss.
//!
//! Two merge strategies exist:
//!
//! - [`HookRegistry::merge`] (defined alongside the registry in
//!   [`super::traits`]) consumes another registry by value and appends
//!   every hook onto `self`, preserving each registry's internal order.
//! - [`HookRegistry::merge_shared`] (defined here) folds in a *shared*
//!   registry (`Arc<HookRegistry>`) by registering per-category forwarding
//!   hooks that delegate to the shared registry's own dispatch methods.
//!   This is the no-loss path for callers that retain outstanding `Arc`
//!   clones of their programmatic registry.
//!
//! In both cases dispatch semantics are preserved: hooks run in
//! registration order, and the first [`HookOutcome::Block`] wins. Hooks
//! merged into `self` *after* existing registrations therefore lose
//! conflicts against the hooks already present — the caller decides
//! precedence by choosing merge order.

use std::sync::Arc;

use async_trait::async_trait;

use super::new_traits::{
    CompactionHook, PostToolFailureHook, SessionLifecycleHook, StopHook, SubagentHook,
    UserPromptHook,
};
use super::traits::{
    Hook, HookOutcome, HookRegistry, LlmCallSummary, PostLlmHook, PostToolHook, PreLlmHook,
    PreToolHook, SessionEventHook,
};
use crate::provider::request::ProviderRequest;
use crate::session::events::SessionEvent;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

impl HookRegistry {
    /// Fold a *shared* registry into `self` by registering forwarding hooks
    /// that delegate to `other`'s own dispatch methods.
    ///
    /// One forwarding hook is registered per category that `other` actually
    /// populates, so empty categories add nothing. The shared registry's
    /// internal ordering and first-`Block`-wins semantics are preserved
    /// inside each forwarded dispatch; relative to hooks already registered
    /// on `self`, the forwarded hooks run after (and therefore lose
    /// conflicts against) the existing ones.
    ///
    /// This is the honest fallback for merging an `Arc<HookRegistry>` whose
    /// owner retains outstanding clones — no `Arc::try_unwrap`, no silent
    /// substitution of an empty registry.
    pub fn merge_shared(&mut self, other: Arc<Self>) {
        if other.pre_tool_len() > 0 {
            self.register(Hook::PreTool(Box::new(ForwardingHooks(Arc::clone(&other)))));
        }
        if other.post_tool_len() > 0 {
            self.register(Hook::PostTool(Box::new(ForwardingHooks(Arc::clone(
                &other,
            )))));
        }
        if other.pre_llm_len() > 0 {
            self.register(Hook::PreLlm(Box::new(ForwardingHooks(Arc::clone(&other)))));
        }
        if other.post_llm_len() > 0 {
            self.register(Hook::PostLlm(Box::new(ForwardingHooks(Arc::clone(&other)))));
        }
        if other.session_event_len() > 0 {
            self.register(Hook::SessionEvent(Box::new(ForwardingHooks(Arc::clone(
                &other,
            )))));
        }
        if other.user_prompt_len() > 0 {
            self.register(Hook::UserPrompt(Box::new(ForwardingHooks(Arc::clone(
                &other,
            )))));
        }
        if other.stop_len() > 0 {
            self.register(Hook::Stop(Box::new(ForwardingHooks(Arc::clone(&other)))));
        }
        if other.subagent_len() > 0 {
            self.register(Hook::Subagent(Box::new(ForwardingHooks(Arc::clone(
                &other,
            )))));
        }
        if other.session_lifecycle_len() > 0 {
            self.register(Hook::SessionLifecycle(Box::new(ForwardingHooks(
                Arc::clone(&other),
            ))));
        }
        if other.compaction_len() > 0 {
            self.register(Hook::Compaction(Box::new(ForwardingHooks(Arc::clone(
                &other,
            )))));
        }
        if other.post_tool_failure_len() > 0 {
            self.register(Hook::PostToolFailure(Box::new(ForwardingHooks(other))));
        }
    }
}

/// Forwarding adapter that delegates every hook category to a shared
/// [`HookRegistry`]'s dispatch methods. Registered per-category by
/// [`HookRegistry::merge_shared`].
struct ForwardingHooks(Arc<HookRegistry>);

#[async_trait]
impl PreToolHook for ForwardingHooks {
    async fn before_tool(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> HookOutcome {
        self.0.run_pre_tool(envelope, ctx).await
    }
}

#[async_trait]
impl PostToolHook for ForwardingHooks {
    async fn after_tool(&self, envelope: &ToolEnvelope, output: &ToolOutput, ctx: &ToolContext) {
        self.0.run_post_tool(envelope, output, ctx).await;
    }
}

#[async_trait]
impl PreLlmHook for ForwardingHooks {
    async fn before_llm(&self, request: &ProviderRequest) -> HookOutcome {
        self.0.run_pre_llm(request).await
    }
}

#[async_trait]
impl PostLlmHook for ForwardingHooks {
    async fn after_llm(&self, summary: &LlmCallSummary) {
        self.0.run_post_llm(summary).await;
    }
}

#[async_trait]
impl SessionEventHook for ForwardingHooks {
    async fn on_event(&self, event: &SessionEvent) {
        self.0.run_on_event(event).await;
    }
}

#[async_trait]
impl UserPromptHook for ForwardingHooks {
    async fn on_user_prompt(&self, prompt: &str, session_id: &str) -> HookOutcome {
        self.0.run_user_prompt(prompt, session_id).await
    }
}

#[async_trait]
impl StopHook for ForwardingHooks {
    async fn on_stop(&self, final_text: &str) -> HookOutcome {
        self.0.run_stop(final_text).await
    }
}

#[async_trait]
impl SubagentHook for ForwardingHooks {
    async fn on_subagent_start(&self, agent_id: &str, agent_type: &str) {
        self.0.run_subagent_start(agent_id, agent_type).await;
    }

    async fn on_subagent_stop(&self, agent_id: &str, agent_type: &str) -> HookOutcome {
        self.0.run_subagent_stop(agent_id, agent_type).await
    }
}

#[async_trait]
impl SessionLifecycleHook for ForwardingHooks {
    async fn on_session_start(&self, session_id: &str) {
        self.0.run_session_start(session_id).await;
    }

    async fn on_session_end(&self, session_id: &str) {
        self.0.run_session_end(session_id).await;
    }
}

#[async_trait]
impl CompactionHook for ForwardingHooks {
    async fn before_compaction(&self, event_count: usize) -> HookOutcome {
        self.0.run_pre_compaction(event_count).await
    }
}

#[async_trait]
impl PostToolFailureHook for ForwardingHooks {
    async fn after_tool_failure(
        &self,
        envelope: &ToolEnvelope,
        output: &ToolOutput,
        ctx: &ToolContext,
    ) {
        self.0.run_post_tool_failure(envelope, output, ctx).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Extract the reason from a [`HookOutcome::Block`], `None` otherwise.
    fn block_reason(outcome: HookOutcome) -> Option<String> {
        match outcome {
            HookOutcome::Block { reason } => Some(reason),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => None,
        }
    }

    struct RecordingStop {
        label: &'static str,
        calls: Arc<AtomicUsize>,
        block: bool,
    }

    #[async_trait]
    impl StopHook for RecordingStop {
        async fn on_stop(&self, _final_text: &str) -> HookOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.block {
                HookOutcome::Block {
                    reason: self.label.to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    struct CountingUserPrompt {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl UserPromptHook for CountingUserPrompt {
        async fn on_user_prompt(&self, _prompt: &str, _session_id: &str) -> HookOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Proceed
        }
    }

    /// `merge` moves every hook across by value: both registries' hooks fire
    /// through the merged registry, and the pre-existing hook keeps
    /// first-Block-wins precedence over the merged-in one.
    #[tokio::test]
    async fn merge_by_value_combines_hooks_and_preserves_precedence() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let prompt_calls = Arc::new(AtomicUsize::new(0));

        let mut primary = HookRegistry::new();
        primary.register(Hook::Stop(Box::new(RecordingStop {
            label: "primary",
            calls: Arc::clone(&first_calls),
            block: true,
        })));

        let mut secondary = HookRegistry::new();
        secondary.register(Hook::Stop(Box::new(RecordingStop {
            label: "secondary",
            calls: Arc::clone(&second_calls),
            block: true,
        })));
        secondary.register(Hook::UserPrompt(Box::new(CountingUserPrompt {
            calls: Arc::clone(&prompt_calls),
        })));

        primary.merge(secondary);

        assert_eq!(primary.stop_len(), 2, "both stop hooks present after merge");
        assert_eq!(primary.user_prompt_len(), 1, "user-prompt hook merged in");

        let outcome = primary.run_stop("done").await;
        assert_eq!(
            block_reason(outcome).as_deref(),
            Some("primary"),
            "pre-existing hook must win the first-Block-wins race",
        );
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "merged-in hook must not run after the earlier Block",
        );

        let prompt = primary.run_user_prompt("hello", "session").await;
        assert!(matches!(prompt, HookOutcome::Proceed));
        assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
    }

    /// `merge_shared` keeps every hook of a shared registry reachable —
    /// blocks propagate through the forwarder and outstanding clones of the
    /// shared registry stay valid.
    #[tokio::test]
    async fn merge_shared_forwards_dispatch_to_shared_registry() {
        let shared_calls = Arc::new(AtomicUsize::new(0));
        let mut inner = HookRegistry::new();
        inner.register(Hook::Stop(Box::new(RecordingStop {
            label: "shared",
            calls: Arc::clone(&shared_calls),
            block: true,
        })));
        let shared = Arc::new(inner);
        let outstanding_clone = Arc::clone(&shared);

        let mut merged = HookRegistry::new();
        merged.merge_shared(shared);

        assert_eq!(merged.stop_len(), 1, "one forwarder registered");
        assert_eq!(
            merged.user_prompt_len(),
            0,
            "empty categories register no forwarder",
        );

        let outcome = merged.run_stop("done").await;
        assert_eq!(block_reason(outcome).as_deref(), Some("shared"));
        assert_eq!(shared_calls.load(Ordering::SeqCst), 1);

        // The clone the caller retained still dispatches independently.
        let direct = outstanding_clone.run_stop("again").await;
        assert!(matches!(direct, HookOutcome::Block { .. }));
        assert_eq!(shared_calls.load(Ordering::SeqCst), 2);
    }

    /// Hooks registered on `self` before `merge_shared` keep precedence over
    /// the forwarded shared registry (first-Block-wins ordering).
    #[tokio::test]
    async fn merge_shared_existing_hooks_win_conflicts() {
        let own_calls = Arc::new(AtomicUsize::new(0));
        let shared_calls = Arc::new(AtomicUsize::new(0));

        let mut inner = HookRegistry::new();
        inner.register(Hook::Stop(Box::new(RecordingStop {
            label: "shared",
            calls: Arc::clone(&shared_calls),
            block: true,
        })));

        let mut merged = HookRegistry::new();
        merged.register(Hook::Stop(Box::new(RecordingStop {
            label: "own",
            calls: Arc::clone(&own_calls),
            block: true,
        })));
        merged.merge_shared(Arc::new(inner));

        let outcome = merged.run_stop("done").await;
        assert_eq!(block_reason(outcome).as_deref(), Some("own"));
        assert_eq!(own_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            shared_calls.load(Ordering::SeqCst),
            0,
            "forwarded hooks must not run after the earlier Block",
        );
    }
}
