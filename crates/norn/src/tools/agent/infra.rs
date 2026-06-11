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

use crate::agent::mailbox::Mailbox;
use crate::agent::registry::AgentRegistry;
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
    /// Mailbox routing inter-agent messages.
    pub mailbox: Arc<Mailbox>,
    /// Provider used for sub-agent and fork model calls.
    pub provider: Arc<dyn Provider>,
    /// Parent agent's session event store.
    pub event_store: Arc<EventStore>,
    /// Calling agent's id (the sender for outbound messages, parent for spawn).
    pub agent_id: Uuid,
    /// Calling agent's parent id, if any (for genealogy).
    pub parent_id: Option<Uuid>,
    /// Shared tool registry handed to spawned and forked sub-agents. When
    /// `None`, spawn and fork report a configuration error — they refuse to
    /// silently launch a sub-agent that has no tools available. The registry
    /// itself is never mutated through this handle; callers wrap it in a
    /// [`SubAgentExecutor`] with an optional per-spawn allow-list instead.
    pub tool_registry: Option<Arc<ToolRegistry>>,
}

/// Fetches the configured [`AgentToolInfra`] from the context, returning
/// a typed [`ToolError::MissingExtension`] naming the missing type when
/// none is present.
pub(super) fn infra_from(ctx: &ToolContext) -> Result<Arc<AgentToolInfra>, ToolError> {
    ctx.require_extension::<AgentToolInfra>()
}

/// Resolves a string identifier (path or raw UUID) to a registered agent.
pub(super) fn resolve_agent_id(
    registry: &Arc<RwLock<AgentRegistry>>,
    identifier: &str,
) -> Result<Uuid, ToolError> {
    if let Some(entry) = registry.read().get_by_path(identifier) {
        return Ok(entry.id);
    }
    if let Ok(uuid) = Uuid::parse_str(identifier) {
        if registry.read().get(uuid).is_some() {
            return Ok(uuid);
        }
        return Err(ToolError::ExecutionFailed {
            reason: format!("agent id '{identifier}' not registered"),
        });
    }
    Err(ToolError::ExecutionFailed {
        reason: format!("could not resolve agent '{identifier}' by path or UUID"),
    })
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
            mailbox: Arc::new(Mailbox::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id,
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
