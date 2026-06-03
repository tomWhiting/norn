//! Sub-agent handles created at spawn time (NA-006, NA-008).
//!
//! [`AgentHandles`] is installed as an empty [`crate::tool::context::ToolContext`]
//! extension during `build_runtime`. `SpawnAgentTool` populates it when it
//! launches a child: each [`AgentHandle`] carries the runtime resources the
//! parent needs to observe and steer a spawned child — a status watch
//! receiver, an inbound message sender, the child task's `JoinHandle`, and
//! (NA-008) the child's [`EventStore`] plus its [`ChildBranchMetadata`] so
//! the parent can read the child's audit trail on demand.
//!
//! `JoinHandle<()>` is not `Clone`, so the collection cannot hand out a
//! borrowed `&AgentHandle` through its `Mutex` without leaking the lock
//! guard. Instead the cheap, cloneable fields are exposed through typed
//! accessors ([`AgentHandles::status_rx`], [`AgentHandles::inbound_tx`],
//! [`AgentHandles::event_store`], [`AgentHandles::branch_metadata`]), and the
//! whole handle is recovered via [`AgentHandles::remove`].

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::agent::registry::AgentStatus;
use crate::r#loop::inbound::InboundSender;
use crate::session::store::EventStore;
use crate::session::tree::{SessionId, SessionTree};

/// Provenance metadata captured for each spawned child (NA-008 R3).
///
/// Recorded at spawn time and stored on the child's [`AgentHandle`] so the
/// parent can attribute the child's audit trail — who spawned it, under
/// which profile, and when — without consulting the agent registry or the
/// session tree.
#[derive(Clone, Debug)]
pub struct ChildBranchMetadata {
    /// The spawned child's agent id.
    pub child_agent_id: Uuid,
    /// The spawning agent's id.
    pub parent_agent_id: Uuid,
    /// Profile name the child was spawned with, if any.
    pub profile_name: Option<String>,
    /// Wall-clock instant the child was spawned.
    pub spawned_at: DateTime<Utc>,
}

/// Orchestrator-published handle to the shared [`SessionTree`] together with
/// the calling agent's own session id within it (NA-008 R3).
///
/// When an orchestrator installs this extension on an agent's
/// [`crate::tool::context::ToolContext`], `SpawnAgentTool` branches each
/// child's [`EventStore`] as a named child session under [`Self::session_id`],
/// wiring the child into the parent's session audit tree. When the extension
/// is absent (standalone mode) the child receives a private, disconnected
/// store instead.
///
/// The child is given its own `SharedSessionTree` — the same `tree`, but the
/// child's `session_id` — so grandchildren branch correctly in turn.
pub struct SharedSessionTree {
    /// The shared session tree.
    pub tree: Arc<SessionTree>,
    /// The calling agent's session id within [`Self::tree`].
    pub session_id: SessionId,
}

/// Live handle to a spawned sub-agent.
///
/// Constructed by `SpawnAgentTool` once the child's `tokio::spawn` task is
/// running, then stored in the spawning agent's [`AgentHandles`] extension
/// keyed by [`Self::agent_id`].
pub struct AgentHandle {
    /// The spawned sub-agent's id.
    pub agent_id: Uuid,
    /// Receiver tracking the child's lifecycle status. The spawn wrapper
    /// updates the matching sender on each terminal transition, so a
    /// reactive waiter can subscribe instead of polling.
    pub status_rx: watch::Receiver<AgentStatus>,
    /// Sender the parent uses to push `Steer` / `FollowUp` messages into the
    /// child's inbound channel at the child's next tool boundary.
    pub inbound_tx: InboundSender,
    /// Join handle for the child's `tokio::spawn` task.
    pub join_handle: JoinHandle<()>,
    /// The child's append-only session event store (NA-008 R3). In
    /// `SessionTree` mode this `Arc` aliases the tree's store for the child's
    /// session; in standalone mode it is the child's private store. Either
    /// way the parent reads the child's audit trail — including the
    /// `tool_use_description` recorded on every tool call — through this
    /// handle without real-time streaming.
    pub event_store: Arc<EventStore>,
    /// Provenance metadata captured when the child was spawned (NA-008 R3).
    pub branch_metadata: ChildBranchMetadata,
}

