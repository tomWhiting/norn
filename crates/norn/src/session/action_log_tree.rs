//! Session-wide tree of per-agent [`ActionLog`]s.
//!
//! Every agent in a session — root, fork, spawn — records its tool
//! dispatches into its **own** [`ActionLog`] over its **own**
//! [`EventStore`](crate::session::store::EventStore). The
//! [`ActionLogTree`] is the session-wide registry of those logs, keyed by
//! agent id with parent links mirroring the spawn/fork genealogy carried
//! on [`AgentToolInfra`](crate::tools::agent::AgentToolInfra)
//! (`agent_id` / `parent_id`). It is anchored on **agent ids**, not
//! session ids, because a child of an ephemeral parent has no session id
//! at all — yet its action log must still be reachable from the parent.
//! (Persistent children unify the two: their session id IS their agent
//! id.)
//!
//! The tree is published on the shared
//! [`ToolContext`](crate::tool::context::ToolContext) as an
//! `Arc<ActionLogTree>` extension and forwarded parent → child at every
//! spawn/fork site, so a parent's `action_log` tool can federate queries
//! over its descendants' logs. Attribution is **structural**: an entry's
//! agent is the log it lives in — there is no writable attribution field.
//!
//! # Lifetime and reclamation
//!
//! Registered logs are retained for the lifetime of the tree (the
//! session), independent of
//! [`AgentRegistry`](crate::agent::registry::AgentRegistry) reclamation:
//! a finished child's registry entry may be reclaimed, but its action log
//! stays queryable — the audit outlives the agent, consistent with the
//! registry's tombstone retention.
//!
//! # Persistence and resume
//!
//! The tree is purely in-memory and session-scoped. On session resume,
//! only the root agent's log is rebuilt from the persisted event store
//! (see [`crate::agent::resume::rebuild_action_log`]); child session
//! branches are not persisted today, so a resumed session's tree starts
//! with the root alone.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::session::action_log::ActionLog;

struct TreeInner {
    /// Per-agent action logs. An agent appears here exactly once, at
    /// registration time; duplicates are rejected (first registration
    /// wins) because an agent id identifies one launch.
    logs: HashMap<Uuid, Arc<ActionLog>>,
    /// Child → parent links.
    parents: HashMap<Uuid, Uuid>,
    /// Parent → children links, in registration order.
    children: HashMap<Uuid, Vec<Uuid>>,
}

/// Registry of per-agent [`ActionLog`]s with parent/child links.
///
/// Guarded by a single [`parking_lot::RwLock`], matching [`ActionLog`]'s
/// own concurrency model: parallel children register and query
/// concurrently without contention on the logs themselves (each agent
/// writes only its own log) — only registration and link walks take the
/// tree lock, for the minimum scope.
pub struct ActionLogTree {
    root: Uuid,
    inner: RwLock<TreeInner>,
}

impl std::fmt::Debug for ActionLogTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read();
        f.debug_struct("ActionLogTree")
            .field("root", &self.root)
            .field("logs", &inner.logs.len())
            .finish()
    }
}

impl ActionLogTree {
    /// Create a tree rooted at `root_agent_id`.
    ///
    /// The root's own log is added via [`Self::register`] with
    /// `parent: None`; creation and registration are separate so launch
    /// paths that have an agent id before a log exists (e.g. a runtime
    /// assembled outside `AgentBuilder`) can still anchor the tree.
    #[must_use]
    pub fn new(root_agent_id: Uuid) -> Self {
        Self {
            root: root_agent_id,
            inner: RwLock::new(TreeInner {
                logs: HashMap::new(),
                parents: HashMap::new(),
                children: HashMap::new(),
            }),
        }
    }

    /// The root agent's id (label `"root"` in federated query output).
    #[must_use]
    pub fn root(&self) -> Uuid {
        self.root
    }

    /// Register `agent_id`'s action log, linked under `parent` (use
    /// `None` for the root agent).
    ///
    /// A duplicate registration for an already-registered id keeps the
    /// first log and logs a warning — agent ids are minted per launch, so
    /// a duplicate indicates a wiring bug, never a legitimate replacement.
    pub fn register(&self, agent_id: Uuid, parent: Option<Uuid>, log: Arc<ActionLog>) {
        let mut inner = self.inner.write();
        if inner.logs.contains_key(&agent_id) {
            tracing::warn!(
                agent_id = %agent_id,
                "ActionLogTree::register: agent already registered; keeping the first log",
            );
            return;
        }
        inner.logs.insert(agent_id, log);
        if let Some(parent_id) = parent {
            inner.parents.insert(agent_id, parent_id);
            inner.children.entry(parent_id).or_default().push(agent_id);
        }
    }

