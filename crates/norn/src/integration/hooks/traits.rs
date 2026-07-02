//! Lifecycle hooks for pre/post tool, pre/post LLM, and session events.
//!
//! Five hook traits cover the boundaries where external code might want to
//! observe, log, or intercept Norn's behaviour:
//!
//! - [`PreToolHook`] / [`PostToolHook`] — wraps tool execution.
//! - [`PreLlmHook`] / [`PostLlmHook`] — wraps each provider call.
//! - [`SessionEventHook`] — fires for every session event append.
//!
//! Pre-hooks return a [`HookOutcome`] that lets them veto the call. The
//! first `Block` short-circuits the remaining pre-hooks. Post and event
//! hooks are observation only — they cannot influence the result.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use super::new_traits::{
    CompactionHook, PostToolFailureHook, SessionLifecycleHook, StopHook, SubagentHook,
    UserPromptHook,
};
use crate::provider::events::StopReason;
use crate::provider::request::ProviderRequest;
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

/// Outcome of a pre-hook invocation.
#[derive(Clone, Debug)]
pub enum HookOutcome {
    /// Allow the underlying operation to proceed.
    Proceed,
    /// Veto the underlying operation with a reason.
    Block {
        /// Reason returned to the caller.
        reason: String,
    },
    /// Allow the underlying operation to proceed with replacement input.
    ///
    /// Returned only from [`PreToolHook`]. The dispatcher replaces the
    /// envelope's `model_args` with `updated_input` and forwards the
    /// modified arguments to the tool executor.
    Modify {
        /// Replacement tool arguments serialised as a JSON value.
        updated_input: serde_json::Value,
    },
}

/// Summary of a completed LLM call, passed to [`PostLlmHook`].
#[derive(Clone, Debug, Default)]
pub struct LlmCallSummary {
    /// How the provider finished the response.
    pub stop_reason: Option<StopReason>,
    /// Token usage reported by the provider.
    pub usage: Usage,
    /// Number of streaming events the provider emitted.
    pub event_count: u64,
    /// First error message encountered during the stream, if any.
    pub error: Option<String>,
}

