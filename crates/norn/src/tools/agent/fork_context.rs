//! Per-fork [`ToolContext`](crate::tool::context::ToolContext)
//! construction for [`crate::tools::agent::fork_tool::ForkTool`] (R3).
//!
//! The fork's child store and [`SessionBinding`] are minted through the
//! parent's
//! [`SessionBinding::branch_child`](crate::session::SessionBinding::branch_child)
//! in [`crate::tools::agent::fork_tool::ForkTool::execute`]; this module
//! forwards the parent context's shared infrastructure onto the fork's
//! own context. The `tokio::spawn` launch / completion wrapper lives in
//! [`super::fork_launch`]; the outcome projection lives in
//! [`super::fork_outcome`]; the child-store seeding step (R2) lives in
//! [`super::fork_seed`]. Split from the former `fork_pipeline.rs` per
//! the child-persistence design ruling D-b (context and outcome are
//! orthogonal clusters).

use std::sync::Arc;

use uuid::Uuid;

use super::handle::{AgentHandles, AgentWakeRegistry};
use super::infra::{AgentCancellation, AgentToolInfra, ParentGrant};
use super::spawn_context::{forward_diagnostic_infra, wire_child_action_log};
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::fork::ParentSystemInstruction;
use crate::config::permissions::PermissionPolicy;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
use crate::session::SessionBinding;
use crate::session::store::EventStore;
use crate::tool::catalog::SharedToolCatalog;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::scheduling::ToolEffectIndex;
use crate::tools::task::SharedTaskStore;

