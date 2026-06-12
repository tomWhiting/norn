//! Shared delegation plumbing for the spawn/fork launch paths (W3.4):
//! spawner-policy resolution, per-spawn grant computation, hierarchical
//! auto-path generation, and the per-agent child-result channel.
//!
//! One implementation serves both tools so the budget a spawn enforces
//! and the budget a fork enforces can never drift; the registry
//! re-validates the same invariants from ground truth in
//! [`AgentRegistry::reserve`](crate::agent::registry::AgentRegistry::reserve).

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use super::infra::AgentToolInfra;
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::error::ToolError;
use crate::tool::context::ToolContext;

/// Resolve the spawning agent's **own** granted [`ChildPolicy`].
///
/// Children and forks carry the harness-stamped grant on their
/// [`AgentToolInfra`]; a root agent has no granting parent, so its policy
/// is the builder envelope's `child_policy` — the root's own budget
/// (W3.4: "root uses the builder envelope's policy"). The value is never
/// model-controlled.
pub(super) fn resolve_spawner_policy(
    infra: &AgentToolInfra,
    envelope: &CoordinationEnvelope,
) -> ChildPolicy {
    infra.grant.as_ref().map_or_else(
        || envelope.child_policy.clone(),
        |grant| grant.policy.clone(),
    )
}

/// Compute the [`ChildPolicy`] granted to the child being launched:
/// the optional model-supplied `child_policy` argument validated as a
/// strict narrowing of the spawner's own grant, or the design-specified
/// default derivation (inherit-with-decrement) when omitted — see
/// [`ChildPolicy::grant_for_child`].
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] carrying the typed
/// [`PolicyNarrowingError`](crate::agent::child_policy::PolicyNarrowingError)
/// text — depth exhaustion or the specific widening violation, each
/// naming the caller's own budget. `surface` is the tool name
/// (`"spawn_agent"` / `"fork"`) for message attribution.
pub(super) fn grant_child_policy(
    spawner_policy: &ChildPolicy,
    requested: Option<ChildPolicy>,
    surface: &str,
) -> Result<ChildPolicy, ToolError> {
    spawner_policy
        .grant_for_child(requested)
        .map_err(|violation| ToolError::ExecutionFailed {
            reason: format!("{surface}: {violation}"),
        })
}

/// Generate the auto path for a child, namespaced under the **spawning
/// agent's** registry path so the agents tree reads as a real tree at
/// every depth (W3.4 path namespacing): `{spawner_path}/{kind}/{uuid}`.
///
/// A spawner with no registry entry (an unregistered root) has no path
/// prefix, so its children land at `/{kind}/{uuid}` — exactly the
/// pre-recursion shape.
pub(crate) fn auto_child_path(
    registry: &Arc<RwLock<AgentRegistry>>,
    spawner_id: Uuid,
    kind: &str,
) -> String {
    let prefix = registry
        .read()
        .get(spawner_id)
        .map(|entry| entry.path)
        .unwrap_or_default();
    format!("{prefix}/{kind}/{}", Uuid::new_v4())
}

/// Give a child that can itself delegate its own child-result channel.
///
/// Created iff the child's granted `delegation.remaining_depth >= 1` — a
/// leaf cannot spawn, so it gets no channel (and its spawn/fork calls are
/// refused by the registry budget anyway). The sender is installed as an
/// extension on the **child's** [`ToolContext`] (exactly what
/// `install_agent_infra` does for the root), so the child's own
/// spawn/fork sites deliver grandchild results to *the child*; the
/// returned receiver must be wired onto the child's
/// [`LoopContext::child_result_rx`](crate::r#loop::loop_context::LoopContext::child_result_rx)
/// so its loop drains them at the existing step boundaries. Results
/// bubble exactly one hop per level — never skipping levels (Wave 3
/// §"Recursive result delivery").
///
/// `capacity` comes from the builder envelope's `child_result_capacity`
/// (DECISION R3) — the same deliberate value at every depth, never a
/// library default.
pub(super) fn install_child_result_channel(
    child_ctx: &ToolContext,
    child_policy: &ChildPolicy,
    capacity: usize,
) -> Option<tokio::sync::mpsc::Receiver<ChildAgentResult>> {
    if child_policy.delegation.remaining_depth == 0 {
        return None;
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(capacity);
    child_ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    Some(rx)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::child_policy::{DelegationBudget, MessagingScope};

    fn policy(depth: u32) -> ChildPolicy {
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: depth,
                max_concurrent_children: 4,
            },
            inbound_capacity: 8,
        }
    }

    /// A delegating child gets a channel pair: sender installed on its
    /// context, receiver returned for its loop. A leaf gets neither.
    #[test]
    fn result_channel_exists_iff_child_can_delegate() {
        let ctx = ToolContext::empty();
        let rx = install_child_result_channel(&ctx, &policy(1), 16);
        assert!(rx.is_some(), "a depth-1 child drains its own children");
        assert!(
            ctx.get_extension::<ChildResultSender>().is_some(),
            "the sender rides the child's context for its spawn sites",
        );

        let leaf_ctx = ToolContext::empty();
        let leaf_rx = install_child_result_channel(&leaf_ctx, &policy(0), 16);
        assert!(leaf_rx.is_none(), "a leaf cannot spawn — no channel");
        assert!(
            leaf_ctx.get_extension::<ChildResultSender>().is_none(),
            "no sender is installed where nothing may ever send",
        );
    }

    /// The auto path nests under the spawner's registered path; an
    /// unregistered spawner (root) has no prefix.
    #[test]
    fn auto_child_path_namespaces_under_spawner() {
        let registry = AgentRegistry::shared();
        let unregistered = Uuid::new_v4();
        let root_path = auto_child_path(&registry, unregistered, "spawn");
        assert!(
            root_path.starts_with("/spawn/"),
            "unregistered root keeps the flat prefix: {root_path}",
        );

        let guard = AgentRegistry::reserve(
            &registry,
            "/root/worker".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
            policy(2),
            None,
        )
        .expect("register spawner");
        let spawner = guard.id();
        guard.confirm().expect("confirm");
        let nested = auto_child_path(&registry, spawner, "fork");
        assert!(
            nested.starts_with("/root/worker/fork/"),
            "child paths nest under the spawner: {nested}",
        );
    }
}
