//! Additional lifecycle hooks for user prompts, agent stop, sub-agent
//! lifecycle, session lifecycle, compaction, and tool failure.
//!
//! Six hook traits extend the original five (in [`super::traits`]) to cover
//! the lifecycle boundaries that are not wrapped by the tool or LLM hooks:
//!
//! - [`UserPromptHook`] — fires before a user prompt enters the agent loop.
//! - [`StopHook`] — fires when the model would end the turn.
//! - [`SubagentHook`] — fires on sub-agent spawn and completion.
//! - [`SessionLifecycleHook`] — fires on session construction and teardown.
//! - [`CompactionHook`] — fires before automatic compaction.
//! - [`PostToolFailureHook`] — fires when a tool execution reports an error.
//!
//! Blocking hooks return [`HookOutcome::Proceed`] or [`HookOutcome::Block`].
//! [`HookOutcome::Modify`] is reserved for `PreToolHook` and is not produced
//! by any trait in this module.

use async_trait::async_trait;

use super::traits::HookOutcome;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

/// Fires when a user (or orchestrator) prompt enters the agent loop. Can veto
/// the prompt via [`HookOutcome::Block`].
#[async_trait]
pub trait UserPromptHook: Send + Sync {
    /// Invoked with the submitted prompt text and the current session id.
    async fn on_user_prompt(&self, prompt: &str, session_id: &str) -> HookOutcome;
}

/// Fires when the model would stop. Can veto the stop via
/// [`HookOutcome::Block`], forcing the agent loop to continue.
#[async_trait]
pub trait StopHook: Send + Sync {
    /// Invoked with the model's final text output.
    async fn on_stop(&self, final_text: &str) -> HookOutcome;
}

/// Fires on sub-agent spawn (`on_subagent_start`) and completion
/// (`on_subagent_stop`). Start is observational; stop can block to force the
/// sub-agent to continue.
#[async_trait]
pub trait SubagentHook: Send + Sync {
    /// Invoked when a sub-agent is launched.
    async fn on_subagent_start(&self, agent_id: &str, agent_type: &str);

    /// Invoked when a sub-agent would complete. Returning
    /// [`HookOutcome::Block`] prevents the agent from being marked completed.
    async fn on_subagent_stop(&self, agent_id: &str, agent_type: &str) -> HookOutcome;
}

/// Fires on session construction (`on_session_start`) and teardown
/// (`on_session_end`). Both methods are observational.
#[async_trait]
pub trait SessionLifecycleHook: Send + Sync {
    /// Invoked once at session construction.
    async fn on_session_start(&self, session_id: &str);

    /// Invoked once at session teardown.
    async fn on_session_end(&self, session_id: &str);
}

/// Fires before automatic compaction runs. Can veto compaction via
/// [`HookOutcome::Block`].
#[async_trait]
pub trait CompactionHook: Send + Sync {
    /// Invoked with the current event count immediately before compaction.
    async fn before_compaction(&self, event_count: usize) -> HookOutcome;
}

