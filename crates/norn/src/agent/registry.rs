//! `AgentRegistry` — tracks active agents by hierarchical path with no
//! hardcoded concurrency limits. Spawning uses a two-phase reservation with
//! RAII cleanup via [`SpawnGuard`].

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AgentError;

const MAX_CONCURRENT_CHILDREN: usize = 32;

/// Lifecycle status of a registered agent.
///
/// Serialized in `snake_case` (`"active"`, `"completed"`, `"failed"`, ...)
/// so every status string norn emits — registry entries, tool outputs,
/// typed lifecycle events — shares one stable representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Reservation made; awaiting confirmation.
    Spawning,
    /// Confirmed and actively running.
    Active,
    /// Wrapping up — emitting final output.
    Completing,
    /// Finished successfully.
    Completed,
    /// Terminated with a failure.
    Failed,
}

impl AgentStatus {
    /// True for statuses that end the agent's lifecycle
    /// ([`Self::Completed`] and [`Self::Failed`]).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Error from a registry status transition (`mark_*`).
///
/// Defined here rather than on [`AgentError`] because the transition
/// rules are a registry invariant: terminal statuses are immutable.
/// Once an entry is [`AgentStatus::Completed`] or
/// [`AgentStatus::Failed`] its outcome is part of the audit record —
/// observers (status displays, the result channel) may have already
/// reported it, so rewriting it would falsify history. Re-marking the
/// *same* terminal status is an accepted no-op so independent
/// finalisers need no coordination.
#[derive(Debug, thiserror::Error)]
pub enum StatusTransitionError {
    /// No entry with the given id is registered (or it was reclaimed).
    #[error("agent not found: id:{id}")]
    NotFound {
        /// The unknown agent id.
        id: Uuid,
    },
    /// The entry already carries a terminal status; terminal statuses
    /// are immutable (terminal → non-terminal and terminal → different
    /// terminal transitions are both rejected).
    #[error(
        "invalid status transition for agent {id}: {from:?} is terminal and cannot become {to:?}"
    )]
    TerminalImmutable {
        /// The agent whose transition was rejected.
        id: Uuid,
        /// The entry's current (terminal) status.
        from: AgentStatus,
        /// The rejected target status.
        to: AgentStatus,
    },
}

/// A registered agent record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Unique agent identifier.
    pub id: Uuid,
    /// Hierarchical path, e.g. `/workflow/dev/agent-1`.
    pub path: String,
    /// Functional role (e.g. `dev`, `fork`, `monitor`).
    pub role: String,
    /// Current lifecycle status.
    pub status: AgentStatus,
    /// Model identifier this agent is bound to.
    pub model: String,
    /// When the reservation was created.
    pub spawned_at: DateTime<Utc>,
    /// Parent agent id, if any.
    pub parent_id: Option<Uuid>,
    /// When the entry reached a terminal status ([`AgentStatus::Completed`]
    /// or [`AgentStatus::Failed`]); `None` while the agent is live. Stamped
    /// by the registry on the terminal `mark_*` transition and carried onto
    /// the entry's [`AgentTombstone`] at reclamation.
    pub completed_at: Option<DateTime<Utc>>,
}

/// Completion record retained after a terminal entry is reclaimed.
///
/// [`AgentRegistry::remove_terminal`] leaves one of these behind so the
/// coordination tools can tell the truth about agents that finished and
/// were reclaimed: "already completed at \<ts\>" instead of the dishonest
/// "not registered". Tombstones are tiny (id, path, status, timestamp) and
/// are retained for the registry's lifetime — i.e. the session — so the
/// record never expires while anything could still ask about the agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentTombstone {
    /// The reclaimed agent's id.
    pub id: Uuid,
    /// The hierarchical path the agent was registered at. Paths are freed
    /// at the terminal transition, so a later agent may reuse this path;
    /// path-based tombstone lookup returns the most recently reclaimed
    /// holder.
    pub path: String,
    /// The terminal status the agent finished with.
    pub status: AgentStatus,
    /// When the agent reached its terminal status.
    pub completed_at: DateTime<Utc>,
}

