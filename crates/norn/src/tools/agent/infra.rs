//! Shared agent-coordination infrastructure and helpers.
//!
//! Orchestrators publish an [`AgentToolInfra`] on the [`ToolContext`]
//! extension map. The four agent-coordination tools (`SpawnAgentTool`,
//! `ForkTool`, `SignalAgentTool`, `CloseAgentTool`)
//! fetch it via [`ToolContext::get_extension`]. A missing infra is a hard
//! configuration error — never a panic.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::child_policy::ChildPolicy;
use crate::agent::message_router::MessageRouter;
use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::registry::{AgentEntry, AgentRegistry, AgentTombstone};
use crate::error::ToolError;
use crate::r#loop::runner::ToolExecutor;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope, split_envelope_fields};
use crate::tool::lifecycle::{
    Advisory, PostCheckResult, PostValidateMode, PostValidateOutcome, PreValidateOutcome,
};
use crate::tool::registry::{
    ToolRegistry, append_advisories, append_check_override, resolved_post_validate_mode,
};

/// Shared agent-coordination infrastructure surfaced to tools.
///
/// Orchestrators construct this once per agent and insert it into the
/// [`ToolContext`] extension map.
pub struct AgentToolInfra {
    /// Active-agent registry shared across the workspace.
    pub registry: Arc<RwLock<AgentRegistry>>,
    /// Router delivering inter-agent messages onto recipients' inbound
    /// channels (shared workspace-wide, like the registry).
    pub router: Arc<MessageRouter>,
    /// Durable pending messages accepted for dormant-but-resumable agents.
    ///
    /// This is shared by the whole agent tree. `signal_agent` records into it
    /// only when a message has a real future consumer: an explicit
    /// resume/wake path that drains this store before the resumed provider
    /// request. Live routes continue through [`Self::router`].
    pub pending_messages: Arc<PendingAgentMessages>,
    /// Provider used for sub-agent and fork model calls.
    pub provider: Arc<dyn Provider>,
    /// Parent agent's session event store.
    pub event_store: Arc<EventStore>,
    /// Calling agent's id (the sender for outbound messages, parent for spawn).
    pub agent_id: Uuid,
    /// Calling agent's parent id, if any (for genealogy).
    ///
    /// Invariant on harness-built contexts: `parent_id` and [`Self::grant`]
    /// are `Some` together (spawn/fork children) or `None` together (root
    /// agents). `signal_agent` rejects a context that has a parent but no
    /// grant with a typed configuration error rather than inventing a
    /// scope; the grant's two halves cannot diverge because they travel in
    /// one [`ParentGrant`].
    pub parent_id: Option<Uuid>,
    /// The coordination grant stamped onto *this* agent by its spawning
    /// parent at launch. `None` for root agents, which have no granting
    /// parent — a root's `signal_agent` is governed structurally instead
    /// (it may message its own children, unrestricted by a granted scope).
    pub grant: Option<ParentGrant>,
    /// Shared tool registry handed to spawned and forked sub-agents. When
    /// `None`, spawn and fork report a configuration error — they refuse to
    /// silently launch a sub-agent that has no tools available. The registry
    /// itself is never mutated through this handle; callers wrap it in a
    /// [`SubAgentExecutor`] with an optional per-spawn allow-list instead.
    pub tool_registry: Option<Arc<ToolRegistry>>,
}