/// Fires after a tool execution returns an error output. Observational.
#[async_trait]
pub trait PostToolFailureHook: Send + Sync {
    /// Invoked with the envelope and the error-bearing output.
    async fn after_tool_failure(
        &self,
        envelope: &ToolEnvelope,
        output: &ToolOutput,
        ctx: &ToolContext,
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::integration::hooks::traits::{Hook, HookRegistry};
    use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
    use crate::tool::traits::ToolOutput;

    fn make_envelope(name: &str) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "tc_1".to_owned(),
            tool_name: name.to_owned(),
            model_args: serde_json::json!({}),
            runtime_inputs: RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        }
    }

    // R1: UserPromptHook returning Block surfaces via HookOutcome::Block.
    struct RejectIfEmpty;

    #[async_trait]
    impl UserPromptHook for RejectIfEmpty {
        async fn on_user_prompt(&self, prompt: &str, _session_id: &str) -> HookOutcome {
            if prompt.is_empty() {
                HookOutcome::Block {
                    reason: "empty prompt".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    #[tokio::test]
    async fn user_prompt_hook_blocks_empty_prompt() {
        let hook = RejectIfEmpty;
        match hook.on_user_prompt("", "sess-1").await {
            HookOutcome::Block { reason } => assert_eq!(reason, "empty prompt"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
        let outcome = hook.on_user_prompt("hello", "sess-1").await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R2: StopHook returning Block when final_text is empty.
    struct BlockIfEmptyStop;

    #[async_trait]
    impl StopHook for BlockIfEmptyStop {
        async fn on_stop(&self, final_text: &str) -> HookOutcome {
            if final_text.is_empty() {
                HookOutcome::Block {
                    reason: "still work to do".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    #[tokio::test]
    async fn stop_hook_blocks_when_final_text_empty() {
        let hook = BlockIfEmptyStop;
        match hook.on_stop("").await {
            HookOutcome::Block { reason } => assert_eq!(reason, "still work to do"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
        let outcome = hook.on_stop("done").await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R3: SubagentHook — start is observational, stop can block.
    struct TrackedSubagent {
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
        block_on_stop: bool,
    }

    #[async_trait]
    impl SubagentHook for TrackedSubagent {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {
            self.start_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            if self.block_on_stop {
                HookOutcome::Block {
                    reason: "not done".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    #[tokio::test]
    async fn subagent_hook_start_observes_stop_blocks() {
        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let hook = TrackedSubagent {
            start_count: Arc::clone(&start_count),
            stop_count: Arc::clone(&stop_count),
            block_on_stop: true,
        };

        hook.on_subagent_start("child-1", "researcher").await;
        let outcome = hook.on_subagent_stop("child-1", "researcher").await;

        assert_eq!(start_count.load(Ordering::SeqCst), 1);
        assert_eq!(stop_count.load(Ordering::SeqCst), 1);
        match outcome {
            HookOutcome::Block { reason } => assert_eq!(reason, "not done"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
    }

    // R4: SessionLifecycleHook — both methods observational.
    struct CountingLifecycle {
        start_count: Arc<AtomicUsize>,
        end_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SessionLifecycleHook for CountingLifecycle {
        async fn on_session_start(&self, _session_id: &str) {
            self.start_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn on_session_end(&self, _session_id: &str) {
            self.end_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn session_lifecycle_hook_fires_both_methods() {
        let start_count = Arc::new(AtomicUsize::new(0));
        let end_count = Arc::new(AtomicUsize::new(0));
        let hook = CountingLifecycle {
            start_count: Arc::clone(&start_count),
            end_count: Arc::clone(&end_count),
        };
        hook.on_session_start("sess-1").await;
        hook.on_session_end("sess-1").await;
        assert_eq!(start_count.load(Ordering::SeqCst), 1);
        assert_eq!(end_count.load(Ordering::SeqCst), 1);
    }

    // R5: CompactionHook — block when event_count below threshold.
    struct BlockSmallCompactions;

    #[async_trait]
    impl CompactionHook for BlockSmallCompactions {
        async fn before_compaction(&self, event_count: usize) -> HookOutcome {
            if event_count < 10 {
                HookOutcome::Block {
                    reason: "too few events to compact".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    #[tokio::test]
    async fn compaction_hook_blocks_below_threshold() {
        let hook = BlockSmallCompactions;
        match hook.before_compaction(3).await {
            HookOutcome::Block { reason } => assert_eq!(reason, "too few events to compact"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
        let outcome = hook.before_compaction(50).await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R6: PostToolFailureHook — counter increments on error output.
    struct CountingToolFailure {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PostToolFailureHook for CountingToolFailure {
        async fn after_tool_failure(
            &self,
            _envelope: &ToolEnvelope,
            _output: &ToolOutput,
            _ctx: &ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn post_tool_failure_hook_fires_on_error_output() {
        let counter = Arc::new(AtomicUsize::new(0));
        let hook = CountingToolFailure {
            counter: Arc::clone(&counter),
        };
        let envelope = make_envelope("bash");
        let output = ToolOutput {
            content: serde_json::json!({ "error": "boom" }),
            is_error: true,
            duration: Duration::ZERO,
        };
        hook.after_tool_failure(&envelope, &output, &ToolContext::empty())
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // R7: HookRegistry tracks all 11 categories independently and dispatches
    // each new variant through the correct per-category vector.
    struct NoopUserPrompt;
    #[async_trait]
    impl UserPromptHook for NoopUserPrompt {
        async fn on_user_prompt(&self, _prompt: &str, _session_id: &str) -> HookOutcome {
            HookOutcome::Proceed
        }
    }

    struct NoopStop;
    #[async_trait]
    impl StopHook for NoopStop {
        async fn on_stop(&self, _final_text: &str) -> HookOutcome {
            HookOutcome::Proceed
        }
    }

    struct NoopSubagent;
    #[async_trait]
    impl SubagentHook for NoopSubagent {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            HookOutcome::Proceed
        }
    }

    struct NoopLifecycle;
    #[async_trait]
    impl SessionLifecycleHook for NoopLifecycle {
        async fn on_session_start(&self, _session_id: &str) {}
        async fn on_session_end(&self, _session_id: &str) {}
    }

    struct NoopCompaction;
    #[async_trait]
    impl CompactionHook for NoopCompaction {
        async fn before_compaction(&self, _event_count: usize) -> HookOutcome {
            HookOutcome::Proceed
        }
    }

    struct NoopFailure;
    #[async_trait]
    impl PostToolFailureHook for NoopFailure {
        async fn after_tool_failure(
            &self,
            _envelope: &ToolEnvelope,
            _output: &ToolOutput,
            _ctx: &ToolContext,
        ) {
        }
    }

    #[tokio::test]
    async fn registry_tracks_and_dispatches_new_categories() {
        let mut reg = HookRegistry::new();
        reg.register(Hook::UserPrompt(Box::new(NoopUserPrompt)));
        reg.register(Hook::Stop(Box::new(NoopStop)));
        reg.register(Hook::Subagent(Box::new(NoopSubagent)));
        reg.register(Hook::SessionLifecycle(Box::new(NoopLifecycle)));
        reg.register(Hook::Compaction(Box::new(NoopCompaction)));
        reg.register(Hook::PostToolFailure(Box::new(NoopFailure)));

        assert_eq!(reg.user_prompt_len(), 1);
        assert_eq!(reg.stop_len(), 1);
        assert_eq!(reg.subagent_len(), 1);
        assert_eq!(reg.session_lifecycle_len(), 1);
        assert_eq!(reg.compaction_len(), 1);
        assert_eq!(reg.post_tool_failure_len(), 1);

        // pre-existing categories untouched
        assert_eq!(reg.pre_tool_len(), 0);
        assert_eq!(reg.post_tool_len(), 0);
        assert_eq!(reg.pre_llm_len(), 0);
        assert_eq!(reg.post_llm_len(), 0);
        assert_eq!(reg.session_event_len(), 0);

        // observational dispatchers run without panicking.
        reg.run_subagent_start("child-1", "researcher").await;
        reg.run_session_start("sess-1").await;
        reg.run_session_end("sess-1").await;
        let envelope = make_envelope("bash");
        let output = ToolOutput {
            content: serde_json::json!({ "error": "boom" }),
            is_error: true,
            duration: Duration::ZERO,
        };
        reg.run_post_tool_failure(&envelope, &output, &ToolContext::empty())
            .await;

        // blocking dispatchers return Proceed when no hook blocks.
        let outcome = reg.run_user_prompt("hello", "sess-1").await;
        assert!(matches!(outcome, HookOutcome::Proceed));
        let outcome = reg.run_stop("done").await;
        assert!(matches!(outcome, HookOutcome::Proceed));
        let outcome = reg.run_subagent_stop("child-1", "researcher").await;
        assert!(matches!(outcome, HookOutcome::Proceed));
        let outcome = reg.run_pre_compaction(42).await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // Blocking dispatchers must short-circuit on the first Block.
    struct AlwaysBlockUserPrompt;
    #[async_trait]
    impl UserPromptHook for AlwaysBlockUserPrompt {
        async fn on_user_prompt(&self, _prompt: &str, _session_id: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "denied".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn run_user_prompt_first_block_wins() {
        let mut reg = HookRegistry::new();
        reg.register(Hook::UserPrompt(Box::new(AlwaysBlockUserPrompt)));
        reg.register(Hook::UserPrompt(Box::new(NoopUserPrompt)));
        match reg.run_user_prompt("hi", "sess-1").await {
            HookOutcome::Block { reason } => assert_eq!(reason, "denied"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
    }
}