    /// Replace the **root agent's** registered log with `log`.
    ///
    /// Single-call-site contract: session **store rotation** (the TUI's
    /// `/new`, via `rotate_store_dependents` in `norn-tui`) is the only
    /// legitimate caller. Rotation rebuilds the root agent's
    /// [`ActionLog`] against the new session store; a tree installed
    /// before the rotation captured the pre-rotation root log at
    /// registration and would otherwise serve the old conversation's
    /// ledger to federated queries forever. Everywhere else, swapping a
    /// registered log is a wiring bug — use [`Self::register`], which
    /// keeps the first log and warns on duplicates, precisely because an
    /// agent id identifies one launch.
    ///
    /// Only the root's log slot changes: parent/child links and every
    /// descendant's registered log are untouched (descendants belong to
    /// the rotated-out conversation and stay queryable for the session,
    /// consistent with the tree's retention contract). When the root had
    /// no registered log yet — the tree can be installed lazily at the
    /// first spawn/fork, before any root log is published — the new log
    /// simply becomes the root's.
    pub fn replace_root_log(&self, log: Arc<ActionLog>) {
        self.inner.write().logs.insert(self.root, log);
    }

    /// The registered log for `agent_id`, if any.
    #[must_use]
    pub fn log_of(&self, agent_id: Uuid) -> Option<Arc<ActionLog>> {
        self.inner.read().logs.get(&agent_id).map(Arc::clone)
    }

    /// Direct children of `agent_id`, in registration order. Empty when
    /// the agent has no registered children.
    #[must_use]
    pub fn children_of(&self, agent_id: Uuid) -> Vec<Uuid> {
        self.inner
            .read()
            .children
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
    }

    /// All descendants of `agent_id` (children, grandchildren, …) in
    /// depth-first preorder, excluding `agent_id` itself.
    #[must_use]
    pub fn descendants_of(&self, agent_id: Uuid) -> Vec<Uuid> {
        let inner = self.inner.read();
        let mut out = Vec::new();
        // Stack-based preorder: push children in reverse so the first
        // registered child is visited first.
        let mut stack: Vec<Uuid> = inner
            .children
            .get(&agent_id)
            .map(|c| c.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(grandchildren) = inner.children.get(&id) {
                stack.extend(grandchildren.iter().rev().copied());
            }
        }
        out
    }

    /// Whether `candidate` lies inside `ancestor`'s subtree — i.e. it is
    /// `ancestor` itself or reachable by walking parent links upward from
    /// `candidate` to `ancestor`.
    ///
    /// This is the boundary check behind the `action_log` tool's scope
    /// argument: an agent may query its own subtree only, never a parent
    /// or sibling.
    #[must_use]
    pub fn is_in_subtree(&self, ancestor: Uuid, candidate: Uuid) -> bool {
        if candidate == ancestor {
            return true;
        }
        let inner = self.inner.read();
        let mut cursor = candidate;
        // Parent links form a forest (each child registers exactly one
        // parent and ids are unique), so this walk terminates; the hop
        // bound is a defensive guard, not a semantic limit.
        for _ in 0..=inner.parents.len() {
            match inner.parents.get(&cursor) {
                Some(parent) if *parent == ancestor => return true,
                Some(parent) => cursor = *parent,
                None => return false,
            }
        }
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::store::EventStore;

    fn fresh_log() -> Arc<ActionLog> {
        Arc::new(ActionLog::new(Arc::new(EventStore::new())))
    }

    #[test]
    fn register_and_lookup_logs_and_links() {
        let root = Uuid::new_v4();
        let child_a = Uuid::new_v4();
        let child_b = Uuid::new_v4();
        let grandchild = Uuid::new_v4();

        let tree = ActionLogTree::new(root);
        assert_eq!(tree.root(), root);
        assert!(tree.log_of(root).is_none(), "nothing registered yet");

        let root_log = fresh_log();
        tree.register(root, None, Arc::clone(&root_log));
        tree.register(child_a, Some(root), fresh_log());
        tree.register(child_b, Some(root), fresh_log());
        tree.register(grandchild, Some(child_a), fresh_log());

        assert!(Arc::ptr_eq(&tree.log_of(root).unwrap(), &root_log));
        assert_eq!(tree.children_of(root), vec![child_a, child_b]);
        assert_eq!(tree.children_of(child_a), vec![grandchild]);
        assert!(tree.children_of(grandchild).is_empty());
    }

    #[test]
    fn descendants_are_preorder() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let a1 = Uuid::new_v4();

        let tree = ActionLogTree::new(root);
        tree.register(root, None, fresh_log());
        tree.register(a, Some(root), fresh_log());
        tree.register(b, Some(root), fresh_log());
        tree.register(a1, Some(a), fresh_log());

        assert_eq!(tree.descendants_of(root), vec![a, a1, b]);
        assert_eq!(tree.descendants_of(a), vec![a1]);
        assert!(tree.descendants_of(b).is_empty());
    }