/// Collection of live sub-agent handles keyed by agent id.
///
/// Installed as a [`crate::tool::context::ToolContext`] extension during
/// `build_runtime` as an empty collection; `SpawnAgentTool` populates it
/// when it launches a child. The wrapping [`Mutex`] keeps the type
/// `Send + Sync` for extension-map storage.
pub struct AgentHandles {
    inner: Mutex<HashMap<Uuid, AgentHandle>>,
}

impl AgentHandles {
    /// Constructs an empty handle collection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a handle for a spawned sub-agent, keyed by its agent id.
    ///
    /// A subsequent insert for the same id replaces the previous handle.
    pub fn insert(&self, handle: AgentHandle) {
        self.inner.lock().insert(handle.agent_id, handle);
    }

    /// Removes and returns the handle for `agent_id`, if tracked.
    ///
    /// This is the only way to recover the non-cloneable
    /// [`AgentHandle::join_handle`] — callers that need to await the child
    /// task take ownership of the whole handle here.
    pub fn remove(&self, agent_id: Uuid) -> Option<AgentHandle> {
        self.inner.lock().remove(&agent_id)
    }

    /// Returns `true` when a handle for `agent_id` is tracked.
    #[must_use]
    pub fn contains(&self, agent_id: Uuid) -> bool {
        self.inner.lock().contains_key(&agent_id)
    }

    /// Returns the number of tracked sub-agent handles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Returns `true` when no sub-agent handles are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Returns a clone of the status watch receiver for `agent_id`.
    ///
    /// `watch::Receiver` is cheaply cloneable; the clone observes the same
    /// channel, so a reactive waiter can subscribe to the child's status
    /// transitions without holding the collection lock.
    #[must_use]
    pub fn status_rx(&self, agent_id: Uuid) -> Option<watch::Receiver<AgentStatus>> {
        self.inner
            .lock()
            .get(&agent_id)
            .map(|h| h.status_rx.clone())
    }

    /// Returns a clone of the inbound message sender for `agent_id`.
    ///
    /// `InboundSender` is cheaply cloneable; the clone feeds the same
    /// bounded channel, so the parent can push `Steer` / `FollowUp`
    /// messages to the child without holding the collection lock.
    #[must_use]
    pub fn inbound_tx(&self, agent_id: Uuid) -> Option<InboundSender> {
        self.inner
            .lock()
            .get(&agent_id)
            .map(|h| h.inbound_tx.clone())
    }

    /// Returns a clone of the child's [`EventStore`] handle for `agent_id`
    /// (NA-008 R3).
    ///
    /// The returned `Arc` shares the child's append-only event log, so the
    /// parent can walk the child's audit trail — assistant messages, tool
    /// calls with their `tool_use_description`, tool results — without
    /// holding the collection lock and without real-time streaming.
    #[must_use]
    pub fn event_store(&self, agent_id: Uuid) -> Option<Arc<EventStore>> {
        self.inner
            .lock()
            .get(&agent_id)
            .map(|h| Arc::clone(&h.event_store))
    }

    /// Returns a cloned snapshot of the provenance metadata for `agent_id`
    /// (NA-008 R3).
    ///
    /// [`ChildBranchMetadata`] is a small, cheaply-cloneable struct; the
    /// snapshot lets the parent attribute the child's audit trail without
    /// holding the collection lock.
    #[must_use]
    pub fn branch_metadata(&self, agent_id: Uuid) -> Option<ChildBranchMetadata> {
        self.inner
            .lock()
            .get(&agent_id)
            .map(|h| h.branch_metadata.clone())
    }

    /// Returns the ids of every tracked sub-agent (NA-008 R3).
    ///
    /// Collects the keys into an owned `Vec` so the parent can enumerate
    /// every child — and then fetch each child's [`EventStore`] via
    /// [`Self::event_store`] — without holding the collection lock during
    /// iteration.
    #[must_use]
    pub fn list_children(&self) -> Vec<Uuid> {
        self.inner.lock().keys().copied().collect()
    }
}