/// Construct the per-fork [`ToolContext`](crate::tool::context::ToolContext) (R3).
///
/// Fresh [`AgentToolInfra`] carrying the child's own `agent_id` / `parent_id`,
/// its own [`EventStore`], and its own [`SessionBinding`] (minted by the
/// parent's `branch_child`), plus a fresh [`AgentHandles`] so the fork can
/// spawn grandchildren in turn. Shared infrastructure is forwarded from the
/// parent context so tasks, tool discovery, and the parent's base system
/// instruction stay reachable from inside the fork.
///
/// The consent-boundary [`PermissionPolicy`] and the scheduling
/// [`ToolEffectIndex`] are likewise forwarded: the fork's agent loop
/// resolves both from *its own* executor's shared context, so omitting
/// them here would let a fork evade every deny/ask rule the parent is
/// subject to (and lose effect-based batch scheduling).
///
/// The parent's workspace-confinement root (a plain [`ToolContext`] field,
/// not an extension) is forwarded via
/// [`ToolContext::confine_to_workspace`] for the same reason: the fork's
/// file tools check confinement against the *fork's* dispatch context, so
/// dropping the root would let a confined parent escape its sandbox simply
/// by forking. The fork's working dir is its **own** [`SharedWorkingDir`]
/// handle seeded from the parent's *current* working dir — snapshot
/// semantics, matching [`SharedWorkingDir`]'s documented fork contract:
/// forks run concurrently with the parent, so sharing the live handle
/// would let a fork's bash `cd` move the parent's (and every sibling's)
/// working dir mid-turn.
///
/// The parent's shared
/// [`HookRegistry`](crate::integration::hooks::HookRegistry) extension is
/// forwarded so the fork's own spawn/fork sites (grandchildren) observe
/// the same operator hooks; [`ForkTool::execute`] separately installs the
/// registry on the fork's `LoopContext` so pre/post-tool hooks fire for
/// the fork's own calls.
///
/// `child_policy` is the [`ChildPolicy`] the parent grants this fork —
/// computed by the fork tool from the parent's own grant (narrowed or
/// inherit-with-decrement, W3.4): it is stamped on the fork's
/// [`AgentToolInfra`] together with the parent's event store, so
/// `signal_agent` enforces the granted messaging scope, the dual-store
/// `Sent` audit writes from ground truth, and the fork's own spawn/fork
/// sites read *their* budget from the grant. The parent's
/// [`CoordinationEnvelope`] extension is forwarded for the envelope-wide
/// `child_result_capacity`; the
/// [`ReclaimOnResultDelivery`](super::reclaim::ReclaimOnResultDelivery)
/// marker is forwarded so the fork's own children are reclaimed at every
/// level exactly as depth-1 children are.
///
/// `child_cancel` is the fork's own run-cancellation token — created by
/// the fork tool as a child of the forker's published
/// [`AgentCancellation`] (or free-standing when the forker publishes
/// none; see [`AgentCancellation`] for the root boundary) — published on
/// the fork's context here so the fork's own spawn/fork sites chain
/// grandchild tokens under it (W3.5 cancellation cascade).
///
/// [`ForkTool::execute`]: crate::tools::agent::fork_tool::ForkTool
pub(super) fn build_fork_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_session: Arc<SessionBinding>,
    child_policy: ChildPolicy,
    child_cancel: tokio_util::sync::CancellationToken,
) -> Arc<ToolContext> {
    let child_log_store = Arc::clone(&child_store);
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        router: Arc::clone(&parent_infra.router),
        pending_messages: Arc::clone(&parent_infra.pending_messages),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
        grant: Some(ParentGrant {
            policy: child_policy,
            parent_store: Arc::clone(&parent_infra.event_store),
        }),
        tool_registry: parent_infra.tool_registry.as_ref().map(Arc::clone),
        // The fork's own branching identity: grandchild mints route
        // through this binding, so depth recursion is structural.
        session: child_session,
    };

    let mut child_ctx =
        ToolContext::with_working_dir(SharedWorkingDir::new(parent_ctx.working_dir()));
    if let Some(root) = parent_ctx.workspace_root() {
        child_ctx.confine_to_workspace(root.to_path_buf());
        // Inherit the parent's read carve-out (already canonicalized) so a
        // confined fork can READ the same operator-configured skill /
        // profile / config dirs the parent could (DECISIONS §0.6(b)).
        let exempt = parent_ctx.read_exempt_roots().to_vec();
        if !exempt.is_empty() {
            child_ctx.set_read_exempt_roots(exempt);
        }
    }
    child_ctx.insert_extension(Arc::new(child_infra));
    child_ctx.insert_extension(Arc::new(AgentCancellation(child_cancel)));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    if let Some(wake_registry) = parent_ctx.get_extension::<AgentWakeRegistry>() {
        child_ctx.insert_extension(wake_registry);
    }
    if let Some(task_store) = parent_ctx.get_extension::<SharedTaskStore>() {
        child_ctx.insert_extension(task_store);
    }
    if let Some(catalog) = parent_ctx.get_extension::<SharedToolCatalog>() {
        child_ctx.insert_extension(catalog);
    }
    // Skill infrastructure: the fork shares the parent's registry, so the
    // `skill` tool is offered to it. Without forwarding both the search
    // paths and the catalog the tool would always fail `MissingExtension`
    // at execute — offered but unusable. Forwarded as `Arc` clones like the
    // other shared infrastructure above.
    if let Some(skill_paths) = parent_ctx.get_extension::<crate::tools::skill::SkillSearchPaths>() {
        child_ctx.insert_extension(skill_paths);
    }
    if let Some(skill_catalog) = parent_ctx.get_extension::<crate::skill::SkillCatalog>() {
        child_ctx.insert_extension(skill_catalog);
    }
    if let Some(diagnostics) = parent_ctx.get_extension::<DiagnosticCollector>() {
        child_ctx.insert_extension(diagnostics);
    }
    forward_diagnostic_infra(parent_ctx, &mut child_ctx);
    if let Some(sp) = parent_ctx.get_extension::<SharedProvider>() {
        child_ctx.insert_extension(sp);
    }
    if let Some(parent_base) = parent_ctx.get_extension::<ParentSystemInstruction>() {
        child_ctx.insert_extension(parent_base);
    }
    if let Some(policy) = parent_ctx.get_extension::<PermissionPolicy>() {
        child_ctx.insert_extension(policy);
    }
    if let Some(effects) = parent_ctx.get_extension::<ToolEffectIndex>() {
        child_ctx.insert_extension(effects);
    }
    if let Some(hooks) = parent_ctx.get_extension::<HookRegistry>() {
        child_ctx.insert_extension(hooks);
    }
    if let Some(ch) =
        parent_ctx.get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
    {
        child_ctx.insert_extension(ch);
    }
    if let Some(envelope) = parent_ctx.get_extension::<CoordinationEnvelope>() {
        child_ctx.insert_extension(envelope);
    }
    if let Some(marker) = parent_ctx.get_extension::<super::reclaim::ReclaimOnResultDelivery>() {
        child_ctx.insert_extension(marker);
    }
    // Per-agent action log + session log-tree registration: the fork's
    // log starts empty at the fork point (its seeded conversation is its
    // memory; its action log records what *it* did). See
    // [`wire_child_action_log`].
    wire_child_action_log(
        parent_infra,
        parent_ctx,
        child_id,
        child_log_store,
        &child_ctx,
    );
    Arc::new(child_ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::registry::AgentRegistry;
    use crate::provider::traits::Provider;
    use crate::tool::registry::ToolRegistry;
    use crate::tools::diagnostics::{DiagnosticInfra, build_diagnostic_infra};
    use tempfile::tempdir;

    /// Documented-proposal policy used by tests — a deliberate test-caller
    /// choice, never a library default.
    fn test_policy() -> ChildPolicy {
        use crate::agent::child_policy::{DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        }
    }

    /// DECISIONS §0.6(b): a fork inherits the parent's read carve-out, so
    /// under inherited confinement it can READ a file inside a
    /// parent-declared exempt dir outside the workspace root — end-to-end
    /// through the read tool (twin of the spawn-side
    /// `child_inherits_read_exemption_and_reads_exempt_file`; deleting the
    /// fork-side inheritance block must fail this test).
    #[tokio::test]
    async fn fork_inherits_read_exemption_and_reads_exempt_file() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;
        use crate::tool::context::SharedWorkingDir;
        use crate::tool::envelope::ToolEnvelope;
        use crate::tool::traits::Tool;
        use crate::tools::read::ReadTool;

        let outer = tempdir().map_err(|e| format!("outer tempdir: {e}"))?;
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        std::fs::create_dir(&root).map_err(|e| format!("mkdir ws: {e}"))?;
        std::fs::create_dir(&skills).map_err(|e| format!("mkdir skills: {e}"))?;
        let companion = skills.join("SKILL.md");
        std::fs::write(&companion, "name: demo\n").map_err(|e| format!("write companion: {e}"))?;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };

        // Confined parent carrying the exempt root.
        let mut parent_ctx = ToolContext::with_working_dir(SharedWorkingDir::new(root.clone()));
        parent_ctx.confine_to_workspace(root.clone());
        parent_ctx.set_read_exempt_roots(vec![skills.clone()]);

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        assert_eq!(
            child_ctx.read_exempt_roots(),
            parent_ctx.read_exempt_roots(),
            "the fork must inherit the parent's canonicalized exempt roots",
        );

        // End-to-end: the fork reads the exempt companion despite
        // confinement to `root`.
        let tool = ReadTool::new();
        let env = ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "read".to_owned(),
            model_args: serde_json::json!({ "path": companion.to_string_lossy() }),
            metadata: serde_json::Value::Null,
        };
        let out = tool
            .execute(&env, &child_ctx)
            .await
            .map_err(|e| format!("read output: {e}"))?;
        assert!(
            !out.is_error(),
            "fork must read the inherited-exempt file: {:?}",
            out.content
        );

        // A non-exempt outside path stays refused for the fork.
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "s").map_err(|e| format!("write secret: {e}"))?;
        let refused_env = ToolEnvelope {
            tool_call_id: "call-2".to_owned(),
            tool_name: "read".to_owned(),
            model_args: serde_json::json!({ "path": secret.to_string_lossy() }),
            metadata: serde_json::Value::Null,
        };
        let refused = tool
            .execute(&refused_env, &child_ctx)
            .await
            .map_err(|e| format!("refusal output: {e}"))?;
        assert!(
            refused.is_error(),
            "non-exempt outside path must be refused for the fork"
        );
        assert_eq!(refused.content["kind"], "confinement_refused");
        Ok(())
    }

    /// Permission-escape regression (blocker): the consent-boundary
    /// [`PermissionPolicy`] and the scheduling [`ToolEffectIndex`] must
    /// be forwarded from the parent's context into the fork's context —
    /// the fork loop resolves both from its own executor's shared
    /// context, so a missing forward disables enforcement entirely.
    #[tokio::test]
    async fn fork_context_forwards_permission_policy_and_effect_index() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let parent_ctx = ToolContext::empty();
        let policy = Arc::new(PermissionPolicy::from_patterns(&["bash"], &[], &[]));
        let effects = Arc::new(ToolEffectIndex::new());
        parent_ctx.insert_extension(Arc::clone(&policy));
        parent_ctx.insert_extension(Arc::clone(&effects));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded_policy = child_ctx
            .get_extension::<PermissionPolicy>()
            .ok_or("PermissionPolicy must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded_policy, &policy) {
            return Err("the fork must share the parent's policy instance".to_owned());
        }
        let forwarded_effects = child_ctx
            .get_extension::<ToolEffectIndex>()
            .ok_or("ToolEffectIndex must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded_effects, &effects) {
            return Err("the fork must share the parent's effect index instance".to_owned());
        }
        Ok(())
    }

    /// Confinement-escape regression (blocker): `workspace_root` is a
    /// plain field on [`ToolContext`] — not an extension — so
    /// `build_fork_context` must forward it explicitly, and the fork's
    /// working dir must be seeded from the parent's *current* working
    /// dir on the fork's own handle (snapshot semantics), never from
    /// the process CWD.
    #[test]
    fn fork_context_forwards_workspace_root_and_snapshots_working_dir() -> Result<(), String> {
        use std::path::{Path, PathBuf};

        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;
        use crate::tool::context::SharedWorkingDir;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let mut parent_ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from(
            "/tmp/fork-parent-wd",
        )));
        parent_ctx.confine_to_workspace(PathBuf::from("/tmp/fork-workspace-root"));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        if child_ctx.workspace_root() != Some(Path::new("/tmp/fork-workspace-root")) {
            return Err(format!(
                "the fork must carry the parent's confinement root, got {:?}",
                child_ctx.workspace_root(),
            ));
        }
        if child_ctx.working_dir().as_path() != Path::new("/tmp/fork-parent-wd") {
            return Err(format!(
                "the fork's working dir must be seeded from the parent's current dir, got {}",
                child_ctx.working_dir().display(),
            ));
        }

        // Snapshot semantics: the fork owns its handle, so a fork-side
        // `cd` must not move the parent's working dir.
        child_ctx.set_working_dir(PathBuf::from("/tmp/fork-child-moved"));
        if parent_ctx.working_dir().as_path() != Path::new("/tmp/fork-parent-wd") {
            return Err("fork working-dir mutations must not propagate to the parent".to_owned());
        }
        Ok(())
    }

    /// Hook-coverage regression: the parent's shared
    /// [`HookRegistry`](crate::integration::hooks::HookRegistry)
    /// extension must be forwarded to the fork's context so the fork's
    /// own spawn/fork sites (grandchildren) can reach it.
    #[test]
    fn fork_context_forwards_hook_registry_extension() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::integration::hooks::HookRegistry;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let parent_ctx = ToolContext::empty();
        let hooks = Arc::new(HookRegistry::new());
        parent_ctx.insert_extension(Arc::clone(&hooks));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded = child_ctx
            .get_extension::<HookRegistry>()
            .ok_or("HookRegistry must be forwarded to the fork context")?;
        if !Arc::ptr_eq(&forwarded, &hooks) {
            return Err("the fork must share the parent's hook registry instance".to_owned());
        }
        Ok(())
    }

    #[test]
    fn fork_context_forwards_diagnostic_infra_and_post_check() -> Result<(), String> {
        use crate::agent::message_router::MessageRouter;
        use crate::provider::mock::MockProvider;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let infra = AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id: Uuid::new_v4(),
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        };
        let dir = tempdir().map_err(|error| format!("temp dir: {error}"))?;
        let diagnostic_infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
        let parent_ctx = ToolContext::empty();
        parent_ctx.insert_extension(Arc::clone(&diagnostic_infra));

        let child_ctx = build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded = child_ctx
            .get_extension::<DiagnosticInfra>()
            .ok_or("fork must inherit DiagnosticInfra")?;
        if !Arc::ptr_eq(&forwarded, &diagnostic_infra) {
            return Err("fork must share the parent's diagnostic infrastructure".to_owned());
        }
        if child_ctx.post_checks.len() != 1 {
            return Err("fork must install the diagnostics post-check".to_owned());
        }
        Ok(())
    }
}
