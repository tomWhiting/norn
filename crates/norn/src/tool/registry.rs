//! Tool registry for storing and looking up tools by name.
//!
//! [`ToolRegistry`] stores tool implementations behind trait objects and
//! supports four operations beyond plain storage:
//!
//! 1. **Lookup gating** via [`ToolRegistry::set_available`] (allow-list)
//!    and [`ToolRegistry::set_disallowed`] (deny-list; wins over the
//!    allow-list) — restricts which registered tools may be retrieved
//!    without removing them.
//! 2. **Runtime mutation** via [`ToolRegistry::register`] and
//!    [`ToolRegistry::remove`] — tools can be added or removed while the
//!    registry is in use.
//! 3. **Per-registry [`ToolContext`]** — every dispatched call receives the
//!    same orchestrator-supplied context (flags, runtime args, runtime
//!    pre/post checks, and on-success actions).
//! 4. **Full lifecycle dispatch** via the [`ToolExecutor`] impl — pre-validate
//!    (compile-time + runtime), execute, post-validate (compile-time +
//!    runtime), on-success (compile-time + runtime). Gate-mode post-validate
//!    failures surface as [`ToolError::PostValidationFailed`]; report-mode
//!    failures return the tool's own output (which carries the errors in its
//!    payload) so the model can react.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::context::{ToolContext, ToolFlag};
use super::envelope::ToolEnvelope;
use super::lifecycle::{
    Advisory, CheckOverride, PostCheckResult, PostValidateMode, PostValidateOutcome,
    PreValidateOutcome,
};
use super::post_validation_feedback::{append_advisories, append_post_validation_errors};
use super::scheduling::ToolEffectIndex;
use super::traits::{Tool, ToolOutput};
use crate::error::ToolError;
use crate::r#loop::config::{DispatchOutcome, ToolExecutor};

/// Stores tools as trait objects and dispatches calls through the full
/// pre-validate / execute / post-validate / on-success lifecycle.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool + Send + Sync>>,
    /// When `Some`, restricts [`Self::get`] (and therefore dispatch) to the
    /// named tools. `None` means every registered tool is available.
    available: Option<HashSet<String>>,
    /// Names gated out unconditionally (e.g. `--disallowed-tools`). A
    /// disallowed name is unavailable even when it appears in
    /// [`Self::set_available`]'s allow-list — deny wins.
    disallowed: HashSet<String>,
    /// Shared orchestrator-supplied context passed to every dispatched tool.
    context: Arc<ToolContext>,
    /// Name → implementation index published on [`Self::shared_context`]
    /// so the agent loop's dispatch layer can resolve per-call
    /// [`ToolEffect`](super::scheduling::ToolEffect)s for scheduling.
    /// Kept in sync by [`Self::register`] / [`Self::remove`].
    effects: Arc<ToolEffectIndex>,
}