/// In-memory registry of active agents.
///
/// [`AgentRegistry::reserve`] enforces two structural limits approved in
/// the fork-result-delivery design:
/// - **One layer deep**: a child cannot spawn grandchildren.
/// - **Concurrent cap**: a parent may have at most
///   [`MAX_CONCURRENT_CHILDREN`] non-terminal children at once.
///
/// Callers wrap the registry in `Arc<parking_lot::RwLock<AgentRegistry>>`
/// to share it across tasks. See [`AgentRegistry::shared`] for an
/// ergonomic constructor.
pub struct AgentRegistry {
    entries: HashMap<Uuid, AgentEntry>,
    path_index: HashMap<String, Uuid>,
    /// Completion records for reclaimed entries, keyed by agent id.
    /// Session-lifetime retention; see [`AgentTombstone`].
    tombstones: HashMap<Uuid, AgentTombstone>,
    /// Latest reclaimed holder of each path (paths are reusable, so a
    /// later reclamation under the same path overwrites the older record;
    /// the older record stays reachable by id).
    tombstone_path_index: HashMap<String, Uuid>,
}

impl AgentRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            path_index: HashMap::new(),
            tombstones: HashMap::new(),
            tombstone_path_index: HashMap::new(),
        }
    }

    /// Create a new `Arc<RwLock<AgentRegistry>>` wrapping a fresh registry.
    #[must_use]
    pub fn shared() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new()))
    }

    /// Return the entry for `id`, if any.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<AgentEntry> {
        self.entries.get(&id).cloned()
    }

    /// Return the entry registered at `path`, if any.
    ///
    /// Terminal entries free their path immediately (see [`Self::set_status`]),
    /// so this resolves only *live* (non-terminal) holders. Use
    /// [`Self::get_terminal_by_path`] to find an entry that finished under
    /// `path` but has not been reclaimed yet.
    #[must_use]
    pub fn get_by_path(&self, path: &str) -> Option<AgentEntry> {
        self.path_index
            .get(path)
            .and_then(|id| self.entries.get(id))
            .cloned()
    }

    /// Return the most recently finished *terminal* entry that was
    /// registered at `path`, if any.
    ///
    /// Terminal transitions remove the path from the live index (so the
    /// path is reusable) while the entry stays listed until reclaimed; this
    /// scan keeps such entries resolvable by path so coordination tools can
    /// report their real outcome instead of "not registered". When several
    /// terminal entries share the path (reuse), the latest `completed_at`
    /// wins.
    #[must_use]
    pub fn get_terminal_by_path(&self, path: &str) -> Option<AgentEntry> {
        self.entries
            .values()
            .filter(|e| e.status.is_terminal() && e.path == path)
            .max_by_key(|e| e.completed_at)
            .cloned()
    }

    /// Return the completion record for a reclaimed agent, if any.
    #[must_use]
    pub fn tombstone(&self, id: Uuid) -> Option<AgentTombstone> {
        self.tombstones.get(&id).cloned()
    }

    /// Return the completion record of the most recently reclaimed agent
    /// that was registered at `path`, if any.
    #[must_use]
    pub fn tombstone_by_path(&self, path: &str) -> Option<AgentTombstone> {
        self.tombstone_path_index
            .get(path)
            .and_then(|id| self.tombstones.get(id))
            .cloned()
    }

    /// Snapshot of all registered entries.
    #[must_use]
    pub fn list(&self) -> Vec<AgentEntry> {
        self.entries.values().cloned().collect()
    }

    /// Snapshot of direct children of `parent_id`.
    #[must_use]
    pub fn children(&self, parent_id: Uuid) -> Vec<AgentEntry> {
        self.entries
            .values()
            .filter(|e| e.parent_id == Some(parent_id))
            .cloned()
            .collect()
    }

    /// Number of registered entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Transition an entry to [`AgentStatus::Active`].
    ///
    /// # Errors
    ///
    /// Returns [`StatusTransitionError::NotFound`] if `id` is not
    /// registered, or [`StatusTransitionError::TerminalImmutable`] if the
    /// entry is already terminal.
    pub fn mark_active(&mut self, id: Uuid) -> Result<(), StatusTransitionError> {
        self.set_status(id, AgentStatus::Active)
    }

    /// Transition an entry to [`AgentStatus::Completing`].
    ///
    /// # Errors
    ///
    /// Returns [`StatusTransitionError::NotFound`] if `id` is not
    /// registered, or [`StatusTransitionError::TerminalImmutable`] if the
    /// entry is already terminal.
    pub fn mark_completing(&mut self, id: Uuid) -> Result<(), StatusTransitionError> {
        self.set_status(id, AgentStatus::Completing)
    }

    /// Transition an entry to [`AgentStatus::Completed`], freeing its path
    /// for reuse. The entry itself stays listed (with terminal status) for
    /// observers such as status displays until [`Self::remove_terminal`]
    /// reclaims it.
    ///
    /// # Errors
    ///
    /// Returns [`StatusTransitionError::NotFound`] if `id` is not
    /// registered, or [`StatusTransitionError::TerminalImmutable`] if the
    /// entry is already [`AgentStatus::Failed`]. Re-marking an already
    /// completed entry is an accepted no-op.
    pub fn mark_completed(&mut self, id: Uuid) -> Result<(), StatusTransitionError> {
        self.set_status(id, AgentStatus::Completed)
    }

    /// Transition an entry to [`AgentStatus::Failed`], freeing its path for
    /// reuse. The entry itself stays listed (with terminal status) for
    /// observers such as status displays until [`Self::remove_terminal`]
    /// reclaims it.
    ///
    /// # Errors
    ///
    /// Returns [`StatusTransitionError::NotFound`] if `id` is not
    /// registered, or [`StatusTransitionError::TerminalImmutable`] if the
    /// entry is already [`AgentStatus::Completed`]. Re-marking an already
    /// failed entry is an accepted no-op.
    pub fn mark_failed(&mut self, id: Uuid) -> Result<(), StatusTransitionError> {
        self.set_status(id, AgentStatus::Failed)
    }

    /// Reclaim a terminal entry, removing it from the registry and leaving
    /// an [`AgentTombstone`] behind so the agent's completion stays
    /// reportable for the rest of the session.
    ///
    /// Returns `true` when an entry was removed; `false` when `id` is
    /// absent (already reclaimed) or still non-terminal. Idempotent, so
    /// every reclaimer may call it without coordination. Non-terminal
    /// entries are never removed — lifecycle removal goes through
    /// [`SpawnGuard`] rollback or a terminal `mark_*` first.
    ///
    /// One owner reclaims a naturally-finished child per launch mode:
    /// `close_agent` for forced shutdowns, an external status observer
    /// (e.g. the TUI panel after its hold window) when one is attached,
    /// the spawn/fork wrapper at result delivery when the runtime
    /// installed
    /// [`ReclaimOnResultDelivery`](crate::tools::agent::ReclaimOnResultDelivery),
    /// and otherwise whoever holds the child's
    /// [`AgentHandle`](crate::tools::agent::AgentHandle). See
    /// [`crate::tools::agent::reclaim`] for the full rule.
    pub fn remove_terminal(&mut self, id: Uuid) -> bool {
        match self.entries.get(&id) {
            Some(entry) if entry.status.is_terminal() => {
                let tombstone = AgentTombstone {
                    id: entry.id,
                    path: entry.path.clone(),
                    status: entry.status,
                    // Terminal entries are always stamped by `set_status`;
                    // the fallback keeps the record honest-ish (reclaim
                    // time) should an entry ever reach terminal without a
                    // stamp.
                    completed_at: entry.completed_at.unwrap_or_else(Utc::now),
                };
                self.tombstone_path_index
                    .insert(tombstone.path.clone(), tombstone.id);
                self.tombstones.insert(id, tombstone);
                self.entries.remove(&id);
                true
            }
            _ => false,
        }
    }

    /// Apply a status transition. Terminal statuses free the entry's path
    /// immediately (so the path is reusable, mirroring the RAII rollback
    /// of an unconfirmed [`SpawnGuard`]) but retain the entry with its
    /// terminal status so pollers of [`Self::list`] (e.g. the TUI agent
    /// status panel's hold window) observe the outcome. Reclamation is
    /// explicit via [`Self::remove_terminal`]; richer outcome
    /// observability lives on the
    /// [`AgentHandle`](crate::tools::agent::AgentHandle) status watch
    /// channel and the child result channel.
    ///
    /// Terminal statuses are immutable: once an entry is Completed or
    /// Failed, transitioning it to any *different* status — resurrection
    /// to a non-terminal state or rewriting one terminal outcome as the
    /// other — is rejected with
    /// [`StatusTransitionError::TerminalImmutable`]. Re-applying the same
    /// terminal status is an accepted no-op.
    fn set_status(&mut self, id: Uuid, status: AgentStatus) -> Result<(), StatusTransitionError> {
        match self.entries.get_mut(&id) {
            Some(entry) => {
                if entry.status.is_terminal() && entry.status != status {
                    return Err(StatusTransitionError::TerminalImmutable {
                        id,
                        from: entry.status,
                        to: status,
                    });
                }
                entry.status = status;
                if status.is_terminal() {
                    if entry.completed_at.is_none() {
                        entry.completed_at = Some(Utc::now());
                    }
                    self.path_index.remove(&entry.path);
                }
                Ok(())
            }
            None => Err(StatusTransitionError::NotFound { id }),
        }
    }

    /// Reserve a new agent slot, returning a [`SpawnGuard`].
    ///
    /// The reservation inserts an entry in [`AgentStatus::Spawning`]. The
    /// caller must invoke [`SpawnGuard::confirm`] to transition the entry
    /// to [`AgentStatus::Active`]; otherwise dropping the guard rolls the
    /// reservation back automatically.
    ///
    /// Enforces two structural limits:
    /// - **One layer deep**: a child agent (one with `parent_id`) cannot
    ///   spawn grandchildren.
    /// - **Concurrent cap**: a single parent may have at most 32
    ///   non-terminal children at once.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::SpawnFailed`] if `path` is already in use,
    /// the caller is itself a child, or the concurrent child cap is
    /// reached.
    pub fn reserve(
        registry: &Arc<RwLock<Self>>,
        path: String,
        role: String,
        model: String,
        parent_id: Option<Uuid>,
    ) -> Result<SpawnGuard, AgentError> {
        let id = Uuid::new_v4();
        {
            let mut guard = registry.write();
            if guard.path_index.contains_key(&path) {
                return Err(AgentError::SpawnFailed {
                    reason: format!("path already in use: {path}"),
                });
            }

            if let Some(pid) = parent_id {
                if let Some(parent_entry) = guard.entries.get(&pid)
                    && parent_entry.parent_id.is_some()
                {
                    return Err(AgentError::SpawnFailed {
                        reason: "spawn depth exceeded: children cannot spawn \
                                 grandchildren"
                            .to_owned(),
                    });
                }

                // Terminal entries linger until `remove_terminal` reclaims
                // them, so the cap must count only non-terminal children.
                let active_children = guard
                    .entries
                    .values()
                    .filter(|e| e.parent_id == Some(pid) && !e.status.is_terminal())
                    .count();
                if active_children >= MAX_CONCURRENT_CHILDREN {
                    return Err(AgentError::SpawnFailed {
                        reason: "concurrent child limit reached".to_owned(),
                    });
                }
            }

            let entry = AgentEntry {
                id,
                path: path.clone(),
                role,
                status: AgentStatus::Spawning,
                model,
                spawned_at: Utc::now(),
                parent_id,
                completed_at: None,
            };
            guard.entries.insert(id, entry);
            guard.path_index.insert(path, id);
        }
        Ok(SpawnGuard {
            registry: Arc::clone(registry),
            id,
            confirmed: false,
        })
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard for a reserved agent slot.
///
/// While the guard exists without [`SpawnGuard::confirm`] being called,
/// dropping it removes the reservation. Calling `confirm` transitions
/// the entry to [`AgentStatus::Active`] and consumes the guard so that
/// the entry persists for the rest of the agent's lifecycle.
pub struct SpawnGuard {
    registry: Arc<RwLock<AgentRegistry>>,
    id: Uuid,
    confirmed: bool,
}

impl std::fmt::Debug for SpawnGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnGuard")
            .field("id", &self.id)
            .field("confirmed", &self.confirmed)
            .finish_non_exhaustive()
    }
}