/// Fires immediately before a tool executes. Can veto via [`HookOutcome::Block`].
#[async_trait]
pub trait PreToolHook: Send + Sync {
    /// Invoked with the envelope the runtime is about to dispatch.
    async fn before_tool(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> HookOutcome;
}

/// Fires immediately after a tool finishes executing.
#[async_trait]
pub trait PostToolHook: Send + Sync {
    /// Invoked once the tool has produced an output.
    async fn after_tool(&self, envelope: &ToolEnvelope, output: &ToolOutput, ctx: &ToolContext);
}

/// Fires immediately before a provider call. Can veto via [`HookOutcome::Block`].
#[async_trait]
pub trait PreLlmHook: Send + Sync {
    /// Invoked with the request the runtime is about to send.
    async fn before_llm(&self, request: &ProviderRequest) -> HookOutcome;
}

/// Fires after a provider call completes.
#[async_trait]
pub trait PostLlmHook: Send + Sync {
    /// Invoked with a summary of the completed call.
    async fn after_llm(&self, summary: &LlmCallSummary);
}

/// Fires whenever a session event is appended.
#[async_trait]
pub trait SessionEventHook: Send + Sync {
    /// Invoked with the event being recorded.
    async fn on_event(&self, event: &SessionEvent);
}

/// Closed enum of registrable hook types.
///
/// Public callers construct a `Hook::*` variant and hand it to
/// [`HookRegistry::register`]. This keeps the registry API tidy while still
/// letting downstream code implement the trait shape they want.
///
/// Extended by NH-002 with variants for `UserPrompt`, `Stop`, `Subagent`,
/// `SessionLifecycle`, `Compaction`, and `PostToolFailure` hooks.
pub enum Hook {
    /// A pre-tool hook.
    PreTool(Box<dyn PreToolHook>),
    /// A post-tool hook.
    PostTool(Box<dyn PostToolHook>),
    /// A pre-LLM hook.
    PreLlm(Box<dyn PreLlmHook>),
    /// A post-LLM hook.
    PostLlm(Box<dyn PostLlmHook>),
    /// A session event hook.
    SessionEvent(Box<dyn SessionEventHook>),
    /// A user-prompt hook.
    UserPrompt(Box<dyn UserPromptHook>),
    /// A stop hook.
    Stop(Box<dyn StopHook>),
    /// A sub-agent lifecycle hook (start and stop).
    Subagent(Box<dyn SubagentHook>),
    /// A session lifecycle hook (session start and end).
    SessionLifecycle(Box<dyn SessionLifecycleHook>),
    /// A pre-compaction hook.
    Compaction(Box<dyn CompactionHook>),
    /// A post-tool failure hook.
    PostToolFailure(Box<dyn PostToolFailureHook>),
}

/// Registry of lifecycle hooks.
///
/// Hooks are stored in registration order. Pre-hooks short-circuit on the
/// first [`HookOutcome::Block`] — earlier registrations win.
///
/// `dispatching_session_events` is a non-reentrant guard for
/// [`Self::run_on_event`]. A `SessionEventHook` implementation that appends
/// to the event store (which calls `append_and_notify`, which fires hooks)
/// would otherwise recurse indefinitely. The flag is flipped on entry to
/// `run_on_event` and cleared via an RAII guard so a panicking hook still
/// resets state.
#[derive(Default)]
pub struct HookRegistry {
    pre_tool: Vec<Box<dyn PreToolHook>>,
    post_tool: Vec<Box<dyn PostToolHook>>,
    pre_llm: Vec<Box<dyn PreLlmHook>>,
    post_llm: Vec<Box<dyn PostLlmHook>>,
    session_event: Vec<Box<dyn SessionEventHook>>,
    user_prompt: Vec<Box<dyn UserPromptHook>>,
    stop: Vec<Box<dyn StopHook>>,
    subagent: Vec<Box<dyn SubagentHook>>,
    session_lifecycle: Vec<Box<dyn SessionLifecycleHook>>,
    compaction: Vec<Box<dyn CompactionHook>>,
    post_tool_failure: Vec<Box<dyn PostToolFailureHook>>,
    /// Non-reentrant guard. `true` while a [`Self::run_on_event`] call is in
    /// flight; nested calls observe `true` and skip dispatch.
    dispatching_session_events: AtomicBool,
}

/// RAII guard that flips the session-event dispatch flag back to `false`
/// when dropped — including on panic — so a misbehaving hook cannot wedge
/// the registry into a permanently re-entrant state.
struct SessionEventDispatchGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for SessionEventDispatchGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl HookRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook. Variants are dispatched into per-category vectors so
    /// the registry can iterate them in registration order at call time.
    pub fn register(&mut self, hook: Hook) {
        match hook {
            Hook::PreTool(h) => self.pre_tool.push(h),
            Hook::PostTool(h) => self.post_tool.push(h),
            Hook::PreLlm(h) => self.pre_llm.push(h),
            Hook::PostLlm(h) => self.post_llm.push(h),
            Hook::SessionEvent(h) => self.session_event.push(h),
            Hook::UserPrompt(h) => self.user_prompt.push(h),
            Hook::Stop(h) => self.stop.push(h),
            Hook::Subagent(h) => self.subagent.push(h),
            Hook::SessionLifecycle(h) => self.session_lifecycle.push(h),
            Hook::Compaction(h) => self.compaction.push(h),
            Hook::PostToolFailure(h) => self.post_tool_failure.push(h),
        }
    }