impl ToolRegistry {
    /// Creates an empty registry with an empty [`ToolContext`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_context(Arc::new(ToolContext::empty()))
    }

    /// Creates an empty registry that will share `context` with every
    /// dispatched tool call.
    #[must_use]
    pub fn with_context(context: Arc<ToolContext>) -> Self {
        let effects = Arc::new(ToolEffectIndex::new());
        context.insert_extension(Arc::clone(&effects));
        Self {
            tools: HashMap::new(),
            available: None,
            disallowed: HashSet::new(),
            context,
            effects,
        }
    }

    /// Replaces the orchestrator-supplied [`ToolContext`] shared with every
    /// subsequent dispatched tool call. The registry's
    /// [`ToolEffectIndex`] extension is re-published on the new context so
    /// dispatch keeps resolving effects after the swap.
    pub fn set_context(&mut self, context: Arc<ToolContext>) {
        context.insert_extension(Arc::clone(&self.effects));
        self.context = context;
    }

    /// Registers a tool. Uses the tool's `name()` as the key. When an
    /// availability set is active, the new tool is gated by it — call
    /// [`Self::set_available`] again to include it.
    pub fn register(&mut self, tool: Box<dyn Tool + Send + Sync>) {
        let tool: Arc<dyn Tool + Send + Sync> = Arc::from(tool);
        let name = tool.name().to_string();
        self.effects.insert(name.clone(), Arc::clone(&tool));
        self.tools.insert(name, tool);
    }

    /// Removes a tool from the registry and from the availability set (if
    /// any), returning the removed implementation when present.
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool + Send + Sync>> {
        let removed = self.tools.remove(name);
        self.effects.remove(name);
        if let Some(available) = self.available.as_mut() {
            available.remove(name);
        }
        removed
    }

    /// Restricts subsequent [`Self::get`] / dispatch results to the named
    /// tools. Names that are not currently registered are still accepted —
    /// they become available when [`Self::register`] is called for them.
    pub fn set_available(&mut self, names: Vec<String>) {
        self.available = Some(names.into_iter().collect());
    }

    /// Restores access to every registered tool. Equivalent to clearing the
    /// availability set installed by [`Self::set_available`].
    pub fn reset_available(&mut self) {
        self.available = None;
    }

    /// Unconditionally gates out the named tools (the `--disallowed-tools`
    /// surface). Names are exact matches, consistent with
    /// [`Self::set_available`]. A disallowed name stays unavailable even
    /// when listed in the availability allow-list — deny wins — and the
    /// gate also applies to tools registered after this call.
    pub fn set_disallowed(&mut self, names: Vec<String>) {
        self.disallowed = names.into_iter().collect();
    }

    /// Looks up a tool by name. Returns `None` for unregistered names and
    /// for names that have been gated out by [`Self::set_available`] or
    /// [`Self::set_disallowed`].
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&(dyn Tool + Send + Sync)> {
        if !self.is_name_available(name) {
            return None;
        }
        self.tools.get(name).map(AsRef::as_ref)
    }

    /// Look up a physically registered tool while bypassing only the current
    /// availability view. Explicit deny rules remain authoritative.
    pub(crate) fn get_registered(&self, name: &str) -> Option<&(dyn Tool + Send + Sync)> {
        if self.disallowed.contains(name) {
            return None;
        }
        self.tools.get(name).map(AsRef::as_ref)
    }

    /// Returns `true` when a tool with this exact name is physically
    /// registered, regardless of allow-list / deny-list gating.
    ///
    /// Distinct from [`Self::get`], which returns `None` for a gated name:
    /// gating hides an installed tool from dispatch but does not
    /// unregister it. This answers "is this a real tool at all?" — the CLI
    /// uses it to warn when an `--allowed-tools` / `--disallowed-tools`
    /// flag names a tool that matches nothing, without false-flagging a
    /// tool that was correctly gated out.
    #[must_use]
    pub fn is_registered(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Returns an iterator over the names of currently-available tools, in
    /// lexicographically sorted order.
    ///
    /// The order is deterministic across process runs. The backing store is
    /// a [`HashMap`], whose iteration order is randomised per instance, so
    /// yielding keys directly would vary the order between runs. Every
    /// prompt- and request-visible projection of the registry is built from
    /// this iterator — the system prompt's `# Tools` section
    /// ([`collect_tool_prompt_entries`](crate::agent::prompt_install)), the
    /// provider tool-definition array
    /// ([`collect_function_definitions`](crate::provider::surface::collect_function_definitions)),
    /// the tool catalog, and the MCP tool listing — so a stable order here
    /// keeps those byte-identical between runs and preserves provider prompt
    /// caching.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        let mut names: Vec<&str> = self
            .tools
            .keys()
            .filter(|name| self.is_name_available(name.as_str()))
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names.into_iter()
    }

    /// Returns the number of currently-available tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools
            .keys()
            .filter(|name| self.is_name_available(name.as_str()))
            .count()
    }

    fn is_name_available(&self, name: &str) -> bool {
        if self.disallowed.contains(name) {
            return false;
        }
        self.available
            .as_ref()
            .is_none_or(|allowed| allowed.contains(name))
    }

    fn prepare_dispatch(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<(&(dyn Tool + Send + Sync), ToolEnvelope, &ToolContext), ToolError> {
        let tool = self.get(name).ok_or_else(|| ToolError::ToolNotFound {
            name: name.to_string(),
        })?;

        let split = super::envelope::split_envelope_fields(arguments);
        if let Some(description) = split.description {
            // Bare-registry dispatch has no action log to attach the
            // model's stated intent to (the agent loop records it on its
            // own dispatch path); surface it in the trace stream so the
            // intent is never silently discarded.
            tracing::debug!(
                tool = name,
                call_id,
                %description,
                "tool_use_description on direct registry dispatch",
            );
        }
        let envelope = ToolEnvelope {
            tool_call_id: call_id.to_owned(),
            tool_name: name.to_string(),
            model_args: split.tool_args,
            metadata: split.metadata,
        };
        Ok((tool, envelope, self.context.as_ref()))
    }

    /// Returns `true` when no tool is currently available for dispatch.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.context))
    }

    /// Dispatch a tool call through the full lifecycle.
    ///
    /// Order: compile-time `pre_validate` → runtime `pre_checks` → `execute`
    /// → compile-time `post_validate` → runtime `post_checks` → `on_success`
    /// (compile-time, then runtime). The first phase to fail short-circuits
    /// the rest. Post-validate failures honour the tool's
    /// [`PostValidateMode`]: `Gate` surfaces a [`ToolError::PostValidationFailed`]
    /// without running on-success; `Report` returns the tool's own output
    /// with errors embedded and still runs on-success (advisory errors must
    /// not prevent bookkeeping like read-tracking).
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        self.execute_with_outcome(name, call_id, arguments)
            .await
            .map(|outcome| outcome.content)
    }

    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<DispatchOutcome, ToolError> {
        let (tool, envelope, ctx) = self.prepare_dispatch(name, call_id, arguments)?;
        dispatch_tool_with_outcome(tool, &envelope, ctx).await
    }
}

/// Dispatch a resolved tool through the shared lifecycle against `ctx`.
///
/// This is the single implementation used by root registry dispatch and
/// child-agent executors after they have resolved availability and built the
/// call envelope. Keeping it shared prevents root/spawn/fork lifecycle drift.
pub(crate) async fn dispatch_tool_with_outcome(
    tool: &(dyn Tool + Send + Sync),
    envelope: &ToolEnvelope,
    ctx: &ToolContext,
) -> Result<DispatchOutcome, ToolError> {
    if let PreValidateOutcome::Block(decision) = tool.pre_validate(envelope, ctx).await {
        return Err(decision.into());
    }
    for check in &ctx.pre_checks {
        if let PreValidateOutcome::Block(decision) = check.check(envelope, ctx).await {
            return Err(decision.into());
        }
    }

    // The registry stamps execution duration so individual tools never
    // measure themselves.
    let dispatch_started = std::time::Instant::now();
    let mut output = tool.execute(envelope, ctx).await?;
    output.duration = dispatch_started.elapsed();
    let post_validate_outcome = run_post_validation(tool, &output, ctx).await;

    let tool_default_mode = tool.post_validate_mode();
    let (resolved_mode, override_record) = resolved_post_validate_mode(tool_default_mode, ctx);
    if let Some(ref over) = override_record {
        append_check_override(&mut output.content, over);
    }
    append_advisories(&mut output.content, &post_validate_outcome.advisories);
    append_post_validation_errors(&mut output.content, &post_validate_outcome.errors);

    if !post_validate_outcome.errors.is_empty() && resolved_mode == PostValidateMode::Gate {
        // Per the `Tool::register_follow_ups` contract the hook also runs
        // after a gate-mode post-validation failure, so recovery actions
        // (undo, retry variants) travel with the committed output the
        // model sees; on-success bookkeeping still must not run.
        let follow_ups = tool.register_follow_ups(&output, ctx).await;
        append_follow_ups(&mut output.content, &follow_ups);
        return Err(ToolError::PostValidationFailed {
            reason: post_validate_outcome.errors.join("; "),
            committed_output: Some(output.content.clone()),
        });
    }

    tool.on_success(&output, ctx).await;
    for action in &ctx.on_success_actions {
        action.run(&output, ctx).await;
    }

    let follow_ups = tool.register_follow_ups(&output, ctx).await;
    append_follow_ups(&mut output.content, &follow_ups);

    Ok(DispatchOutcome {
        content: output.content,
        follow_ups,
        post_validate_outcome: post_validate_outcome.value,
    })
}