impl SpawnGuard {
    /// The reserved agent's id.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Confirm the reservation. Transitions the entry to
    /// [`AgentStatus::Active`] and consumes the guard.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NotFound`] if the reservation was already
    /// removed externally, or [`AgentError::SpawnFailed`] if it was
    /// externally driven to a terminal status before confirmation
    /// (neither should happen under normal use).
    pub fn confirm(mut self) -> Result<(), AgentError> {
        self.registry
            .write()
            .mark_active(self.id)
            .map_err(|e| match e {
                StatusTransitionError::NotFound { id } => AgentError::NotFound {
                    path: format!("id:{id}"),
                },
                terminal @ StatusTransitionError::TerminalImmutable { .. } => {
                    AgentError::SpawnFailed {
                        reason: terminal.to_string(),
                    }
                }
            })?;
        self.confirmed = true;
        Ok(())
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        let mut guard = self.registry.write();
        // RAII rollback may only ever undo the reservation it created:
        // an entry that is no longer `Spawning` has been confirmed or
        // driven through its lifecycle by another owner (the launch
        // wrapper owes confirmed entries a terminal transition), so
        // deleting it here would vanish an entry out from under that
        // owner. That state is unreachable through the spawn/fork tools
        // (they confirm exactly once, before launch) — if it ever shows
        // up, something external mutated a reservation it does not own.
        match guard.entries.get(&self.id) {
            Some(entry) if entry.status == AgentStatus::Spawning => {
                if let Some(entry) = guard.entries.remove(&self.id) {
                    guard.path_index.remove(&entry.path);
                }
            }
            Some(entry) => {
                tracing::error!(
                    agent_id = %self.id,
                    status = ?entry.status,
                    "invariant violation: unconfirmed SpawnGuard dropped over an entry \
                     that is no longer Spawning; leaving the entry to its lifecycle owner",
                );
            }
            None => {
                tracing::error!(
                    agent_id = %self.id,
                    "invariant violation: unconfirmed SpawnGuard dropped but its \
                     reservation is already gone from the registry",
                );
            }
        }
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

    fn fresh() -> Arc<RwLock<AgentRegistry>> {
        AgentRegistry::shared()
    }

    #[test]
    fn reserve_and_confirm_persists_entry() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/a".to_string(),
            "dev".to_string(),
            "claude-sonnet".to_string(),
            None,
        )
        .expect("reserve");

