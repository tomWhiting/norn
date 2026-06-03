//! Session tree — multiple [`EventStore`]s arranged as a tree of parent/child
//! sessions with per-session metadata.
//!
//! Each tree node owns an [`Arc<EventStore>`] so a single session's store can
//! be shared with the agent loop, the fork machinery, and any other consumer
//! without copying its append-only event log. Tree state is guarded by a
//! single [`parking_lot::RwLock`], following the same in-memory map +
//! cloned-snapshot pattern used by [`crate::agent::registry::AgentRegistry`].
//!
//! The tree is purely in-memory: there is no persistence, replay, or
//! cross-tree merging (see brief boundaries).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::fork::ContextFilter;
use crate::error::SessionError;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

/// Unique identifier for a session inside a [`SessionTree`].
///
/// Generated with [`Uuid::new_v4`] to match
/// [`crate::agent::registry::AgentRegistry::reserve`]'s id allocation.
pub type SessionId = Uuid;

/// Lifecycle status of a session inside a [`SessionTree`].
///
/// Mirrors the shape of [`crate::agent::registry::AgentStatus`] for
/// consistency. Sessions start [`SessionStatus::Active`] and transition only
/// via explicit operations on the tree (e.g. [`SessionTree::merge_summary`]
/// moves a session to [`SessionStatus::Merged`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// Session is live and may still accept events.
    Active,
    /// Session finished successfully.
    Completed,
    /// Session terminated with a failure.
    Failed,
    /// Session was merged back into its parent via [`SessionTree::merge_summary`].
    Merged,
}

/// Per-session metadata stored alongside the event log on each
/// [`SessionNode`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// When the session node was inserted into the tree.
    pub created_at: DateTime<Utc>,
    /// Model identifier this session is bound to.
    pub model: String,
    /// Functional role of the session, if assigned (e.g. `dev`, `fork`).
    pub role: Option<String>,
    /// Current lifecycle status.
    pub status: SessionStatus,
}

/// A single node in the [`SessionTree`].
///
/// Holds the session's [`EventStore`] together with its tree links and
/// metadata. The store is wrapped in [`Arc`] so callers can share it (the
/// agent loop, the fork machinery, etc.) without cloning events.
#[derive(Clone, Debug)]
pub struct SessionNode {
    /// Identifier for this session.
    pub id: SessionId,
    /// Append-only event log for this session, shared via [`Arc`].
    pub store: Arc<EventStore>,
    /// Parent session id, or [`None`] for the tree root.
    pub parent: Option<SessionId>,
    /// Direct children in insertion order.
    pub children: Vec<SessionId>,
    /// Metadata describing this session.
    pub metadata: SessionMetadata,
}

/// Configuration for a [`SessionTree::branch`] invocation.
///
/// `context_filter` is applied to the parent's event log to produce the
/// child's seed events; `metadata` is stored verbatim on the new child node
/// (the caller picks the model, role, and initial status).
#[derive(Clone, Debug)]
pub struct BranchConfig {
    /// Filter applied to the parent's events before seeding the child store.
    pub context_filter: ContextFilter,
    /// Metadata for the new child session node.
    pub metadata: SessionMetadata,
}

struct TreeInner {
    nodes: HashMap<SessionId, SessionNode>,
    root: SessionId,
}

/// In-memory tree of [`SessionNode`]s with parent/child relationships.
///
/// Internally guarded by a single [`parking_lot::RwLock`]. Public methods
/// acquire the lock for the minimum scope and return **cloned snapshots**;
/// references into the tree are never handed out because that would require
/// returning a borrow tied to the lock guard. The
/// [`SessionNode::store`] is itself an [`Arc`] so cloning a node is cheap
/// and the same underlying event log is shared across callers.
pub struct SessionTree {
    inner: RwLock<TreeInner>,
}

impl SessionTree {
    /// Create a new tree whose root session uses `metadata`.
    ///
    /// The root has no parent, an empty children list, and a freshly
    /// constructed [`EventStore`]. The caller can reach the root id via
    /// [`SessionTree::root`].
    #[must_use]
    pub fn new(metadata: SessionMetadata) -> Self {
        let root_id: SessionId = Uuid::new_v4();
        let root = SessionNode {
            id: root_id,
            store: Arc::new(EventStore::new()),
            parent: None,
            children: Vec::new(),
            metadata,
        };
        let mut nodes = HashMap::new();
        nodes.insert(root_id, root);
        Self {
            inner: RwLock::new(TreeInner {
                nodes,
                root: root_id,
            }),
        }
    }

    /// Return the root session id.
    #[must_use]
    pub fn root(&self) -> SessionId {
        self.inner.read().root
    }