impl Default for AgentHandles {
    fn default() -> Self {
        Self::new()
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
    use super::*;
    use crate::r#loop::inbound::inbound_channel;
    use crate::session::events::{EventBase, SessionEvent};

    /// A trivial session event for exercising the shared-store accessor.
    fn user_event() -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hello".to_owned(),
        }
    }

    /// Builds an [`AgentHandle`] for `agent_id` with live channels, a
    /// trivial spawned task so the join handle is real, a fresh standalone
    /// [`EventStore`], and placeholder [`ChildBranchMetadata`].
    fn make_handle(agent_id: Uuid) -> AgentHandle {
        let (_status_tx, status_rx) = watch::channel(AgentStatus::Active);
        let (inbound_tx, _inbound_rx) = inbound_channel(8);
        let join_handle = tokio::spawn(async {});
        AgentHandle {
            agent_id,
            status_rx,
            inbound_tx,
            join_handle,
            event_store: Arc::new(EventStore::new()),
            branch_metadata: ChildBranchMetadata {
                child_agent_id: agent_id,
                parent_agent_id: Uuid::nil(),
                profile_name: None,
                spawned_at: Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn insert_len_contains_roundtrip() {
        let handles = AgentHandles::new();
        assert!(handles.is_empty());
        assert_eq!(handles.len(), 0);

        let id = Uuid::new_v4();
        handles.insert(make_handle(id));

        assert_eq!(handles.len(), 1);
        assert!(!handles.is_empty());
        assert!(handles.contains(id));
        assert!(!handles.contains(Uuid::new_v4()));
    }

    #[tokio::test]
    async fn remove_returns_some_for_existing_none_for_missing() {
        let handles = AgentHandles::new();
        let id = Uuid::new_v4();
        handles.insert(make_handle(id));

        let removed = handles.remove(id).expect("handle present");
        assert_eq!(removed.agent_id, id);
        assert!(handles.is_empty());

        assert!(handles.remove(id).is_none(), "second remove is None");
        assert!(
            handles.remove(Uuid::new_v4()).is_none(),
            "unknown id is None"
        );
    }

    #[tokio::test]
    async fn typed_accessors_return_some_after_insert() {
        let handles = AgentHandles::new();
        let id = Uuid::new_v4();
        handles.insert(make_handle(id));

        let status_rx = handles.status_rx(id).expect("status_rx present");
        assert_eq!(*status_rx.borrow(), AgentStatus::Active);
        assert!(handles.inbound_tx(id).is_some(), "inbound_tx present");

        let missing = Uuid::new_v4();
        assert!(handles.status_rx(missing).is_none());
        assert!(handles.inbound_tx(missing).is_none());
    }

    /// NA-008 R3: `event_store` returns the child's store as a shared `Arc`
    /// — the same store held on the handle — and `None` for unknown ids.
    #[tokio::test]
    async fn event_store_accessor_returns_shared_arc() {
        let handles = AgentHandles::new();
        let id = Uuid::new_v4();
        let handle = make_handle(id);
        let handle_store = Arc::clone(&handle.event_store);
        handles.insert(handle);

        let store = handles.event_store(id).expect("event_store present");
        assert!(
            Arc::ptr_eq(&store, &handle_store),
            "accessor must hand back the same Arc the handle holds",
        );
        store.append(user_event()).expect("append");
        let store_again = handles.event_store(id).expect("event_store present");
        assert_eq!(store_again.len(), 1, "the store is shared, not copied");

        assert!(handles.event_store(Uuid::new_v4()).is_none());
    }

    /// NA-008 R3: `branch_metadata` returns a cloned snapshot of the child's
    /// provenance, and `list_children` enumerates every tracked id.
    #[tokio::test]
    async fn branch_metadata_and_list_children_accessors() {
        let handles = AgentHandles::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        handles.insert(make_handle(a));
        handles.insert(make_handle(b));

        let meta = handles.branch_metadata(a).expect("branch_metadata present");
        assert_eq!(meta.child_agent_id, a);
        assert_eq!(meta.parent_agent_id, Uuid::nil());
        assert!(meta.profile_name.is_none());
        assert!(handles.branch_metadata(Uuid::new_v4()).is_none());

        let mut children = handles.list_children();
        children.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(children, expected);
    }
}