/// The calling agent's own run-cancellation token, published on the
/// agent's [`ToolContext`] extension map alongside [`AgentToolInfra`]
/// (Wave 3 §Cancellation cascade, W3.5).
///
/// Spawn/fork launch paths read this at the spawn site and create each
/// child's run token as
/// [`child_token()`](tokio_util::sync::CancellationToken::child_token)
/// of it, so cancelling any agent's token cancels its entire spawned
/// subtree: every descendant's loop observes its own (cascaded) token at
/// its next cancellation boundary, returns `Cancelled { usage }`, and its
/// completion wrapper runs the normal terminal sequence — a cancelled
/// tree is fully accounted, never a set of aborted tasks.
///
/// Recorded deviation from the design's letter: §"Cancellation cascade"
/// places this token as a `cancel` field on [`AgentToolInfra`]. It
/// travels as its own extension instead because `AgentToolInfra` is also
/// constructed by embedder assembly paths that own no run token
/// (norn-cli `install_agent_tool_infra`, norn-tui rotation) — a required
/// field there would force those roots to invent a token they do not
/// control. The root boundary is therefore explicit and honest:
///
/// - **Builder-assembled roots** always publish this extension (the
///   builder's `cancel_token`, or its own fresh token), so the cascade
///   covers the whole tree from the root down.
/// - **Roots that publish no token** (the CLI/TUI assembly paths today)
///   launch their *direct* children with free-standing tokens — exactly
///   the pre-W3.5 behavior. Each child's own token is still published on
///   the child's context at construction, so the cascade holds from
///   depth 1 downward regardless of the root's wiring.
pub struct AgentCancellation(pub tokio_util::sync::CancellationToken);

/// The coordination grant a spawning parent stamps onto a child's
/// [`AgentToolInfra`]: the child's granted [`ChildPolicy`] (from the
/// parent's
/// [`CoordinationEnvelope`](crate::agent::child_policy::CoordinationEnvelope))
/// plus the parent's session event store, where `signal_agent` appends
/// the `agent_message.sent` audit record *in addition to* the sender's
/// own store — the dual-store rule (Wave 3 §Audit trail) that lets a
/// parent observe every message its children exchange without sitting on
/// the data path.
///
/// Bundled deliberately: a policy without the parent store would
/// silently halve the audit trail, so that state is unrepresentable.
#[derive(Clone, Debug)]
pub struct ParentGrant {
    /// The [`ChildPolicy`] granted to the child by its spawning parent.
    pub policy: ChildPolicy,
    /// The scope-granting parent's session event store.
    pub parent_store: Arc<EventStore>,
}

/// Fetches the configured [`AgentToolInfra`] from the context, returning
/// a typed [`ToolError::MissingExtension`] naming the missing type when
/// none is present.
pub(super) fn infra_from(ctx: &ToolContext) -> Result<Arc<AgentToolInfra>, ToolError> {
    ctx.require_extension::<AgentToolInfra>()
}

/// Result of resolving a string identifier against the agent registry.
pub(crate) enum ResolvedAgent {
    /// The identifier names a registered agent — live, or terminal but
    /// not yet reclaimed (its entry still carries the real outcome).
    Live(AgentEntry),
    /// The identifier names an agent that finished and was reclaimed;
    /// the registry retains its completion record
    /// ([`AgentTombstone`](crate::agent::registry::AgentTombstone)).
    Reclaimed(AgentTombstone),
}

/// Resolves a string identifier (hierarchical path or raw UUID) against
/// the registry, including agents that already finished.
///
/// Resolution order: live holder of the path → terminal-but-unreclaimed
/// holder of the path → completion record of the most recently reclaimed
/// holder → (for UUIDs) registered entry → completion record.
///
/// "Not registered" is only ever reported for identifiers with no record
/// at all: an agent that completed and was reclaimed resolves to
/// [`ResolvedAgent::Reclaimed`] so callers tell the truth about it
/// ("already completed at \<ts\>") instead of denying it ever existed.
pub(crate) fn resolve_agent(
    registry: &Arc<RwLock<AgentRegistry>>,
    identifier: &str,
) -> Result<ResolvedAgent, ToolError> {
    let reg = registry.read();
    if let Some(entry) = reg.get_by_path(identifier) {
        return Ok(ResolvedAgent::Live(entry));
    }
    if let Some(entry) = reg.get_terminal_by_path(identifier) {
        return Ok(ResolvedAgent::Live(entry));
    }
    if let Some(tombstone) = reg.tombstone_by_path(identifier) {
        return Ok(ResolvedAgent::Reclaimed(tombstone));
    }
    if let Ok(uuid) = Uuid::parse_str(identifier) {
        if let Some(entry) = reg.get(uuid) {
            return Ok(ResolvedAgent::Live(entry));
        }
        if let Some(tombstone) = reg.tombstone(uuid) {
            return Ok(ResolvedAgent::Reclaimed(tombstone));
        }
        return Err(ToolError::ExecutionFailed {
            reason: format!(
                "agent id '{identifier}' is not registered and has no completion \
                 record — no agent with this id has run in this session"
            ),
        });
    }
    Err(ToolError::ExecutionFailed {
        reason: format!(
            "could not resolve agent '{identifier}' by path or UUID: no agent with \
             this identifier is registered or has completed in this session"
        ),
    })
}

