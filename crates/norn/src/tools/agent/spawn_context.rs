//! Per-child [`ToolContext`] construction for
//! [`SpawnAgentTool`](super::spawn::SpawnAgentTool).
//!
//! Split from [`super::spawn`] so each file stays inside the per-file
//! 500-line production-code limit; the launch/lifecycle machinery stays
//! in `spawn.rs` while the context-forwarding rules live here.

use std::sync::Arc;

use uuid::Uuid;

use super::handle::{AgentHandles, SharedSessionTree};
use super::infra::{AgentToolInfra, ParentGrant};
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::config::permissions::PermissionPolicy;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
use crate::session::action_log::ActionLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::session::store::EventStore;
use crate::tool::catalog::SharedToolCatalog;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::scheduling::ToolEffectIndex;
use crate::tools::task::SharedTaskStore;

/// Construct the per-child [`ToolContext`].
///
/// The child gets a *fresh* [`AgentToolInfra`] carrying its own
/// `agent_id` / `parent_id` and its own [`EventStore`], plus a *fresh*
/// (empty) [`AgentHandles`] so it can spawn grandchildren. The shared
/// infrastructure — [`SharedTaskStore`], [`SharedToolCatalog`],
/// [`DiagnosticCollector`] — is forwarded from the parent context so tasks
/// and tool discovery stay global across the agent tree. The
/// [`crate::agent::message_router::MessageRouter`] is shared by design, so
/// a child's send to its `parent_id` routes through the same router.
///
/// The consent-boundary [`PermissionPolicy`] and the scheduling
/// [`ToolEffectIndex`] are likewise forwarded: the child's agent loop
/// resolves both from *its own* executor's shared context, so omitting
/// them here would let a child evade every deny/ask rule the parent is
/// subject to (and lose effect-based batch scheduling).
///
/// The parent's workspace-confinement root (a plain [`ToolContext`] field,
/// not an extension) is forwarded via
/// [`ToolContext::confine_to_workspace`] for the same reason: the child's
/// file tools check confinement against the *child's* dispatch context, so
/// dropping the root would let a confined parent escape its sandbox simply
/// by spawning a child. The child's working dir is its **own**
/// [`SharedWorkingDir`] handle seeded from the parent's *current* working
/// dir — snapshot semantics, matching [`SharedWorkingDir`]'s documented
/// fork contract: children run concurrently with the parent, so sharing
/// the live handle would let a child's bash `cd` move the parent's (and
/// every sibling's) working dir mid-turn.
///
/// The parent's shared [`HookRegistry`] extension is forwarded so the
/// child's own spawn/fork sites (grandchildren) observe the same operator
/// hooks; the caller separately installs the registry on the child's
/// [`LoopContext`](crate::r#loop::loop_context::LoopContext) so
/// pre/post-tool hooks fire for the child's own calls.
///
/// When `child_tree` is `Some` — i.e. an orchestrator published a
/// [`SharedSessionTree`] on the parent context — it is installed on the
/// child context keyed to the *child's* `SessionId`, so a grandchild spawn
/// branches under the child's session in turn (NA-008 R3).
///
/// `child_policy` is the [`ChildPolicy`] the parent grants this child
/// (read from the parent's [`CoordinationEnvelope`] by the spawn tool):
/// it is stamped on the child's [`AgentToolInfra`] together with the
/// parent's event store, so `send_message` enforces the granted
/// [`MessagingScope`](crate::agent::child_policy::MessagingScope) and
/// writes the dual-store `Sent` audit from ground truth carried on the
/// child's own context. The parent's [`CoordinationEnvelope`] extension is
/// forwarded so the child's own spawn sites can read policy at any depth
/// (the registry depth gate still rejects grandchildren until W3.4, which
/// also replaces the inherited-envelope read with the child's narrowed
/// policy).
pub(super) fn build_child_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
    child_policy: ChildPolicy,
) -> Arc<ToolContext> {
    let child_log_store = Arc::clone(&child_store);
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        router: Arc::clone(&parent_infra.router),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
        grant: Some(ParentGrant {
            policy: child_policy,
            parent_store: Arc::clone(&parent_infra.event_store),
        }),
        tool_registry: parent_infra.tool_registry.as_ref().map(Arc::clone),
    };

    let mut child_ctx =
        ToolContext::with_working_dir(SharedWorkingDir::new(parent_ctx.working_dir()));
    if let Some(root) = parent_ctx.workspace_root() {
        child_ctx.confine_to_workspace(root.to_path_buf());
    }
    child_ctx.insert_extension(Arc::new(child_infra));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    if let Some(task_store) = parent_ctx.get_extension::<SharedTaskStore>() {
        child_ctx.insert_extension(task_store);
    }
    if let Some(catalog) = parent_ctx.get_extension::<SharedToolCatalog>() {
        child_ctx.insert_extension(catalog);
    }
    if let Some(diagnostics) = parent_ctx.get_extension::<DiagnosticCollector>() {
        child_ctx.insert_extension(diagnostics);
    }
    if let Some(sp) = parent_ctx.get_extension::<SharedProvider>() {
        child_ctx.insert_extension(sp);
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
    if let Some(tree) = child_tree {
        child_ctx.insert_extension(Arc::new(tree));
    }
    wire_child_action_log(
        parent_infra,
        parent_ctx,
        child_id,
        child_log_store,
        &child_ctx,
    );
    Arc::new(child_ctx)
}

