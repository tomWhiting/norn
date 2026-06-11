//! Per-child [`ToolContext`] construction for
//! [`SpawnAgentTool`](super::spawn::SpawnAgentTool).
//!
//! Split from [`super::spawn`] so each file stays inside the per-file
//! 500-line production-code limit; the launch/lifecycle machinery stays
//! in `spawn.rs` while the context-forwarding rules live here.

use std::sync::Arc;

use uuid::Uuid;

use super::handle::{AgentHandles, SharedSessionTree};
use super::infra::AgentToolInfra;
use crate::config::permissions::PermissionPolicy;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
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
/// [`crate::agent::mailbox::Mailbox`] is shared by design, so a child's
/// send to its `parent_id` routes back to the same mailbox.
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
pub(super) fn build_child_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_tree: Option<SharedSessionTree>,
) -> Arc<ToolContext> {
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        mailbox: Arc::clone(&parent_infra.mailbox),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
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
    if let Some(tree) = child_tree {
        child_ctx.insert_extension(Arc::new(tree));
    }
    Arc::new(child_ctx)
}