        let id = guard.id();
        {
            let r = registry.read();
            let entry = r.get(id).expect("entry exists");
            assert_eq!(entry.status, AgentStatus::Spawning);
            assert_eq!(entry.path, "/root/a");
            assert_eq!(entry.role, "dev");
            assert_eq!(entry.model, "claude-sonnet");
            assert!(entry.parent_id.is_none());
        }

        guard.confirm().expect("confirm");

        let r = registry.read();
        let entry = r.get(id).expect("entry still exists");
        assert_eq!(entry.status, AgentStatus::Active);
        assert!(r.get_by_path("/root/a").is_some());
    }

    #[test]
    fn reserve_without_confirm_cleans_up_on_drop() {
        let registry = fresh();
        let id;
        {
            let guard = AgentRegistry::reserve(
                &registry,
                "/root/transient".to_string(),
                "fork".to_string(),
                "haiku".to_string(),
                None,
            )
            .expect("reserve");
            id = guard.id();
            assert!(registry.read().get(id).is_some());
            // Drop without confirming.
        }
        let r = registry.read();
        assert!(r.get(id).is_none());
        assert!(r.get_by_path("/root/transient").is_none());
    }

    #[test]
    fn duplicate_path_rejected() {
        let registry = fresh();
        let _first = AgentRegistry::reserve(
            &registry,
            "/root/dup".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("first");

        let err = AgentRegistry::reserve(
            &registry,
            "/root/dup".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect_err("duplicate must error");

        assert!(matches!(err, AgentError::SpawnFailed { .. }));
    }

    #[test]
    fn one_hundred_agents_all_accessible() {
        let registry = fresh();
        let mut ids = Vec::with_capacity(100);
        let mut guards = Vec::with_capacity(100);
        for i in 0..100 {
            let guard = AgentRegistry::reserve(
                &registry,
                format!("/root/agent-{i}"),
                "dev".to_string(),
                "claude".to_string(),
                None,
            )
            .expect("reserve");
            ids.push(guard.id());
            guards.push(guard);
        }
        for g in guards {
            g.confirm().expect("confirm");
        }

        let r = registry.read();
        assert_eq!(r.len(), 100);
        for (i, id) in ids.iter().enumerate() {
            assert!(r.get(*id).is_some(), "id {id} not found");
            assert!(
                r.get_by_path(&format!("/root/agent-{i}")).is_some(),
                "path /root/agent-{i} not found"
            );
        }
        assert_eq!(r.list().len(), 100);
    }

    #[test]
    fn children_returns_direct_children() {
        let registry = fresh();
        let parent = AgentRegistry::reserve(
            &registry,
            "/root/parent".to_string(),
            "lead".to_string(),
            "opus".to_string(),
            None,
        )
        .expect("reserve parent");
        let parent_id = parent.id();
        parent.confirm().expect("confirm parent");

        let child_a = AgentRegistry::reserve(
            &registry,
            "/root/parent/a".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent_id),
        )
        .expect("reserve child a");
        let first_child_id = child_a.id();
        child_a.confirm().expect("confirm a");

        let child_b = AgentRegistry::reserve(
            &registry,
            "/root/parent/b".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent_id),
        )
        .expect("reserve child b");
        let second_child_id = child_b.id();
        child_b.confirm().expect("confirm b");

        let r = registry.read();
        let kids = r.children(parent_id);
        assert_eq!(kids.len(), 2);
        let ids: std::collections::HashSet<Uuid> = kids.iter().map(|e| e.id).collect();
        assert!(ids.contains(&first_child_id));
        assert!(ids.contains(&second_child_id));
    }

    #[test]
    fn status_transitions() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/states".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");

        {
            let mut w = registry.write();
            w.mark_completing(id).expect("completing");
        }
        assert_eq!(
            registry.read().get(id).expect("entry").status,
            AgentStatus::Completing
        );

        {
            let mut w = registry.write();
            w.mark_completed(id).expect("completed");
        }
        // Terminal transition frees the path immediately but keeps the
        // entry observable (status displays hold it) until reclaimed.
        let r = registry.read();
        assert_eq!(
            r.get(id).expect("terminal entry observable").status,
            AgentStatus::Completed
        );
        assert!(r.get_by_path("/root/states").is_none(), "path is freed");
        drop(r);
        assert!(registry.write().remove_terminal(id), "reclaim succeeds");
        assert!(registry.read().get(id).is_none(), "entry reclaimed");
    }

    #[test]
    fn mark_unknown_returns_not_found() {
        let registry = fresh();
        let mut w = registry.write();
        let err = w.mark_active(Uuid::new_v4()).expect_err("unknown");
        assert!(matches!(err, StatusTransitionError::NotFound { .. }));
        let err = w.mark_completed(Uuid::new_v4()).expect_err("unknown");
        assert!(matches!(err, StatusTransitionError::NotFound { .. }));
    }

    /// Terminal-resurrection regression: a Completed or Failed entry can
    /// never transition back to a non-terminal status, and one terminal
    /// outcome can never be rewritten as the other. Re-marking the same
    /// terminal status stays an accepted no-op.
    #[test]
    fn terminal_status_is_immutable() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/immutable".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");
        registry.write().mark_failed(id).expect("fail");

        let mut w = registry.write();
        // terminal → non-terminal: rejected, status unchanged.
        let err = w.mark_active(id).expect_err("Failed must not resurrect");
        assert!(matches!(
            err,
            StatusTransitionError::TerminalImmutable {
                from: AgentStatus::Failed,
                to: AgentStatus::Active,
                ..
            }
        ));
        let err = w
            .mark_completing(id)
            .expect_err("Failed must not become Completing");
        assert!(matches!(
            err,
            StatusTransitionError::TerminalImmutable { .. }
        ));
        // terminal → different terminal: rejected (a failure is never
        // rewritten as a success).
        let err = w
            .mark_completed(id)
            .expect_err("Failed must not be rewritten as Completed");
        assert!(matches!(
            err,
            StatusTransitionError::TerminalImmutable {
                from: AgentStatus::Failed,
                to: AgentStatus::Completed,
                ..
            }
        ));
        // terminal → same terminal: idempotent no-op.
        w.mark_failed(id).expect("re-marking Failed is a no-op");
        assert_eq!(w.get(id).expect("entry").status, AgentStatus::Failed);
    }

    /// The Completed direction of terminal immutability.
    #[test]
    fn completed_status_cannot_be_rewritten() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/done".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");
        registry.write().mark_completed(id).expect("complete");

        let mut w = registry.write();
        assert!(matches!(
            w.mark_failed(id),
            Err(StatusTransitionError::TerminalImmutable { .. })
        ));
        assert!(matches!(
            w.mark_active(id),
            Err(StatusTransitionError::TerminalImmutable { .. })
        ));
        w.mark_completed(id)
            .expect("re-marking Completed is a no-op");
        assert_eq!(w.get(id).expect("entry").status, AgentStatus::Completed);
    }

    #[test]
    fn mark_failed_frees_path_and_retains_entry_until_reclaimed() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/x".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");
        registry.write().mark_failed(id).expect("mark_failed");
        {
            let r = registry.read();
            let entry = r.get(id).expect("terminal entry stays observable");
            assert_eq!(entry.status, AgentStatus::Failed);
            assert!(r.get_by_path("/root/x").is_none(), "path is freed");
        }
        assert!(
            registry.write().remove_terminal(id),
            "terminal entry reclaims"
        );
        assert!(registry.read().get(id).is_none(), "entry removed");
        assert!(
            !registry.write().remove_terminal(id),
            "reclaim is idempotent"
        );
    }

    #[test]
    fn remove_terminal_never_removes_live_entries() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/live".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");
        assert!(
            !registry.write().remove_terminal(id),
            "active entry must not be reclaimable"
        );
        assert!(registry.read().get(id).is_some());
    }

    /// Fix 10 regression: a path used by a finished agent is reusable. Before
    /// terminal cleanup, the stale `path_index` entry rejected the second
    /// reservation forever.
    #[test]
    fn terminal_cleanup_allows_path_reuse() {
        let registry = fresh();
        let parent = AgentRegistry::reserve(
            &registry,
            "/root".to_string(),
            "lead".to_string(),
            "opus".to_string(),
            None,
        )
        .expect("reserve parent");
        let parent_id = parent.id();
        parent.confirm().expect("confirm parent");

        let first = AgentRegistry::reserve(
            &registry,
            "/root/worker".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent_id),
        )
        .expect("first reservation");
        let first_id = first.id();
        first.confirm().expect("confirm first");
        registry.write().mark_completed(first_id).expect("complete");

        let second = AgentRegistry::reserve(
            &registry,
            "/root/worker".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent_id),
        )
        .expect("a completed agent's path must be reusable");
        let second_id = second.id();
        second.confirm().expect("confirm second");
        assert_ne!(first_id, second_id);
        assert_eq!(
            registry
                .read()
                .get_by_path("/root/worker")
                .expect("reused path resolves")
                .id,
            second_id,
        );

        // The failed-path variant frees the slot just the same.
        registry.write().mark_failed(second_id).expect("fail");
        let third = AgentRegistry::reserve(
            &registry,
            "/root/worker".to_string(),
            "dev".to_string(),
            "haiku".to_string(),
            Some(parent_id),
        )
        .expect("a failed agent's path must be reusable");
        drop(third);
    }

    #[test]
    fn entry_serde_roundtrip() {
        let entry = AgentEntry {
            id: Uuid::new_v4(),
            path: "/p".to_string(),
            role: "dev".to_string(),
            status: AgentStatus::Active,
            model: "claude".to_string(),
            spawned_at: Utc::now(),
            parent_id: None,
            completed_at: None,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: AgentEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.path, entry.path);
        assert_eq!(back.status, entry.status);
    }

    /// `remove_terminal` leaves a tombstone carrying the entry's id, path,
    /// terminal status, and the timestamp stamped at the terminal mark —
    /// so coordination tools can report "already completed at <ts>"
    /// instead of "not registered" for the rest of the session.
    #[test]
    fn remove_terminal_leaves_truthful_tombstone() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/done".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");

        let before = Utc::now();
        registry.write().mark_failed(id).expect("fail");
        let stamped = registry
            .read()
            .get(id)
            .expect("terminal entry observable")
            .completed_at
            .expect("terminal mark must stamp completed_at");
        assert!(stamped >= before, "completed_at is the terminal-mark time");

        assert!(registry.write().remove_terminal(id), "reclaim succeeds");
        let r = registry.read();
        let tombstone = r.tombstone(id).expect("tombstone retained after reclaim");
        assert_eq!(tombstone.id, id);
        assert_eq!(tombstone.path, "/root/done");
        assert_eq!(tombstone.status, AgentStatus::Failed);
        assert_eq!(tombstone.completed_at, stamped);
        let by_path = r
            .tombstone_by_path("/root/done")
            .expect("tombstone resolvable by path");
        assert_eq!(by_path.id, id);

        // Live agents never have tombstones until reclaimed.
        assert!(r.tombstone(Uuid::new_v4()).is_none());
        assert!(r.tombstone_by_path("/never-existed").is_none());
    }

    /// Path reuse keeps tombstones honest: the path index points at the
    /// most recently reclaimed holder while the older record stays
    /// reachable by id.
    #[test]
    fn tombstone_path_lookup_returns_latest_holder() {
        let registry = fresh();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let guard = AgentRegistry::reserve(
                &registry,
                "/root/worker".to_string(),
                "dev".to_string(),
                "claude".to_string(),
                None,
            )
            .expect("reserve");
            let id = guard.id();
            guard.confirm().expect("confirm");
            registry.write().mark_completed(id).expect("complete");
            assert!(registry.write().remove_terminal(id), "reclaim");
            ids.push(id);
        }
        let r = registry.read();
        assert_eq!(
            r.tombstone_by_path("/root/worker").expect("latest").id,
            ids[1],
            "path lookup returns the most recently reclaimed holder",
        );
        assert!(
            r.tombstone(ids[0]).is_some(),
            "the older holder's record stays reachable by id",
        );
    }

    /// A reservation rolled back by guard drop was never an agent — no
    /// tombstone may be left behind.
    #[test]
    fn rolled_back_reservation_leaves_no_tombstone() {
        let registry = fresh();
        let id;
        {
            let guard = AgentRegistry::reserve(
                &registry,
                "/root/rollback".to_string(),
                "dev".to_string(),
                "claude".to_string(),
                None,
            )
            .expect("reserve");
            id = guard.id();
        }
        let r = registry.read();
        assert!(r.get(id).is_none(), "reservation rolled back");
        assert!(r.tombstone(id).is_none(), "no tombstone for a rollback");
        assert!(r.tombstone_by_path("/root/rollback").is_none());
    }

    /// A terminal entry frees its path from the live index but must stay
    /// resolvable via `get_terminal_by_path` until reclaimed — so tools
    /// addressing it by path can report its real outcome.
    #[test]
    fn terminal_entry_resolvable_by_path_until_reclaimed() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/finished".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        guard.confirm().expect("confirm");
        registry.write().mark_completed(id).expect("complete");

        let r = registry.read();
        assert!(r.get_by_path("/root/finished").is_none(), "path freed");
        let entry = r
            .get_terminal_by_path("/root/finished")
            .expect("terminal entry resolvable by path");
        assert_eq!(entry.id, id);
        assert_eq!(entry.status, AgentStatus::Completed);
        drop(r);

        assert!(registry.write().remove_terminal(id));
        assert!(
            registry
                .read()
                .get_terminal_by_path("/root/finished")
                .is_none(),
            "reclaimed entries resolve via tombstones instead",
        );
    }

    /// Guard-drop hardening: rollback may only undo the `Spawning`
    /// reservation it created. An entry that was confirmed (or otherwise
    /// driven onward) is owed its lifecycle by another owner and must
    /// survive an unconfirmed guard drop.
    #[test]
    fn unconfirmed_guard_drop_never_removes_activated_entry() {
        let registry = fresh();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root/external".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
        )
        .expect("reserve");
        let id = guard.id();
        // Externally activate the entry while the guard is still
        // unconfirmed (a state the spawn/fork tools never produce).
        registry.write().mark_active(id).expect("activate");
        drop(guard);

        let r = registry.read();
        let entry = r.get(id).expect("activated entry must survive guard drop");
        assert_eq!(entry.status, AgentStatus::Active);
        assert!(
            r.get_by_path("/root/external").is_some(),
            "the live path index must survive too",
        );
    }

    /// Status strings are part of the embedder contract: snake_case,
    /// matching the typed lifecycle events and tool outputs.
    #[test]
    fn agent_status_serializes_snake_case() {
        let cases = [
            (AgentStatus::Spawning, "\"spawning\""),
            (AgentStatus::Active, "\"active\""),
            (AgentStatus::Completing, "\"completing\""),
            (AgentStatus::Completed, "\"completed\""),
            (AgentStatus::Failed, "\"failed\""),
        ];
        for (status, expected) in cases {
            let json = serde_json::to_string(&status).expect("serialize");
            assert_eq!(json, expected);
        }
    }
}