    /// Append every hook from `other` onto `self`, preserving `other`'s
    /// internal registration order.
    ///
    /// Hooks already registered on `self` keep their earlier positions, so
    /// on conflicting outcomes (first-`Block`-wins) the hooks already
    /// present take precedence over the merged-in ones. See
    /// [`Self::merge_shared`] (in [`super::merge`]) for folding in a shared
    /// `Arc<HookRegistry>` without consuming it.
    pub fn merge(&mut self, other: Self) {
        let Self {
            pre_tool,
            post_tool,
            pre_llm,
            post_llm,
            session_event,
            user_prompt,
            stop,
            subagent,
            session_lifecycle,
            compaction,
            post_tool_failure,
            dispatching_session_events: _,
        } = other;
        self.pre_tool.extend(pre_tool);
        self.post_tool.extend(post_tool);
        self.pre_llm.extend(pre_llm);
        self.post_llm.extend(post_llm);
        self.session_event.extend(session_event);
        self.user_prompt.extend(user_prompt);
        self.stop.extend(stop);
        self.subagent.extend(subagent);
        self.session_lifecycle.extend(session_lifecycle);
        self.compaction.extend(compaction);
        self.post_tool_failure.extend(post_tool_failure);
    }

    /// Number of pre-tool hooks registered.
    #[must_use]
    pub fn pre_tool_len(&self) -> usize {
        self.pre_tool.len()
    }

    /// Number of post-tool hooks registered.
    #[must_use]
    pub fn post_tool_len(&self) -> usize {
        self.post_tool.len()
    }

    /// Number of pre-LLM hooks registered.
    #[must_use]
    pub fn pre_llm_len(&self) -> usize {
        self.pre_llm.len()
    }

    /// Number of post-LLM hooks registered.
    #[must_use]
    pub fn post_llm_len(&self) -> usize {
        self.post_llm.len()
    }

    /// Number of session-event hooks registered.
    #[must_use]
    pub fn session_event_len(&self) -> usize {
        self.session_event.len()
    }

    /// Number of user-prompt hooks registered.
    #[must_use]
    pub fn user_prompt_len(&self) -> usize {
        self.user_prompt.len()
    }

    /// Number of stop hooks registered.
    #[must_use]
    pub fn stop_len(&self) -> usize {
        self.stop.len()
    }

    /// Number of sub-agent lifecycle hooks registered.
    #[must_use]
    pub fn subagent_len(&self) -> usize {
        self.subagent.len()
    }

    /// Number of session lifecycle hooks registered.
    #[must_use]
    pub fn session_lifecycle_len(&self) -> usize {
        self.session_lifecycle.len()
    }

    /// Number of pre-compaction hooks registered.
    #[must_use]
    pub fn compaction_len(&self) -> usize {
        self.compaction.len()
    }

    /// Number of post-tool failure hooks registered.
    #[must_use]
    pub fn post_tool_failure_len(&self) -> usize {
        self.post_tool_failure.len()
    }