struct CapturedPostValidate {
    errors: Vec<String>,
    advisories: Vec<Advisory>,
    value: Option<Value>,
}

async fn run_post_validation(
    tool: &(dyn Tool + Send + Sync),
    output: &ToolOutput,
    ctx: &ToolContext,
) -> CapturedPostValidate {
    let mut errors = Vec::new();
    let mut advisories = Vec::new();
    let mut outcomes = Vec::new();

    let tool_outcome = tool.post_validate(output, ctx).await;
    if let PostValidateOutcome::Fail { errors: errs } = &tool_outcome {
        errors.extend(errs.clone());
    }
    outcomes.push(serde_json::json!({ "source": "tool", "outcome": tool_outcome }));

    for check in &ctx.post_checks {
        let PostCheckResult {
            outcome,
            advisories: check_advisories,
        } = check.check(output, ctx).await;
        if let PostValidateOutcome::Fail { errors: errs } = &outcome {
            errors.extend(errs.clone());
        }
        advisories.extend(check_advisories);
        outcomes.push(serde_json::json!({ "source": "runtime", "outcome": outcome }));
    }

    CapturedPostValidate {
        errors,
        advisories,
        value: Some(Value::Array(outcomes)),
    }
}

/// Apply orchestrator flag overrides to a tool's declared
/// [`PostValidateMode`].
///
/// Priority (highest → lowest): `ForceGate` → `RejectBrokenAst` →
/// `AllowBrokenAst` → tool default. `ForceGate` and `RejectBrokenAst`
/// both promote `Report` to `Gate`; `AllowBrokenAst` demotes `Gate` to
/// `Report`. When the flag combination would not actually change the
/// resolved mode (e.g. `ForceGate` on a tool whose default is already
/// `Gate`), no [`CheckOverride`] is recorded.
pub(crate) fn resolved_post_validate_mode(
    default_mode: PostValidateMode,
    ctx: &ToolContext,
) -> (PostValidateMode, Option<CheckOverride>) {
    let has_force_gate = ctx.has_flag(&ToolFlag::ForceGate);
    let has_reject = ctx.has_flag(&ToolFlag::RejectBrokenAst);
    let has_allow = ctx.has_flag(&ToolFlag::AllowBrokenAst);

    // Resolve in priority order.
    if has_force_gate {
        let mode = PostValidateMode::Gate;
        if mode == default_mode {
            return (mode, None);
        }
        return (
            mode,
            Some(CheckOverride {
                check_name: "post_validate_mode".to_string(),
                flag: ToolFlag::ForceGate,
                source: ctx
                    .flag_source(&ToolFlag::ForceGate)
                    .unwrap_or("")
                    .to_string(),
            }),
        );
    }
    if has_reject {
        let mode = PostValidateMode::Gate;
        if mode == default_mode {
            return (mode, None);
        }
        return (
            mode,
            Some(CheckOverride {
                check_name: "post_validate_mode".to_string(),
                flag: ToolFlag::RejectBrokenAst,
                source: ctx
                    .flag_source(&ToolFlag::RejectBrokenAst)
                    .unwrap_or("")
                    .to_string(),
            }),
        );
    }
    if has_allow {
        let mode = PostValidateMode::Report;
        if mode == default_mode {
            return (mode, None);
        }
        return (
            mode,
            Some(CheckOverride {
                check_name: "post_validate_mode".to_string(),
                flag: ToolFlag::AllowBrokenAst,
                source: ctx
                    .flag_source(&ToolFlag::AllowBrokenAst)
                    .unwrap_or("")
                    .to_string(),
            }),
        );
    }
    (default_mode, None)
}

/// Append a [`CheckOverride`] record to the tool's output payload under
/// the `check_overrides` key. Silently no-ops if the output is not a
/// JSON object (the convention used by file-modification tools).
pub(crate) fn append_check_override(content: &mut serde_json::Value, over: &CheckOverride) {
    let Some(map) = content.as_object_mut() else {
        return;
    };
    let entry = serde_json::to_value(over).unwrap_or(serde_json::Value::Null);
    match map.entry("check_overrides".to_string()) {
        serde_json::map::Entry::Vacant(vac) => {
            vac.insert(serde_json::Value::Array(vec![entry]));
        }
        serde_json::map::Entry::Occupied(mut occ) => {
            if let serde_json::Value::Array(arr) = occ.get_mut() {
                arr.push(entry);
            } else {
                occ.insert(serde_json::Value::Array(vec![entry]));
            }
        }
    }
}