/// Give a spawn/fork child its own per-agent [`ActionLog`] and register it
/// in the session-wide [`ActionLogTree`].
///
/// The child's log is built over the **child's** event store and the
/// **child's** [`SharedWorkingDir`] handle (so its mutation ledger
/// resolves relative paths against the child's live working dir), then
/// inserted on the child context — fixing the inherited-tool /
/// missing-extension failure where a child's `action_log` calls errored
/// with `MissingExtension`. A fork's log starts empty at the fork point:
/// its seeded conversation is its memory; its action log records what
/// *it* did.
///
/// The [`ActionLogTree`] is fetched from the parent context and forwarded
/// to the child, so the child's own spawn/fork sites register
/// grandchildren into the same tree (and the child can federate over its
/// own subtree — never upward). When the parent context carries no tree —
/// a runtime assembled outside `AgentBuilder`, e.g. `norn-cli`'s
/// `build_runtime` — the tree is installed on the parent now, rooted at
/// the parent agent, with the parent's own log registered when one is
/// published. Spawn and fork are `Process`-effect tools and therefore run
/// serialized within the parent's dispatch loop, so this get-or-install
/// step never races with itself.
pub(super) fn wire_child_action_log(
    parent_infra: &AgentToolInfra,
    parent_ctx: &ToolContext,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    child_ctx: &ToolContext,
) {
    let child_log = Arc::new(ActionLog::with_working_dir(
        child_store,
        child_ctx.shared_working_dir(),
    ));
    child_ctx.insert_extension(Arc::clone(&child_log));

    let log_tree = parent_ctx
        .get_extension::<ActionLogTree>()
        .unwrap_or_else(|| {
            let tree = Arc::new(ActionLogTree::new(parent_infra.agent_id));
            if let Some(parent_log) = parent_ctx.get_extension::<ActionLog>() {
                tree.register(parent_infra.agent_id, None, parent_log);
            }
            parent_ctx.insert_extension(Arc::clone(&tree));
            tree
        });
    log_tree.register(child_id, Some(parent_infra.agent_id), child_log);
    child_ctx.insert_extension(log_tree);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentRegistry;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::tool::registry::ToolRegistry;

    fn parent_infra(agent_id: Uuid) -> AgentToolInfra {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
        }
    }

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
        }
    }

    /// The child context carries its own [`ActionLog`] (the production
    /// regression: children previously had none and every `action_log`
    /// call failed `MissingExtension`), registered in the shared
    /// [`ActionLogTree`] under the parent, with the tree forwarded to the
    /// child.
    #[test]
    fn build_child_context_installs_child_log_and_registers_in_tree() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();
        let parent_log = Arc::new(crate::session::action_log::ActionLog::new(Arc::new(
            EventStore::new(),
        )));
        parent_ctx.insert_extension(Arc::clone(&parent_log));

        let child_id = Uuid::new_v4();
        let child_ctx = build_child_context(
            &infra,
            child_id,
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );

        let child_log = child_ctx
            .get_extension::<crate::session::action_log::ActionLog>()
            .expect("the child must carry its own ActionLog extension");
        assert!(
            !Arc::ptr_eq(&child_log, &parent_log),
            "the child's log is per-agent, never the parent's instance",
        );

        // The tree was lazily installed on the parent, rooted at the
        // parent, with both logs registered and the parent→child edge.
        let tree = parent_ctx
            .get_extension::<ActionLogTree>()
            .expect("tree installed on the parent context");
        assert_eq!(tree.root(), parent_id);
        assert!(Arc::ptr_eq(
            &tree.log_of(parent_id).expect("root log"),
            &parent_log
        ));
        assert!(Arc::ptr_eq(
            &tree.log_of(child_id).expect("child log"),
            &child_log
        ));
        assert_eq!(tree.children_of(parent_id), vec![child_id]);

        // Forwarded: the child shares the same tree instance, so
        // grandchildren register into the same session-wide tree.
        let child_tree = child_ctx
            .get_extension::<ActionLogTree>()
            .expect("tree forwarded to the child context");
        assert!(Arc::ptr_eq(&child_tree, &tree));
    }

    /// A second child reuses the already-installed tree — both children
    /// hang off the same root.
    #[test]
    fn second_child_registers_into_the_same_tree() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();
        parent_ctx.insert_extension(Arc::new(crate::session::action_log::ActionLog::new(
            Arc::new(EventStore::new()),
        )));

        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let _c1 = build_child_context(
            &infra,
            first,
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );
        let tree_after_first = parent_ctx.get_extension::<ActionLogTree>().expect("tree");
        let _c2 = build_child_context(
            &infra,
            second,
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );
        let tree_after_second = parent_ctx.get_extension::<ActionLogTree>().expect("tree");

        assert!(
            Arc::ptr_eq(&tree_after_first, &tree_after_second),
            "the second child must reuse the installed tree, not replace it",
        );
        assert_eq!(
            tree_after_second.children_of(parent_id),
            vec![first, second]
        );
    }

    /// A parent context with no [`ActionLog`] of its own (assembled
    /// outside `AgentBuilder`) still anchors the tree at the parent: the
    /// child registers and is reachable; the root simply has no log.
    #[test]
    fn child_registers_even_when_parent_has_no_log() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();

        let child_id = Uuid::new_v4();
        let _child_ctx = build_child_context(
            &infra,
            child_id,
            Arc::new(EventStore::new()),
            &parent_ctx,
            None,
            test_policy(),
        );

        let tree = parent_ctx.get_extension::<ActionLogTree>().expect("tree");
        assert_eq!(tree.root(), parent_id);
        assert!(
            tree.log_of(parent_id).is_none(),
            "no parent log to register"
        );
        assert!(tree.log_of(child_id).is_some(), "child log registered");
        assert_eq!(tree.children_of(parent_id), vec![child_id]);
    }
}