    /// Invoke all pre-tool hooks in registration order. The first
    /// non-[`HookOutcome::Proceed`] outcome short-circuits the remaining
    /// hooks and is returned to the caller — either a [`HookOutcome::Block`]
    /// veto or a [`HookOutcome::Modify`] that the dispatcher must apply
    /// before invoking the tool.
    pub async fn run_pre_tool(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> HookOutcome {
        for hook in &self.pre_tool {
            match hook.before_tool(envelope, ctx).await {
                HookOutcome::Proceed => {}
                other => return other,
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all post-tool hooks in registration order.
    pub async fn run_post_tool(
        &self,
        envelope: &ToolEnvelope,
        output: &ToolOutput,
        ctx: &ToolContext,
    ) {
        for hook in &self.post_tool {
            hook.after_tool(envelope, output, ctx).await;
        }
    }

    /// Invoke all pre-LLM hooks in registration order.
    pub async fn run_pre_llm(&self, request: &ProviderRequest) -> HookOutcome {
        for hook in &self.pre_llm {
            if let HookOutcome::Block { reason } = hook.before_llm(request).await {
                return HookOutcome::Block { reason };
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all post-LLM hooks in registration order.
    pub async fn run_post_llm(&self, summary: &LlmCallSummary) {
        for hook in &self.post_llm {
            hook.after_llm(summary).await;
        }
    }

    /// Invoke all session-event hooks in registration order.
    ///
    /// Non-reentrant: if a hook implementation triggers another
    /// `run_on_event` invocation on the same registry (for example by
    /// appending to the event store and going back through
    /// `append_and_notify`), the inner call observes the guard, skips
    /// dispatch silently, and returns immediately. The guard is cleared via
    /// an RAII wrapper so a panicking hook does not leave the registry in a
    /// permanently locked state.
    pub async fn run_on_event(&self, event: &SessionEvent) {
        if self.dispatching_session_events.swap(true, Ordering::AcqRel) {
            return;
        }
        let _guard = SessionEventDispatchGuard {
            flag: &self.dispatching_session_events,
        };
        for hook in &self.session_event {
            hook.on_event(event).await;
        }
    }

    /// Invoke all user-prompt hooks in registration order. Returns the first
    /// [`HookOutcome::Block`] encountered, or [`HookOutcome::Proceed`] if none
    /// block. [`HookOutcome::Modify`] is not produced by user-prompt hooks
    /// (CO10).
    pub async fn run_user_prompt(&self, prompt: &str, session_id: &str) -> HookOutcome {
        for hook in &self.user_prompt {
            if let HookOutcome::Block { reason } = hook.on_user_prompt(prompt, session_id).await {
                return HookOutcome::Block { reason };
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all stop hooks in registration order. Returns the first
    /// [`HookOutcome::Block`] encountered, or [`HookOutcome::Proceed`] if none
    /// block. A Block forces the agent loop to continue.
    pub async fn run_stop(&self, final_text: &str) -> HookOutcome {
        for hook in &self.stop {
            if let HookOutcome::Block { reason } = hook.on_stop(final_text).await {
                return HookOutcome::Block { reason };
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all sub-agent start hooks in registration order. Observational —
    /// every registered hook runs.
    pub async fn run_subagent_start(&self, agent_id: &str, agent_type: &str) {
        for hook in &self.subagent {
            hook.on_subagent_start(agent_id, agent_type).await;
        }
    }

    /// Invoke all sub-agent stop hooks in registration order. Returns the
    /// first [`HookOutcome::Block`] encountered, or [`HookOutcome::Proceed`]
    /// if none block.
    pub async fn run_subagent_stop(&self, agent_id: &str, agent_type: &str) -> HookOutcome {
        for hook in &self.subagent {
            if let HookOutcome::Block { reason } = hook.on_subagent_stop(agent_id, agent_type).await
            {
                return HookOutcome::Block { reason };
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all session-start lifecycle hooks in registration order.
    pub async fn run_session_start(&self, session_id: &str) {
        for hook in &self.session_lifecycle {
            hook.on_session_start(session_id).await;
        }
    }

    /// Invoke all session-end lifecycle hooks in registration order.
    pub async fn run_session_end(&self, session_id: &str) {
        for hook in &self.session_lifecycle {
            hook.on_session_end(session_id).await;
        }
    }

    /// Invoke all pre-compaction hooks in registration order. Returns the
    /// first [`HookOutcome::Block`] encountered, or [`HookOutcome::Proceed`]
    /// if none block. A Block prevents compaction.
    pub async fn run_pre_compaction(&self, event_count: usize) -> HookOutcome {
        for hook in &self.compaction {
            if let HookOutcome::Block { reason } = hook.before_compaction(event_count).await {
                return HookOutcome::Block { reason };
            }
        }
        HookOutcome::Proceed
    }

    /// Invoke all post-tool failure hooks in registration order.
    pub async fn run_post_tool_failure(
        &self,
        envelope: &ToolEnvelope,
        output: &ToolOutput,
        ctx: &ToolContext,
    ) {
        for hook in &self.post_tool_failure {
            hook.after_tool_failure(envelope, output, ctx).await;
        }
    }
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

    use super::*;
    use crate::provider::request::{Message, MessageRole, ProviderRequest};
    use crate::session::events::{EventBase, SessionEvent};
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

    fn make_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("hi".to_owned()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "test".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    struct BlockToolByName {
        name: String,
        reason: String,
    }

    #[async_trait]
    impl PreToolHook for BlockToolByName {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == self.name {
                HookOutcome::Block {
                    reason: self.reason.clone(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    struct AlwaysProceed {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PreToolHook for AlwaysProceed {
        async fn before_tool(&self, _envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            self.counter.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Proceed
        }
    }

    struct CountingPostTool {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PostToolHook for CountingPostTool {
        async fn after_tool(
            &self,
            _envelope: &ToolEnvelope,
            _output: &ToolOutput,
            _ctx: &ToolContext,
        ) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct BlockLlm;

    #[async_trait]
    impl PreLlmHook for BlockLlm {
        async fn before_llm(&self, _req: &ProviderRequest) -> HookOutcome {
            HookOutcome::Block {
                reason: "no llm calls in tests".to_owned(),
            }
        }
    }

    struct CountingPostLlm {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PostLlmHook for CountingPostLlm {
        async fn after_llm(&self, _summary: &LlmCallSummary) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct CountingEvent {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SessionEventHook for CountingEvent {
        async fn on_event(&self, _event: &SessionEvent) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    // R6 acceptance: registering a PreToolHook that blocks 'bash' causes
    // run_pre_tool to return Block with the expected reason.
    #[tokio::test]
    async fn pre_tool_block_short_circuits_bash() {
        let mut reg = HookRegistry::new();
        reg.register(Hook::PreTool(Box::new(BlockToolByName {
            name: "bash".to_owned(),
            reason: "policy".to_owned(),
        })));

        let ctx = ToolContext::empty();
        let outcome = reg.run_pre_tool(&make_envelope("bash"), &ctx).await;
        match outcome {
            HookOutcome::Block { reason } => assert_eq!(reason, "policy"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }

        let outcome = reg.run_pre_tool(&make_envelope("read"), &ctx).await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R6 acceptance: registration order is preserved — first Block wins.
    #[tokio::test]
    async fn pre_tool_first_block_wins() {
        let mut reg = HookRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register(Hook::PreTool(Box::new(BlockToolByName {
            name: "bash".to_owned(),
            reason: "first".to_owned(),
        })));
        reg.register(Hook::PreTool(Box::new(AlwaysProceed {
            counter: Arc::clone(&counter),
        })));
        reg.register(Hook::PreTool(Box::new(BlockToolByName {
            name: "bash".to_owned(),
            reason: "second".to_owned(),
        })));

        let outcome = reg
            .run_pre_tool(&make_envelope("bash"), &ToolContext::empty())
            .await;
        match outcome {
            HookOutcome::Block { reason } => assert_eq!(reason, "first"),
            HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("expected Block"),
        }
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "later hooks must not run after a Block"
        );
    }

    #[tokio::test]
    async fn pre_tool_no_hooks_proceeds() {
        let reg = HookRegistry::new();
        let outcome = reg
            .run_pre_tool(&make_envelope("anything"), &ToolContext::empty())
            .await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    #[tokio::test]
    async fn post_tool_fires_in_order() {
        let mut reg = HookRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register(Hook::PostTool(Box::new(CountingPostTool {
            counter: Arc::clone(&counter),
        })));
        reg.register(Hook::PostTool(Box::new(CountingPostTool {
            counter: Arc::clone(&counter),
        })));

        let output = ToolOutput::success(serde_json::json!({}));
        reg.run_post_tool(&make_envelope("x"), &output, &ToolContext::empty())
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pre_llm_block_returns_block() {
        let mut reg = HookRegistry::new();
        reg.register(Hook::PreLlm(Box::new(BlockLlm)));
        let outcome = reg.run_pre_llm(&make_request()).await;
        assert!(matches!(outcome, HookOutcome::Block { .. }));
    }

    #[tokio::test]
    async fn post_llm_counter_increments() {
        let mut reg = HookRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register(Hook::PostLlm(Box::new(CountingPostLlm {
            counter: Arc::clone(&counter),
        })));
        reg.run_post_llm(&LlmCallSummary::default()).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn session_event_counter_increments() {
        let mut reg = HookRegistry::new();
        let counter = Arc::new(AtomicUsize::new(0));
        reg.register(Hook::SessionEvent(Box::new(CountingEvent {
            counter: Arc::clone(&counter),
        })));
        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hi".to_owned(),
        };
        reg.run_on_event(&event).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registry_tracks_per_category_counts() {
        let mut reg = HookRegistry::new();
        reg.register(Hook::PreTool(Box::new(BlockToolByName {
            name: "x".to_owned(),
            reason: "r".to_owned(),
        })));
        reg.register(Hook::PostTool(Box::new(CountingPostTool {
            counter: Arc::new(AtomicUsize::new(0)),
        })));
        reg.register(Hook::SessionEvent(Box::new(CountingEvent {
            counter: Arc::new(AtomicUsize::new(0)),
        })));
        assert_eq!(reg.pre_tool_len(), 1);
        assert_eq!(reg.post_tool_len(), 1);
        assert_eq!(reg.pre_llm_len(), 0);
        assert_eq!(reg.post_llm_len(), 0);
        assert_eq!(reg.session_event_len(), 1);
    }

    // R2 acceptance: construct Modify with a JSON object, pattern match
    // extracts updated_input.
    #[test]
    fn hook_outcome_modify_holds_updated_input() {
        let outcome = HookOutcome::Modify {
            updated_input: serde_json::json!({ "command": "echo hello" }),
        };
        match outcome {
            HookOutcome::Modify { updated_input } => {
                assert_eq!(updated_input["command"], "echo hello");
            }
            HookOutcome::Proceed | HookOutcome::Block { .. } => panic!("expected Modify"),
        }
    }

    // R8 / C45: a SessionEventHook that calls run_on_event back on the same
    // registry must observe the non-reentrant guard. The hook fires exactly
    // once for the outer dispatch; the inner recursive call is a silent
    // no-op (the counter would be 2 otherwise).
    #[tokio::test]
    async fn session_event_guard_skips_recursive_dispatch() {
        struct CountAndRecurse {
            calls: Arc<AtomicUsize>,
            registry: std::sync::OnceLock<Arc<HookRegistry>>,
        }

        #[async_trait]
        impl SessionEventHook for CountAndRecurse {
            async fn on_event(&self, _event: &SessionEvent) {
                let prior = self.calls.fetch_add(1, Ordering::SeqCst);
                if prior == 0
                    && let Some(registry) = self.registry.get()
                {
                    let nested = SessionEvent::UserMessage {
                        base: EventBase::new(None),
                        content: "nested".to_owned(),
                    };
                    registry.run_on_event(&nested).await;
                }
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let hook = Arc::new(CountAndRecurse {
            calls: Arc::clone(&calls),
            registry: std::sync::OnceLock::new(),
        });

        // Wrap the hook in a thin adapter so the registry stores Box<dyn …>
        // while the test retains shared access to the underlying counter.
        struct AdapterHook(Arc<CountAndRecurse>);

        #[async_trait]
        impl SessionEventHook for AdapterHook {
            async fn on_event(&self, event: &SessionEvent) {
                self.0.on_event(event).await;
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Hook::SessionEvent(Box::new(AdapterHook(Arc::clone(&hook)))));
        let reg = Arc::new(reg);
        let _ = hook.registry.set(Arc::clone(&reg));

        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "outer".to_owned(),
        };
        reg.run_on_event(&event).await;

        // The hook ran exactly once. The recursive call inside the hook hit
        // the guard and skipped dispatch — the counter would be 2 otherwise.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // R2 acceptance: a PreToolHook returning Modify surfaces through the
    // registry's pre-tool dispatch.
    #[tokio::test]
    async fn pre_tool_modify_surfaces_from_registry() {
        struct RewriteArgs;
        #[async_trait]
        impl PreToolHook for RewriteArgs {
            async fn before_tool(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> HookOutcome {
                HookOutcome::Modify {
                    updated_input: serde_json::json!({ "rewritten": true }),
                }
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Hook::PreTool(Box::new(RewriteArgs)));
        let outcome = reg
            .run_pre_tool(&make_envelope("anything"), &ToolContext::empty())
            .await;
        match outcome {
            HookOutcome::Modify { updated_input } => {
                assert_eq!(updated_input["rewritten"], true);
            }
            HookOutcome::Proceed | HookOutcome::Block { .. } => panic!("expected Modify"),
        }
    }
}