    /// Return a cloned snapshot of the [`SessionNode`] for `id`, if any.
    ///
    /// The returned node is a snapshot — its `store` is an [`Arc`] clone so
    /// it tracks ongoing appends, but `children` and `metadata` reflect the
    /// state at the moment of the call.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<SessionNode> {
        self.inner.read().nodes.get(&id).cloned()
    }

    /// Return the shared [`EventStore`] for `id`, if any.
    #[must_use]
    pub fn get_store(&self, id: SessionId) -> Option<Arc<EventStore>> {
        self.inner
            .read()
            .nodes
            .get(&id)
            .map(|n| Arc::clone(&n.store))
    }

    /// Total number of sessions currently in the tree.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.inner.read().nodes.len()
    }

    /// Create a child session under `parent_id`, seeding it with the parent's
    /// events filtered through `config.context_filter`.
    ///
    /// Also appends a [`SessionEvent::Fork`] to the parent's store recording
    /// the new child's id; `source_event_id` is the id of the parent's most
    /// recent event at branch time, or a synthesised [`EventId::new`] when
    /// the parent store is empty (mirrors the orphan-id convention used by
    /// [`EventBase::new`] for root events).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidEventId`] if `parent_id` is not in the
    /// tree, or [`SessionError::EventAppendFailed`] propagated from
    /// [`EventStore::append`] (e.g. on a duplicate event id while seeding).
    pub fn branch(
        &self,
        parent_id: SessionId,
        config: BranchConfig,
    ) -> Result<SessionId, SessionError> {
        let BranchConfig {
            context_filter,
            metadata,
        } = config;

        let mut inner = self.inner.write();

        let parent_store = {
            let parent =
                inner
                    .nodes
                    .get(&parent_id)
                    .ok_or_else(|| SessionError::InvalidEventId {
                        id: parent_id.to_string(),
                    })?;
            Arc::clone(&parent.store)
        };

        let parent_events = parent_store.events();
        let filtered = context_filter.apply(&parent_events);

        let child_store = Arc::new(EventStore::new());
        for event in &filtered {
            child_store.append(event.clone())?;
        }

        let child_id: SessionId = Uuid::new_v4();
        let child = SessionNode {
            id: child_id,
            store: Arc::clone(&child_store),
            parent: Some(parent_id),
            children: Vec::new(),
            metadata,
        };

        // Tie the child to the parent before releasing the lock so a
        // concurrent reader never observes a half-inserted edge.
        if let Some(parent_mut) = inner.nodes.get_mut(&parent_id) {
            parent_mut.children.push(child_id);
        }
        inner.nodes.insert(child_id, child);

        // The fork event records where in the parent's timeline the branch
        // happened. We compute the source id from the snapshot we already
        // took: branching from an empty store yields a synthesised id, which
        // matches the orphan-id convention used by EventBase::new for root
        // events.
        let source_event_id = parent_events
            .last()
            .map_or_else(EventId::new, |e| e.base().id.clone());

        drop(inner);

        // EventStore::append takes &self and uses its own internal lock, so
        // it is safe to call after releasing the tree's write lock.
        parent_store.append(SessionEvent::Fork {
            base: EventBase::new(None),
            source_event_id,
            forked_session_id: child_id.to_string(),
        })?;

        Ok(child_id)
    }

    /// Bring a child session's results back into its parent as a
    /// [`SessionEvent::Compaction`] and mark the child as
    /// [`SessionStatus::Merged`].
    ///
    /// The child's own events are never deleted — the audit trail invariant
    /// (CO7) is preserved. The compaction's `replaced_event_ids` field is
    /// empty because we are layering an external summary in, not replacing
    /// parent events.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidEventId`] if `child_id` is not in the
    /// tree, [`SessionError::StorageError`] if `child_id` is the root (which
    /// has no parent to merge into), or [`SessionError::EventAppendFailed`]
    /// propagated from [`EventStore::append`].
    pub fn merge_summary(&self, child_id: SessionId, summary: String) -> Result<(), SessionError> {
        let mut inner = self.inner.write();

        let parent_id = {
            let child = inner
                .nodes
                .get(&child_id)
                .ok_or_else(|| SessionError::InvalidEventId {
                    id: child_id.to_string(),
                })?;
            child.parent.ok_or_else(|| SessionError::StorageError {
                reason: "cannot merge root session".to_owned(),
            })?
        };

        let parent_store = {
            let parent = inner
                .nodes
                .get(&parent_id)
                .ok_or_else(|| SessionError::StorageError {
                    reason: format!("parent session {parent_id} missing from tree"),
                })?;
            Arc::clone(&parent.store)
        };

        let compaction = SessionEvent::Compaction {
            base: EventBase::new(None),
            summary,
            replaced_event_ids: Vec::new(),
        };
        parent_store.append(compaction)?;

        if let Some(child_mut) = inner.nodes.get_mut(&child_id) {
            child_mut.metadata.status = SessionStatus::Merged;
        }

        Ok(())
    }

    /// Direct children of `id` in insertion order. Returns an empty Vec if
    /// `id` is not in the tree (matches the pure-query semantics of
    /// [`crate::agent::registry::AgentRegistry::children`]).
    #[must_use]
    pub fn list_children(&self, id: SessionId) -> Vec<SessionId> {
        self.inner
            .read()
            .nodes
            .get(&id)
            .map(|n| n.children.clone())
            .unwrap_or_default()
    }

    /// Ancestry chain from the root down to `id` inclusive. Returns an empty
    /// Vec if `id` is not in the tree. Walks the parent links and reverses
    /// so the result reads root-first.
    #[must_use]
    pub fn get_ancestry(&self, id: SessionId) -> Vec<SessionId> {
        let inner = self.inner.read();
        if !inner.nodes.contains_key(&id) {
            return Vec::new();
        }
        let mut chain: Vec<SessionId> = Vec::with_capacity(8);
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            chain.push(current);
            cursor = inner.nodes.get(&current).and_then(|n| n.parent);
        }
        chain.reverse();
        chain
    }

    /// Snapshot of all session ids whose status is [`SessionStatus::Active`].
    /// Order is unspecified (`HashMap` iteration order).
    #[must_use]
    pub fn active_sessions(&self) -> Vec<SessionId> {
        self.inner
            .read()
            .nodes
            .values()
            .filter(|n| n.metadata.status == SessionStatus::Active)
            .map(|n| n.id)
            .collect()
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
    use super::*;

    fn meta(model: &str) -> SessionMetadata {
        SessionMetadata {
            created_at: Utc::now(),
            model: model.to_owned(),
            role: None,
            status: SessionStatus::Active,
        }
    }

    fn user_msg(text: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: text.to_owned(),
        }
    }

    fn branch_cfg(model: &str) -> BranchConfig {
        BranchConfig {
            context_filter: ContextFilter::default(),
            metadata: meta(model),
        }
    }

    // -- R1 -----------------------------------------------------------------

    #[test]
    fn tree_new_inserts_root() {
        let tree = SessionTree::new(meta("root-model"));
        let root_id = tree.root();
        let root = tree.get(root_id).expect("root present");
        assert!(root.parent.is_none());
        assert!(root.children.is_empty());
        assert_eq!(root.metadata.model, "root-model");
        assert_eq!(tree.session_count(), 1);
    }

    #[test]
    fn add_three_children_and_verify_links() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();

        let mut child_ids = Vec::new();
        for i in 0..3 {
            let id = tree
                .branch(root_id, branch_cfg(&format!("child-{i}")))
                .expect("branch");
            child_ids.push(id);
        }

        let root = tree.get(root_id).expect("root");
        assert_eq!(root.children.len(), 3);
        for cid in &child_ids {
            let child = tree.get(*cid).expect("child");
            assert_eq!(child.parent, Some(root_id));
        }
        assert_eq!(tree.session_count(), 4);
    }

    #[test]
    fn get_store_returns_shared_arc() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let store_a = tree.get_store(root_id).expect("store");
        store_a.append(user_msg("hello")).expect("append");
        let store_b = tree.get_store(root_id).expect("store");
        assert_eq!(store_b.len(), 1);
        assert!(Arc::ptr_eq(&store_a, &store_b));
    }

    #[test]
    fn get_unknown_returns_none() {
        let tree = SessionTree::new(meta("root"));
        assert!(tree.get(Uuid::new_v4()).is_none());
        assert!(tree.get_store(Uuid::new_v4()).is_none());
    }

    // -- R2 -----------------------------------------------------------------

    #[test]
    fn branch_filters_parent_events_and_records_fork_event() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let root_store = tree.get_store(root_id).expect("root store");
        for i in 0..10 {
            root_store
                .append(user_msg(&format!("msg {i}")))
                .expect("seed");
        }

        let cfg = BranchConfig {
            context_filter: ContextFilter {
                include_system: true,
                include_recent_n: Some(5),
                exclude_tool_calls: false,
            },
            metadata: meta("child"),
        };
        let child_id = tree.branch(root_id, cfg).expect("branch");

        let child_store = tree.get_store(child_id).expect("child store");
        assert_eq!(child_store.len(), 5);
        let child = tree.get(child_id).expect("child node");
        assert_eq!(child.parent, Some(root_id));

        let root = tree.get(root_id).expect("root node");
        assert!(root.children.contains(&child_id));

        let root_events = root_store.events();
        let last = root_events.last().expect("at least one event");
        match last {
            SessionEvent::Fork {
                forked_session_id, ..
            } => {
                let parsed: Uuid = forked_session_id.parse().expect("parse uuid");
                assert_eq!(parsed, child_id);
            }
            other => panic!("expected Fork event, got {other:?}"),
        }
    }

    #[test]
    fn branch_unknown_parent_errors() {
        let tree = SessionTree::new(meta("root"));
        let err = tree
            .branch(Uuid::new_v4(), branch_cfg("orphan"))
            .expect_err("must error");
        assert!(matches!(err, SessionError::InvalidEventId { .. }));
    }

    #[test]
    fn branch_from_empty_parent_uses_synth_source_id() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let child_id = tree.branch(root_id, branch_cfg("child")).expect("branch");

        let root_events = tree.get_store(root_id).expect("root").events();
        // One event: the Fork.
        assert_eq!(root_events.len(), 1);
        match &root_events[0] {
            SessionEvent::Fork {
                forked_session_id, ..
            } => {
                let parsed: Uuid = forked_session_id.parse().expect("parse");
                assert_eq!(parsed, child_id);
            }
            other => panic!("expected Fork, got {other:?}"),
        }
    }

    // -- R3 -----------------------------------------------------------------

    #[test]
    fn merge_summary_appends_compaction_and_marks_child_merged() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let child_id = tree.branch(root_id, branch_cfg("child")).expect("branch");

        let child_store = tree.get_store(child_id).expect("child store");
        child_store.append(user_msg("a")).expect("seed a");
        child_store.append(user_msg("b")).expect("seed b");
        let child_len_before = child_store.len();

        tree.merge_summary(child_id, "summary".to_owned())
            .expect("merge");

        let root_events = tree.get_store(root_id).expect("root").events();
        let last = root_events.last().expect("last");
        match last {
            SessionEvent::Compaction {
                summary,
                replaced_event_ids,
                ..
            } => {
                assert_eq!(summary, "summary");
                assert!(replaced_event_ids.is_empty());
            }
            other => panic!("expected Compaction, got {other:?}"),
        }

        let child = tree.get(child_id).expect("child");
        assert_eq!(child.metadata.status, SessionStatus::Merged);
        // Child events still intact.
        assert_eq!(child_store.len(), child_len_before);
    }

    #[test]
    fn merge_summary_unknown_child_errors() {
        let tree = SessionTree::new(meta("root"));
        let err = tree
            .merge_summary(Uuid::new_v4(), "x".to_owned())
            .expect_err("unknown");
        assert!(matches!(err, SessionError::InvalidEventId { .. }));
    }

    #[test]
    fn merge_summary_root_errors() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let err = tree
            .merge_summary(root_id, "x".to_owned())
            .expect_err("root cannot merge");
        assert!(matches!(err, SessionError::StorageError { .. }));
    }

    #[test]
    fn merge_summary_exactly_one_compaction_per_call() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let child_id = tree.branch(root_id, branch_cfg("child")).expect("branch");

        tree.merge_summary(child_id, "one".to_owned()).expect("m1");

        let count_compactions = |events: &[SessionEvent]| {
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::Compaction { .. }))
                .count()
        };
        let parent_events = tree.get_store(root_id).expect("root").events();
        assert_eq!(count_compactions(&parent_events), 1);
    }

    // -- R4 -----------------------------------------------------------------

    #[test]
    fn ancestry_returns_root_to_descendant_order() {
        let tree = SessionTree::new(meta("root"));
        let root_id = tree.root();
        let child_id = tree.branch(root_id, branch_cfg("child")).expect("branch");
        let grandchild_id = tree
            .branch(child_id, branch_cfg("grandchild"))
            .expect("branch");

        assert_eq!(
            tree.get_ancestry(grandchild_id),
            vec![root_id, child_id, grandchild_id]
        );
        assert_eq!(tree.list_children(root_id), vec![child_id]);
        assert_eq!(tree.list_children(child_id), vec![grandchild_id]);
        assert!(tree.list_children(Uuid::new_v4()).is_empty());
        assert!(tree.get_ancestry(Uuid::new_v4()).is_empty());

        assert_eq!(tree.active_sessions().len(), 3);
        assert_eq!(tree.session_count(), 3);

        tree.merge_summary(child_id, "done".to_owned())
            .expect("merge");
        assert_eq!(tree.active_sessions().len(), 2);
        // session_count is unchanged — merging does not remove the node.
        assert_eq!(tree.session_count(), 3);
    }
}