/// Narrow a child's tool allow-list for its granted
/// [`MessagingScope`](crate::agent::child_policy::MessagingScope).
///
/// [`MessagingScope::None`](crate::agent::child_policy::MessagingScope::None)
/// removes `signal_agent` from the child's surface at spawn/fork time (the
/// tool also refuses at execute as defense-in-depth). An explicit
/// allow-list is filtered in place; an absent allow-list ("every parent
/// tool") is materialized from the registry minus `signal_agent`, so the
/// child's tool definitions and its [`SubAgentExecutor`] gate agree. The
/// result is always an explicit allow-list.
pub(super) fn strip_signal_agent_from_allow_list(
    allow_list: Option<Vec<String>>,
    registry: &ToolRegistry,
) -> Vec<String> {
    let names = match allow_list {
        Some(list) => list,
        None => registry.names().map(str::to_owned).collect(),
    };
    names
        .into_iter()
        .filter(|name| name != crate::tools::agent::coord::SIGNAL_AGENT_TOOL_NAME)
        .collect()
}

/// Tool executor handed to a spawned or forked sub-agent.
///
/// Wraps the parent's `Arc<ToolRegistry>` together with an optional
/// per-sub-agent allow-list and a child-specific [`ToolContext`].
/// Dispatches that target a name outside the allow-list surface as
/// [`ToolError::ToolNotFound`], matching the availability behaviour of
/// `ToolRegistry::set_available` without mutating the shared registry
/// (which is forbidden by the N-018 boundary).
///
/// Unlike a plain delegation to [`ToolRegistry::execute`] — which would
/// dispatch every tool against the *parent's* shared context, leaking the
/// parent's identity to the child — this executor replays the full
/// four-phase lifecycle (pre-validate, execute, post-validate, on-success)
/// against [`Self::child_context`]. Tools that read
/// [`AgentToolInfra`](crate::tools::agent::AgentToolInfra) therefore see the
/// child's `agent_id` / `parent_id`, not the spawning agent's.
pub struct SubAgentExecutor {
    registry: Arc<ToolRegistry>,
    available: Option<HashSet<String>>,
    child_context: Arc<ToolContext>,
}

impl SubAgentExecutor {
    /// Construct an executor that dispatches through `registry` against
    /// `child_context`. When `allow_list` is `Some`, only the named tools
    /// are reachable; every other name returns [`ToolError::ToolNotFound`].
    #[must_use]
    pub fn new(
        registry: Arc<ToolRegistry>,
        allow_list: Option<Vec<String>>,
        child_context: Arc<ToolContext>,
    ) -> Self {
        Self {
            registry,
            available: allow_list.map(|names| names.into_iter().collect()),
            child_context,
        }
    }
}