fn append_follow_ups(
    content: &mut serde_json::Value,
    follow_ups: &[super::follow_up::FollowUpAction],
) {
    if follow_ups.is_empty() {
        return;
    }

    let entries = follow_ups
        .iter()
        .map(super::follow_up::FollowUpAction::model_facing_json)
        .collect();

    let Some(map) = content.as_object_mut() else {
        let original = content.clone();
        *content = serde_json::json!({
            "_original": original,
            "follow_ups": entries,
        });
        return;
    };
    map.insert("follow_ups".to_string(), serde_json::Value::Array(entries));
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::error::ToolError;
    use crate::r#loop::runner::ToolExecutor;
    use crate::tool::context::{ToolContext, ToolFlag};
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::follow_up::{
        BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction,
    };
    use crate::tool::lifecycle::{
        Advisory, AdvisorySeverity, PostCheckResult, PostValidateMode, PostValidateOutcome,
        PreValidateOutcome, RuntimeOnSuccessAction, RuntimePostValidateCheck,
        RuntimePreValidateCheck,
    };
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::ToolOutput;

    // -- Stubs ------------------------------------------------------------

    struct StubTool {
        tool_name: String,
    }

    impl StubTool {
        fn new(name: &str) -> Self {
            Self {
                tool_name: name.to_string(),
            }
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "stub"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(serde_json::json!(null)))
        }
    }

    /// Lifecycle-instrumented tool used to assert that every phase fires (or
    /// does not fire) in the right order under each configuration.
    struct StatefulStubTool {
        tool_name: String,
        pre_outcome: PreValidateOutcome,
        post_outcome: PostValidateOutcome,
        post_mode: PostValidateMode,
        exec_should_fail: bool,
        executed: Arc<AtomicBool>,
        on_success_count: Arc<AtomicUsize>,
        register_follow_ups_count: Arc<AtomicUsize>,
        follow_ups_to_return: Vec<FollowUpAction>,
        output_payload: serde_json::Value,
    }

    impl StatefulStubTool {
        fn new(name: &str) -> Self {
            Self {
                tool_name: name.to_string(),
                pre_outcome: PreValidateOutcome::Proceed,
                post_outcome: PostValidateOutcome::Pass,
                post_mode: PostValidateMode::Report,
                exec_should_fail: false,
                executed: Arc::new(AtomicBool::new(false)),
                on_success_count: Arc::new(AtomicUsize::new(0)),
                register_follow_ups_count: Arc::new(AtomicUsize::new(0)),
                follow_ups_to_return: Vec::new(),
                output_payload: serde_json::json!({"ok": true}),
            }
        }
    }

    #[async_trait]
    impl Tool for StatefulStubTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "stateful stub"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        fn post_validate_mode(&self) -> PostValidateMode {
            self.post_mode
        }

        async fn pre_validate(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> PreValidateOutcome {
            self.pre_outcome.clone()
        }

        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            self.executed.store(true, Ordering::SeqCst);
            if self.exec_should_fail {
                return Err(ToolError::ExecutionFailed {
                    reason: "boom".to_string(),
                });
            }
            Ok(ToolOutput::success(self.output_payload.clone()))
        }

        async fn post_validate(
            &self,
            _output: &ToolOutput,
            _ctx: &ToolContext,
        ) -> PostValidateOutcome {
            self.post_outcome.clone()
        }

        async fn on_success(&self, _output: &ToolOutput, _ctx: &ToolContext) {
            self.on_success_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn register_follow_ups(
            &self,
            _output: &ToolOutput,
            _ctx: &ToolContext,
        ) -> Vec<FollowUpAction> {
            self.register_follow_ups_count
                .fetch_add(1, Ordering::SeqCst);
            self.follow_ups_to_return.clone()
        }
    }

    /// Runtime pre-check that always blocks with a fixed reason — exercises
    /// the runtime portion of pre-validation.
    struct BlockingPreCheck {
        reason: String,
    }

    #[async_trait]
    impl RuntimePreValidateCheck for BlockingPreCheck {
        async fn check(&self, _envelope: &ToolEnvelope, _ctx: &ToolContext) -> PreValidateOutcome {
            PreValidateOutcome::block(self.reason.clone())
        }
    }

    /// Runtime post-check that always fails — exercises the runtime portion
    /// of post-validation under both `Gate` and `Report` modes.
    struct FailingPostCheck;

    #[async_trait]
    impl RuntimePostValidateCheck for FailingPostCheck {
        async fn check(&self, _output: &ToolOutput, _ctx: &ToolContext) -> PostCheckResult {
            PostCheckResult::fail(vec!["runtime post fail".to_string()])
        }
    }

    struct AdvisoryPostCheck {
        outcome: PostValidateOutcome,
        advisories: Vec<Advisory>,
    }

    #[async_trait]
    impl RuntimePostValidateCheck for AdvisoryPostCheck {
        async fn check(&self, _output: &ToolOutput, _ctx: &ToolContext) -> PostCheckResult {
            PostCheckResult {
                outcome: self.outcome.clone(),
                advisories: self.advisories.clone(),
            }
        }
    }

    /// Runtime on-success action that bumps a counter, used to assert
    /// on-success runs (or does not run) under each scenario.
    struct CountingOnSuccess {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RuntimeOnSuccessAction for CountingOnSuccess {
        async fn run(&self, _output: &ToolOutput, _ctx: &ToolContext) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    // -- R1/R5: legacy storage tests (preserved) --------------------------

    #[test]
    fn register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("read")));
        reg.register(Box::new(StubTool::new("write")));
        reg.register(Box::new(StubTool::new("edit")));

        assert_eq!(reg.len(), 3);
        assert!(!reg.is_empty());

        assert!(reg.get("read").is_some());
        assert_eq!(reg.get("read").map(Tool::name), Some("read"));
        assert!(reg.get("write").is_some());
        assert!(reg.get("edit").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn names_returns_all_registered() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("alpha")));
        reg.register(Box::new(StubTool::new("beta")));

        let mut names: Vec<&str> = reg.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn empty_registry() {
        let reg = ToolRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("anything").is_none());
    }

    // -- R4: dynamic tool availability ------------------------------------

    /// R4 acceptance: `set_available(names)` restricts `get()` results.
    #[test]
    fn set_available_restricts_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("a")));
        reg.register(Box::new(StubTool::new("b")));
        reg.register(Box::new(StubTool::new("c")));

        reg.set_available(vec!["a".to_string()]);
        assert!(reg.get("a").is_some(), "available tool must be reachable");
        assert!(reg.get("b").is_none(), "gated tool must be unreachable");
        assert!(reg.get("c").is_none(), "gated tool must be unreachable");
        assert_eq!(reg.len(), 1, "len reflects available tools");
        let mut visible: Vec<&str> = reg.names().collect();
        visible.sort_unstable();
        assert_eq!(visible, vec!["a"], "names reflects available tools");

        reg.reset_available();
        assert!(reg.get("a").is_some());
        assert!(reg.get("b").is_some());
        assert!(reg.get("c").is_some());
        assert_eq!(reg.len(), 3);
    }

    /// H17: `set_disallowed` gates lookups even without an allow-list,
    /// and continues to gate tools registered after the call.
    #[test]
    fn set_disallowed_gates_get_names_and_len() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("read")));
        reg.register(Box::new(StubTool::new("write")));

        reg.set_disallowed(vec!["write".to_string()]);
        assert!(reg.get("read").is_some());
        assert!(reg.get("write").is_none(), "disallowed tool unreachable");
        assert_eq!(reg.len(), 1);
        let visible: Vec<&str> = reg.names().collect();
        assert_eq!(visible, vec!["read"]);

        // Late registration of a disallowed name stays gated.
        reg.set_disallowed(vec!["write".to_string(), "late-write".to_string()]);
        reg.register(Box::new(StubTool::new("late-write")));
        assert!(reg.get("late-write").is_none());
    }

    /// H17: a name present in both the allow-list and the deny-list is
    /// unavailable — deny wins.
    #[test]
    fn disallowed_wins_over_available() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("read")));
        reg.register(Box::new(StubTool::new("write")));
        reg.set_available(vec!["read".to_string(), "write".to_string()]);
        reg.set_disallowed(vec!["write".to_string()]);
        assert!(reg.get("read").is_some());
        assert!(
            reg.get("write").is_none(),
            "deny must win over the availability allow-list",
        );
    }

    /// The registry publishes a [`ToolEffectIndex`] on its shared
    /// context and keeps it in sync across register / remove /
    /// set_context.
    #[test]
    fn effect_index_published_and_synced() {
        use crate::tool::scheduling::ToolEffectIndex;

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("read")));

        let shared = reg.shared_context().expect("registry exposes context");
        let index = shared
            .get_extension::<ToolEffectIndex>()
            .expect("effect index published on shared context");
        assert_eq!(
            index.effect_for("read", &serde_json::json!({})),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            index.effect_for("ghost", &serde_json::json!({})),
            ToolEffect::Unknown,
        );

        reg.remove("read");
        assert_eq!(
            index.effect_for("read", &serde_json::json!({})),
            ToolEffect::Unknown,
        );

        // Swapping the context re-publishes the same index.
        reg.register(Box::new(StubTool::new("again")));
        reg.set_context(Arc::new(ToolContext::empty()));
        let swapped = reg.shared_context().expect("context after swap");
        let index = swapped
            .get_extension::<ToolEffectIndex>()
            .expect("effect index re-published after set_context");
        assert_eq!(
            index.effect_for("again", &serde_json::json!({})),
            ToolEffect::ReadOnly,
        );
    }

    /// R4 acceptance: unavailable tools return `ToolNotFound` from dispatch.
    #[tokio::test]
    async fn unavailable_tool_returns_tool_not_found() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("a")));
        reg.register(Box::new(StubTool::new("b")));

        reg.set_available(vec!["a".to_string()]);
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("b", "test-call", serde_json::json!({}))
            .await
            .expect_err("gated tool must error");
        match err {
            ToolError::ToolNotFound { name } => assert_eq!(name, "b"),
            other => panic!("expected ToolNotFound, got {other:?}"),
        }
    }

    // -- R5: ToolRegistry runtime mutation --------------------------------

    /// R5 acceptance: `remove()` returns the boxed tool, `get()` then misses,
    /// and the availability set drops the removed name to stay consistent.
    #[test]
    fn remove_returns_box_and_invalidates_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("alpha")));

        let removed = reg.remove("alpha");
        assert!(removed.is_some(), "remove returns the boxed tool");
        assert!(reg.get("alpha").is_none(), "get after remove must miss",);
        assert!(reg.remove("alpha").is_none(), "second remove is None");
    }

    /// R5 supporting test: removing a tool that was in the availability set
    /// also clears it from the set so cached state stays consistent.
    #[test]
    fn remove_clears_availability_entry() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("alpha")));
        reg.register(Box::new(StubTool::new("beta")));
        reg.set_available(vec!["alpha".to_string(), "beta".to_string()]);

        let removed = reg.remove("alpha");
        assert!(removed.is_some());
        assert!(reg.get("alpha").is_none(), "removed name must not resolve");
        assert!(reg.get("beta").is_some(), "remaining tool stays reachable");
        assert_eq!(reg.len(), 1, "len drops to 1 after remove");
    }

    /// R5 acceptance: register a tool at runtime then dispatch it via the
    /// `ToolExecutor` impl — verifies that `register` makes a tool
    /// immediately available for the full lifecycle path.
    #[tokio::test]
    async fn register_at_runtime_executes() {
        let mut reg = ToolRegistry::new();
        // Register AFTER constructing the registry; dispatch must succeed.
        reg.register(Box::new(StubTool::new("late")));
        let executor: &dyn ToolExecutor = &reg;
        let out = executor
            .execute("late", "test-call", serde_json::json!({}))
            .await
            .expect("dispatch succeeds for late-registered tool");
        assert_eq!(out, serde_json::json!(null));
    }

    // -- R1: ToolRegistry implements ToolExecutor + full lifecycle --------

    /// R1 verification bullet: `ToolRegistry` satisfies the `ToolExecutor`
    /// trait — this static coercion compiles only if the impl exists.
    #[test]
    fn tool_registry_implements_tool_executor() {
        let reg = ToolRegistry::new();
        let _executor: &dyn ToolExecutor = &reg;
    }

    /// R1 acceptance: pre-validate `Block` prevents execution and surfaces
    /// the reason via `ToolError::PreValidationFailed`.
    #[tokio::test]
    async fn pre_validate_block_prevents_execution() {
        let mut tool = StatefulStubTool::new("blocked");
        tool.pre_outcome = PreValidateOutcome::block("must-read-first");
        let executed = tool.executed.clone();
        let on_success_count = tool.on_success_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("blocked", "test-call", serde_json::json!({}))
            .await
            .expect_err("must error");
        match err {
            ToolError::PreValidationFailed { payload } => {
                assert_eq!(payload.message, "must-read-first");
                assert_eq!(payload.kind, crate::tool::failure::ToolErrorKind::Blocked);
            }
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
        assert!(!executed.load(Ordering::SeqCst), "execute must not run");
        assert_eq!(
            on_success_count.load(Ordering::SeqCst),
            0,
            "on_success must not run",
        );
    }

    /// A structured block survives dispatch as a typed payload: the kind,
    /// message, and guidance all reach the `PreValidationFailed` error, and
    /// the rendered `Display` (what logs show) still carries
    /// message-plus-guidance.
    #[tokio::test]
    async fn block_structure_survives_into_pre_validation_error() {
        use crate::tool::failure::ToolErrorKind;
        use crate::tool::lifecycle::BlockDecision;

        let mut tool = StatefulStubTool::new("guided-block");
        tool.pre_outcome = PreValidateOutcome::Block(
            BlockDecision::new("file has not been read")
                .with_kind(ToolErrorKind::PermissionDenied)
                .with_guidance("read the file first")
                .with_detail(serde_json::json!({ "path": "a.rs" })),
        );

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("guided-block", "test-call", serde_json::json!({}))
            .await
            .expect_err("block must error");
        let rendered = err.to_string();
        assert!(
            rendered.contains("file has not been read")
                && rendered.contains("Guidance: read the file first"),
            "Display must keep message+guidance readable: {rendered}",
        );
        match err {
            ToolError::PreValidationFailed { payload } => {
                assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
                assert_eq!(payload.message, "file has not been read");
                assert_eq!(payload.guidance(), Some("read the file first"));
                assert_eq!(payload.detail["path"], "a.rs");
            }
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
    }

    /// The registry stamps execution duration on dispatched outputs — tools
    /// construct outputs with `Duration::ZERO` and never time themselves.
    #[tokio::test]
    async fn registry_stamps_execution_duration() {
        struct SleepyTool;

        #[async_trait]
        impl Tool for SleepyTool {
            fn name(&self) -> &str {
                "sleepy"
            }
            fn description(&self) -> &str {
                "sleeps"
            }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn effect(&self) -> ToolEffect {
                ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                tokio::time::sleep(Duration::from_millis(15)).await;
                Ok(ToolOutput::success(serde_json::json!({"ok": true})))
            }

            async fn on_success(&self, output: &ToolOutput, _ctx: &ToolContext) {
                // The stamped duration is already visible to later lifecycle
                // phases (hooks, on-success), not just the final caller.
                assert!(
                    output.duration >= Duration::from_millis(15),
                    "duration must be stamped before on_success; got {:?}",
                    output.duration,
                );
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(SleepyTool));
        let executor: &dyn ToolExecutor = &reg;
        executor
            .execute("sleepy", "test-call", serde_json::json!({}))
            .await
            .expect("dispatch succeeds");
    }

    /// R1 supporting: runtime pre-checks also gate execution.
    #[tokio::test]
    async fn runtime_pre_check_block_prevents_execution() {
        let tool = StatefulStubTool::new("any");
        let executed = tool.executed.clone();

        let mut ctx = ToolContext::empty();
        ctx.pre_checks.push(Box::new(BlockingPreCheck {
            reason: "policy".to_string(),
        }));
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("any", "test-call", serde_json::json!({}))
            .await
            .expect_err("runtime check must block");
        match err {
            ToolError::PreValidationFailed { payload } => assert_eq!(payload.message, "policy"),
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
        assert!(!executed.load(Ordering::SeqCst));
    }

    /// R1 acceptance: post-validate `Gate` failure returns an error and
    /// skips on-success — i.e. the change is not "committed".
    #[tokio::test]
    async fn post_validate_gate_fail_returns_error() {
        let mut tool = StatefulStubTool::new("gated");
        tool.post_mode = PostValidateMode::Gate;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["syntax error".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});
        let on_success_count = tool.on_success_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("gated", "test-call", serde_json::json!({}))
            .await
            .expect_err("gate-mode failure must surface as error");
        match err {
            ToolError::PostValidationFailed {
                reason,
                committed_output,
                ..
            } => {
                assert!(
                    reason.contains("syntax error"),
                    "reason must mention failure: {reason}",
                );
                assert_eq!(
                    committed_output
                        .as_ref()
                        .and_then(|v| v.get("committed"))
                        .and_then(serde_json::Value::as_bool),
                    Some(true),
                    "committed_output must carry committed: true",
                );
            }
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
        assert_eq!(
            on_success_count.load(Ordering::SeqCst),
            0,
            "on_success must not run on gate failure",
        );
    }

    /// R1 acceptance: post-validate `Report` failure returns the committed
    /// tool output with a structured validation error and still runs
    /// on-success — post-commit validation errors must not prevent
    /// bookkeeping, but they must be model-visible.
    #[tokio::test]
    async fn post_validate_report_fail_returns_output_and_runs_on_success() {
        let mut tool = StatefulStubTool::new("report");
        tool.post_mode = PostValidateMode::Report;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["nonblocking warning".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "warnings": ["x"]});
        let on_success_count = tool.on_success_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute("report", "test-call", serde_json::json!({}))
            .await
            .expect("report-mode failure surfaces as Ok with the tool payload");
        assert_eq!(out["committed"], true);
        assert_eq!(out["warnings"][0], "x");
        assert_eq!(out["error"]["kind"], "validation_failed");
        assert!(
            out["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("must be fixed properly"),
            "validation error must tell the model this is required work",
        );
        assert_eq!(out["post_validation_errors"][0], "nonblocking warning");
        assert!(
            out["validation_guidance"]
                .as_str()
                .unwrap_or_default()
                .contains("not optional notes"),
            "report-mode guidance must be firm",
        );
        assert_eq!(
            on_success_count.load(Ordering::SeqCst),
            1,
            "on_success runs in report mode despite advisory errors",
        );
    }

    #[tokio::test]
    async fn runtime_post_check_advisories_are_injected_on_success() {
        let tool = StatefulStubTool::new("advisory-success");

        let mut ctx = ToolContext::empty();
        ctx.post_checks.push(Box::new(AdvisoryPostCheck {
            outcome: PostValidateOutcome::Pass,
            advisories: vec![Advisory {
                severity: AdvisorySeverity::Warning,
                message: "line count is getting high".to_string(),
                source: "file-length".to_string(),
            }],
        }));
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute("advisory-success", "test-call", serde_json::json!({}))
            .await
            .expect("success path should include advisories in output");
        let advisories = out["advisories"]
            .as_array()
            .expect("expected advisories array");
        assert_eq!(advisories.len(), 1);
        assert_eq!(advisories[0]["severity"], "Warning");
        assert_eq!(advisories[0]["message"], "line count is getting high");
        assert_eq!(advisories[0]["source"], "file-length");
        assert_eq!(advisories[0]["required"], true);
        assert!(
            advisories[0]["guidance"]
                .as_str()
                .unwrap_or_default()
                .contains("not optional notes"),
            "advisory guidance must be explicit that the finding is required work",
        );
        assert!(
            out["advisory_policy"]
                .as_str()
                .unwrap_or_default()
                .contains("Fix the underlying issue properly"),
            "top-level advisory policy must be visible to the model",
        );
    }

    #[tokio::test]
    async fn runtime_post_check_gate_fail_preserves_advisories_in_committed_output() {
        let mut tool = StatefulStubTool::new("advisory-gate-fail");
        tool.post_mode = PostValidateMode::Gate;
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});

        let mut ctx = ToolContext::empty();
        ctx.post_checks.push(Box::new(AdvisoryPostCheck {
            outcome: PostValidateOutcome::Fail {
                errors: vec!["blocking failure".to_string()],
            },
            advisories: vec![Advisory {
                severity: AdvisorySeverity::Info,
                message: "consider splitting this module".to_string(),
                source: "conventions".to_string(),
            }],
        }));
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("advisory-gate-fail", "test-call", serde_json::json!({}))
            .await
            .expect_err("gate-mode failure should still return committed output");
        match err {
            ToolError::PostValidationFailed {
                reason,
                committed_output,
                ..
            } => {
                assert!(reason.contains("blocking failure"));
                let committed_output = committed_output.expect("expected committed output");
                assert_eq!(committed_output["committed"], true);
                let advisories = committed_output["advisories"]
                    .as_array()
                    .expect("expected advisories array");
                assert_eq!(advisories.len(), 1);
                assert_eq!(advisories[0]["severity"], "Info");
                assert_eq!(advisories[0]["message"], "consider splitting this module");
                assert_eq!(advisories[0]["source"], "conventions");
                assert_eq!(advisories[0]["required"], true);
                assert_eq!(
                    committed_output["post_validation_errors"][0],
                    "blocking failure",
                );
            }
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    /// R1 acceptance: on-success runs only when both compile-time and
    /// runtime post-validation pass. The compile-time bump comes from the
    /// `StatefulStubTool::on_success` impl; the runtime bump comes from the
    /// `CountingOnSuccess` action in `ToolContext`.
    #[tokio::test]
    async fn on_success_runs_when_post_validate_passes() {
        let tool = StatefulStubTool::new("happy");
        let compile_count = tool.on_success_count.clone();
        let runtime_count = Arc::new(AtomicUsize::new(0));

        let mut ctx = ToolContext::empty();
        ctx.on_success_actions.push(Box::new(CountingOnSuccess {
            counter: runtime_count.clone(),
        }));
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute("happy", "test-call", serde_json::json!({}))
            .await
            .expect("happy path returns Ok");
        assert_eq!(out["ok"], true);
        assert_eq!(
            compile_count.load(Ordering::SeqCst),
            1,
            "compile-time on_success runs exactly once",
        );
        assert_eq!(
            runtime_count.load(Ordering::SeqCst),
            1,
            "runtime on_success runs exactly once",
        );
    }

    /// R1 supporting: runtime post-check failure under `Gate` mode surfaces
    /// as an error and skips on-success.
    #[tokio::test]
    async fn runtime_post_check_gate_fail_returns_error() {
        let mut tool = StatefulStubTool::new("gated-runtime");
        tool.post_mode = PostValidateMode::Gate;
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});
        let on_success_count = tool.on_success_count.clone();

        let mut ctx = ToolContext::empty();
        ctx.post_checks.push(Box::new(FailingPostCheck));
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("gated-runtime", "test-call", serde_json::json!({}))
            .await
            .expect_err("runtime post failure under Gate must error");
        match err {
            ToolError::PostValidationFailed {
                committed_output, ..
            } => assert_eq!(
                committed_output
                    .as_ref()
                    .and_then(|v| v.get("committed"))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "committed_output must carry committed: true",
            ),
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
        assert_eq!(on_success_count.load(Ordering::SeqCst), 0);
    }

    /// R1 acceptance: dispatching an unregistered name returns
    /// `ToolError::ToolNotFound` with the requested name attached.
    #[tokio::test]
    async fn tool_not_found_returns_error() {
        let reg = ToolRegistry::new();
        let executor: &dyn ToolExecutor = &reg;
        let err = executor
            .execute("ghost", "test-call", serde_json::json!({}))
            .await
            .expect_err("unregistered tool errors");
        match err {
            ToolError::ToolNotFound { name } => assert_eq!(name, "ghost"),
            other => panic!("expected ToolNotFound, got {other:?}"),
        }
    }

    /// R5 acceptance: a Report-mode tool with `ForceGate` flag promotes
    /// to Gate semantics and post-validate failures surface as errors.
    #[tokio::test]
    async fn force_gate_promotes_report_to_gate() {
        let mut tool = StatefulStubTool::new("force-gate");
        tool.post_mode = PostValidateMode::Report;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["ast invalid".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});

        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::ForceGate, "test:force-gate");
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("force-gate", "test-call", serde_json::json!({}))
            .await
            .expect_err("ForceGate must promote Report to Gate");
        match err {
            ToolError::PostValidationFailed {
                reason,
                committed_output,
                ..
            } => {
                assert!(reason.contains("ast invalid"));
                assert_eq!(
                    committed_output
                        .as_ref()
                        .and_then(|v| v.get("committed"))
                        .and_then(serde_json::Value::as_bool),
                    Some(true),
                    "committed_output must carry committed: true",
                );
            }
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    /// R5 acceptance: a Gate-mode tool with `AllowBrokenAst` flag
    /// demotes to Report semantics and post-validate failures return
    /// the tool's output rather than erroring.
    #[tokio::test]
    async fn allow_broken_ast_demotes_gate_to_report() {
        let mut tool = StatefulStubTool::new("allow-broken");
        tool.post_mode = PostValidateMode::Gate;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["ast invalid".to_string()],
        };

        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::AllowBrokenAst, "test:allow");
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute("allow-broken", "test-call", serde_json::json!({}))
            .await
            .expect("AllowBrokenAst must demote Gate to Report");
        let overrides = out
            .get("check_overrides")
            .and_then(serde_json::Value::as_array)
            .expect("expected check_overrides array");
        assert!(
            overrides
                .iter()
                .any(|o| o.get("check_name").and_then(serde_json::Value::as_str)
                    == Some("post_validate_mode"))
        );
    }

    /// R5 acceptance: `ForceGate` and `AllowBrokenAst` together — the
    /// explicit `ForceGate` wins.
    #[tokio::test]
    async fn force_gate_beats_allow_broken_ast() {
        let mut tool = StatefulStubTool::new("both-flags");
        tool.post_mode = PostValidateMode::Report;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["err".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});

        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::ForceGate, "test:force");
        ctx.set_flag(ToolFlag::AllowBrokenAst, "test:allow");
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("both-flags", "test-call", serde_json::json!({}))
            .await
            .expect_err("ForceGate must win over AllowBrokenAst");
        match err {
            ToolError::PostValidationFailed {
                committed_output, ..
            } => assert_eq!(
                committed_output
                    .as_ref()
                    .and_then(|v| v.get("committed"))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "committed_output must carry committed: true",
            ),
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    /// R5 acceptance: no override flag on a Gate-mode tool leaves the
    /// mode unchanged; the tool default still applies.
    #[tokio::test]
    async fn no_override_flag_keeps_tool_default_mode() {
        let mut tool = StatefulStubTool::new("default-mode");
        tool.post_mode = PostValidateMode::Gate;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["err".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("default-mode", "test-call", serde_json::json!({}))
            .await
            .expect_err("Gate without override must still error");
        match err {
            ToolError::PostValidationFailed {
                committed_output, ..
            } => assert_eq!(
                committed_output
                    .as_ref()
                    .and_then(|v| v.get("committed"))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "committed_output must carry committed: true",
            ),
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    /// R5 acceptance: `RejectBrokenAst` promotes Report to Gate
    /// independently of `ForceGate`.
    #[tokio::test]
    async fn reject_broken_ast_promotes_report_to_gate() {
        let mut tool = StatefulStubTool::new("reject-broken");
        tool.post_mode = PostValidateMode::Report;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["broken".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true, "ok": true});

        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::RejectBrokenAst, "test:reject");
        let mut reg = ToolRegistry::with_context(Arc::new(ctx));
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("reject-broken", "test-call", serde_json::json!({}))
            .await
            .expect_err("RejectBrokenAst must promote Report to Gate");
        match err {
            ToolError::PostValidationFailed {
                committed_output, ..
            } => assert_eq!(
                committed_output
                    .as_ref()
                    .and_then(|v| v.get("committed"))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "committed_output must carry committed: true",
            ),
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    /// R1 supporting: execute-phase error short-circuits the rest of the
    /// lifecycle. `on_success` must not run, and the error propagates.
    #[tokio::test]
    async fn execute_error_short_circuits_lifecycle() {
        let mut tool = StatefulStubTool::new("boom");
        tool.exec_should_fail = true;
        let on_success_count = tool.on_success_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("boom", "test-call", serde_json::json!({}))
            .await
            .expect_err("execute failure surfaces");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert_eq!(on_success_count.load(Ordering::SeqCst), 0);
    }

    // -- NTF-002: register_follow_ups lifecycle phase ---------------------

    /// Build a minimally-valid [`FollowUpAction`] for lifecycle tests.
    fn sample_follow_up(action: &str) -> FollowUpAction {
        FollowUpAction {
            action: action.to_string(),
            description: format!("follow-up {action}"),
            tool: "apply_patch".to_string(),
            args: serde_json::json!({}),
            args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
            expires: ExpiryCondition::Never,
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }
    }

    /// R5(1)/C35/C39: a tool relying on the trait's default
    /// `register_follow_ups` produces an empty follow-up vector and leaves no
    /// `follow_ups` key in the output. `StubTool` does not override the hook,
    /// so this exercises the default body directly.
    #[tokio::test]
    async fn default_register_follow_ups_adds_no_key() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::new("no-follow-ups")));
        let executor: &dyn ToolExecutor = &reg;

        let out = executor
            .execute("no-follow-ups", "test-call", serde_json::json!({}))
            .await
            .expect("dispatch succeeds");
        assert!(
            out.get("follow_ups").is_none(),
            "no follow_ups key when the vector is empty",
        );
    }

    /// `Tool::register_follow_ups` contract: the hook runs after a
    /// gate-mode post-validation failure, and the registered actions are
    /// attached to the committed output carried by the
    /// `PostValidationFailed` error — while on-success bookkeeping still
    /// does not run.
    #[tokio::test]
    async fn gate_failure_attaches_registered_follow_ups() {
        let mut tool = StatefulStubTool::new("gate-follow-ups");
        tool.post_mode = PostValidateMode::Gate;
        tool.post_outcome = PostValidateOutcome::Fail {
            errors: vec!["broken".to_string()],
        };
        tool.output_payload = serde_json::json!({"committed": true});
        tool.follow_ups_to_return = vec![sample_follow_up("undo")];
        let register_count = tool.register_follow_ups_count.clone();
        let on_success_count = tool.on_success_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("gate-follow-ups", "test-call", serde_json::json!({}))
            .await
            .expect_err("gate failure surfaces as error");
        match err {
            ToolError::PostValidationFailed {
                committed_output, ..
            } => {
                let committed = committed_output.expect("committed output present");
                let follow_ups = committed["follow_ups"]
                    .as_array()
                    .expect("follow_ups attached to committed output");
                assert_eq!(follow_ups.len(), 1);
                assert_eq!(follow_ups[0]["action"], "undo");
            }
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
        assert_eq!(
            register_count.load(Ordering::SeqCst),
            1,
            "register_follow_ups must run on the gate-failure path",
        );
        assert_eq!(
            on_success_count.load(Ordering::SeqCst),
            0,
            "on_success must still not run on gate failure",
        );
    }

    /// R5(4)/C37: an execute-phase error short-circuits before
    /// `register_follow_ups` can run — the hook must never be called when no
    /// output was produced.
    #[tokio::test]
    async fn execute_error_does_not_call_register_follow_ups() {
        let mut tool = StatefulStubTool::new("exec-fail-follow-ups");
        tool.exec_should_fail = true;
        tool.follow_ups_to_return = vec![sample_follow_up("undo")];
        let register_count = tool.register_follow_ups_count.clone();

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tool));
        let executor: &dyn ToolExecutor = &reg;

        let err = executor
            .execute("exec-fail-follow-ups", "test-call", serde_json::json!({}))
            .await
            .expect_err("execute failure surfaces");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert_eq!(
            register_count.load(Ordering::SeqCst),
            0,
            "register_follow_ups must not run when execute fails",
        );
    }
}