    #[test]
    fn subtree_membership_enforces_the_query_boundary() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let a1 = Uuid::new_v4();

        let tree = ActionLogTree::new(root);
        tree.register(root, None, fresh_log());
        tree.register(a, Some(root), fresh_log());
        tree.register(b, Some(root), fresh_log());
        tree.register(a1, Some(a), fresh_log());

        // Self and descendants are inside.
        assert!(tree.is_in_subtree(a, a));
        assert!(tree.is_in_subtree(a, a1));
        assert!(tree.is_in_subtree(root, a1));
        // Parent and sibling are outside a child's subtree.
        assert!(
            !tree.is_in_subtree(a, root),
            "child must not reach its parent"
        );
        assert!(!tree.is_in_subtree(a, b), "child must not reach a sibling");
        assert!(
            !tree.is_in_subtree(a1, a),
            "grandchild must not reach upward"
        );
        // Unknown ids are outside everything but themselves.
        assert!(!tree.is_in_subtree(root, Uuid::new_v4()));
    }

    #[test]
    fn duplicate_registration_keeps_first_log() {
        let root = Uuid::new_v4();
        let tree = ActionLogTree::new(root);
        let first = fresh_log();
        let second = fresh_log();
        tree.register(root, None, Arc::clone(&first));
        tree.register(root, None, second);
        assert!(
            Arc::ptr_eq(&tree.log_of(root).unwrap(), &first),
            "first registration wins"
        );
    }

    /// Store rotation repoints the root's log only: descendants' logs
    /// and all links are untouched.
    #[test]
    fn replace_root_log_swaps_root_slot_only() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let tree = ActionLogTree::new(root);
        let old_root_log = fresh_log();
        let child_log = fresh_log();
        tree.register(root, None, Arc::clone(&old_root_log));
        tree.register(child, Some(root), Arc::clone(&child_log));

        let new_root_log = fresh_log();
        tree.replace_root_log(Arc::clone(&new_root_log));

        assert!(
            Arc::ptr_eq(&tree.log_of(root).unwrap(), &new_root_log),
            "the root slot must serve the rotated-in log"
        );
        assert!(
            Arc::ptr_eq(&tree.log_of(child).unwrap(), &child_log),
            "descendant logs are unaffected by rotation"
        );
        assert_eq!(tree.children_of(root), vec![child], "links are unaffected");
    }

    /// A lazily-installed tree may have no root log registered yet;
    /// rotation then simply registers the new log as the root's.
    #[test]
    fn replace_root_log_registers_when_root_had_none() {
        let root = Uuid::new_v4();
        let tree = ActionLogTree::new(root);
        assert!(tree.log_of(root).is_none());
        let log = fresh_log();
        tree.replace_root_log(Arc::clone(&log));
        assert!(Arc::ptr_eq(&tree.log_of(root).unwrap(), &log));
    }

    /// Reclamation independence: the tree retains a child's log for the
    /// session even when nothing else references the child anymore — the
    /// audit outlives the agent.
    #[test]
    fn logs_outlive_external_references() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let tree = ActionLogTree::new(root);
        {
            let child_log = fresh_log();
            child_log.record_completion(crate::session::action_log::CompletionRecord {
                tool_name: "read",
                tool_call_id: "tc-1",
                tool_use_description: "",
                outcome: crate::session::action_log::Outcome::Success,
                output: &serde_json::json!({ "path": "x", "lines": 1 }),
                args: serde_json::json!({}),
                duration_ms: 0,
                follow_ups: Vec::new(),
                post_validate_outcome: None,
                level_1_only: false,
            });
            tree.register(child, Some(root), child_log);
            // child_log dropped here — the tree's Arc keeps it alive.
        }
        let log = tree.log_of(child).expect("log retained after drop");
        assert_eq!(log.entries().len(), 1);
    }
}