#[async_trait]
impl ToolExecutor for SubAgentExecutor {
    /// Exposes the child's [`ToolContext`] so the agent loop publishes
    /// cross-cutting state (e.g. the diagnostic collector) onto the
    /// *child's* context rather than the parent's.
    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.child_context))
    }

    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        if let Some(allowed) = self.available.as_ref()
            && !allowed.contains(name)
        {
            return Err(ToolError::ToolNotFound {
                name: name.to_string(),
            });
        }

        // Look up the trait object from the parent registry, but dispatch
        // it through the child's context — never via `registry.execute`,
        // which would use the parent's shared context.
        let tool = self
            .registry
            .get(name)
            .ok_or_else(|| ToolError::ToolNotFound {
                name: name.to_string(),
            })?;

        let split = split_envelope_fields(arguments);
        let envelope = ToolEnvelope {
            tool_call_id: call_id.to_owned(),
            tool_name: name.to_string(),
            model_args: split.tool_args,
            runtime_inputs: RuntimeInputs::default(),
            metadata: split.metadata,
        };
        let ctx = self.child_context.as_ref();

        if let PreValidateOutcome::Block(decision) = tool.pre_validate(&envelope, ctx).await {
            return Err(ToolError::from(decision));
        }
        for check in &ctx.pre_checks {
            if let PreValidateOutcome::Block(decision) = check.check(&envelope, ctx).await {
                return Err(ToolError::from(decision));
            }
        }

        // Stamp execution duration exactly like `ToolRegistry`'s dispatch
        // path: the executor measures so individual tools never time
        // themselves, and the stamped value is visible to post-validate /
        // on-success phases below.
        let dispatch_started = std::time::Instant::now();
        let mut output = tool.execute(&envelope, ctx).await?;
        output.duration = dispatch_started.elapsed();

        let mut errors: Vec<String> = Vec::new();
        let mut advisories: Vec<Advisory> = Vec::new();
        if let PostValidateOutcome::Fail { errors: errs } = tool.post_validate(&output, ctx).await {
            errors.extend(errs);
        }
        for check in &ctx.post_checks {
            let PostCheckResult {
                outcome,
                advisories: check_advisories,
            } = check.check(&output, ctx).await;
            advisories.extend(check_advisories);
            if let PostValidateOutcome::Fail { errors: errs } = outcome {
                errors.extend(errs);
            }
        }

        let (resolved_mode, override_record) =
            resolved_post_validate_mode(tool.post_validate_mode(), ctx);
        if let Some(ref over) = override_record {
            append_check_override(&mut output.content, over);
        }
        append_advisories(&mut output.content, &advisories);

        if !errors.is_empty() && resolved_mode == PostValidateMode::Gate {
            return Err(ToolError::PostValidationFailed {
                reason: errors.join("; "),
                committed_output: Some(output.content.clone()),
            });
        }

        tool.on_success(&output, ctx).await;
        for action in &ctx.on_success_actions {
            action.run(&output, ctx).await;
        }
        Ok(output.content)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {

    use parking_lot::Mutex;

    use super::*;
    use crate::provider::mock::MockProvider;
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::{Tool, ToolOutput};

    /// Stub tool that records the [`AgentToolInfra::agent_id`] it observes
    /// during `pre_validate`, so a test can prove the four-phase lifecycle
    /// ran against the child's context rather than the parent's.
    struct IdentityProbe {
        seen: Arc<Mutex<Option<Uuid>>>,
    }

    #[async_trait]
    impl Tool for IdentityProbe {
        fn name(&self) -> &'static str {
            "identity_probe"
        }
        fn description(&self) -> &'static str {
            "records the agent identity it sees"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn pre_validate(
            &self,
            _envelope: &ToolEnvelope,
            ctx: &ToolContext,
        ) -> PreValidateOutcome {
            if let Some(infra) = ctx.get_extension::<AgentToolInfra>() {
                *self.seen.lock() = Some(infra.agent_id);
            }
            PreValidateOutcome::Proceed
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    fn infra_for(
        agent_id: Uuid,
        parent_id: Option<Uuid>,
        registry: Option<Arc<ToolRegistry>>,
    ) -> Arc<AgentToolInfra> {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        Arc::new(AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id,
            grant: None,
            tool_registry: registry,
        })
    }

    /// R2: `SubAgentExecutor::execute` replays the full four-phase lifecycle
    /// against the *child's* `ToolContext`. The probe tool's `pre_validate`
    /// must observe the child's `agent_id`, the parent's own context must
    /// remain intact and distinct, and `shared_context()` must expose the
    /// child context for collector publishing.
    #[tokio::test]
    async fn sub_agent_executor_dispatches_against_child_context() {
        let parent_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        let seen = Arc::new(Mutex::new(None));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(IdentityProbe {
            seen: Arc::clone(&seen),
        }));
        let registry = Arc::new(registry);

        let parent_ctx = registry
            .shared_context()
            .expect("registry exposes a shared context");
        parent_ctx.insert_extension(infra_for(parent_id, None, Some(Arc::clone(&registry))));

        let child_ctx = Arc::new(ToolContext::empty());
        child_ctx.insert_extension(infra_for(
            child_id,
            Some(parent_id),
            Some(Arc::clone(&registry)),
        ));

        let executor = SubAgentExecutor::new(Arc::clone(&registry), None, Arc::clone(&child_ctx));
        let out = executor
            .execute("identity_probe", "test-call", serde_json::json!({}))
            .await
            .expect("probe dispatch succeeds");
        assert_eq!(out["ok"], true);

        assert_eq!(
            *seen.lock(),
            Some(child_id),
            "pre_validate must run against the child context",
        );

        let parent_infra = parent_ctx
            .get_extension::<AgentToolInfra>()
            .expect("parent infra intact");
        assert_eq!(parent_infra.agent_id, parent_id);
        assert_ne!(
            parent_infra.agent_id, child_id,
            "parent and child identities must stay distinct",
        );

        let shared = executor
            .shared_context()
            .expect("executor exposes the child context");
        let shared_infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("child infra reachable from shared context");
        assert_eq!(shared_infra.agent_id, child_id);
    }

    /// Stub tool whose `on_success` records the stamped execution
    /// duration, proving the executor measures dispatch like
    /// `ToolRegistry` does.
    struct DurationProbe {
        seen_duration: Arc<Mutex<Option<std::time::Duration>>>,
    }

    #[async_trait]
    impl Tool for DurationProbe {
        fn name(&self) -> &'static str {
            "duration_probe"
        }
        fn description(&self) -> &'static str {
            "records the stamped duration it sees in on_success"
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
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok(ToolOutput::success(serde_json::json!({"ok": true})))
        }
        async fn on_success(&self, output: &ToolOutput, _ctx: &ToolContext) {
            *self.seen_duration.lock() = Some(output.duration);
        }
    }

    /// Duration parity with the registry dispatch path: the child
    /// executor stamps execution duration on the output before the
    /// on-success phase, so lifecycle hooks observe a real measurement
    /// rather than the `Duration::ZERO` default.
    #[tokio::test]
    async fn sub_agent_executor_stamps_execution_duration() {
        let seen_duration = Arc::new(Mutex::new(None));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DurationProbe {
            seen_duration: Arc::clone(&seen_duration),
        }));
        let registry = Arc::new(registry);

        let child_ctx = Arc::new(ToolContext::empty());
        let executor = SubAgentExecutor::new(Arc::clone(&registry), None, child_ctx);
        executor
            .execute("duration_probe", "test-call", serde_json::json!({}))
            .await
            .expect("probe dispatch succeeds");

        let stamped = seen_duration
            .lock()
            .expect("on_success must run and observe the output");
        assert!(
            stamped > std::time::Duration::ZERO,
            "execution duration must be stamped before on_success, got {stamped:?}",
        );
    }

    /// R2: a name outside the per-child allow-list surfaces as
    /// [`ToolError::ToolNotFound`] before any lifecycle phase runs.
    #[tokio::test]
    async fn sub_agent_executor_allow_list_gates_disallowed() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(IdentityProbe {
            seen: Arc::new(Mutex::new(None)),
        }));
        let registry = Arc::new(registry);

        let child_ctx = Arc::new(ToolContext::empty());
        child_ctx.insert_extension(infra_for(Uuid::new_v4(), None, Some(Arc::clone(&registry))));

        let executor = SubAgentExecutor::new(
            Arc::clone(&registry),
            Some(vec!["something_else".to_owned()]),
            child_ctx,
        );
        let err = executor
            .execute("identity_probe", "test-call", serde_json::json!({}))
            .await
            .expect_err("disallowed tool must error");
        assert!(matches!(err, ToolError::ToolNotFound { .. }));
    }
}
