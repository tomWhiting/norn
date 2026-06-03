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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
}

impl AgentRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            path_index: HashMap::new(),
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
    #[must_use]
    pub fn get_by_path(&self, path: &str) -> Option<AgentEntry> {
        self.path_index
            .get(path)
            .and_then(|id| self.entries.get(id))
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
    /// Returns [`AgentError::NotFound`] if `id` is not registered.
    pub fn mark_active(&mut self, id: Uuid) -> Result<(), AgentError> {
        self.set_status(id, AgentStatus::Active)
    }

    /// Transition an entry to [`AgentStatus::Completing`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NotFound`] if `id` is not registered.
    pub fn mark_completing(&mut self, id: Uuid) -> Result<(), AgentError> {
        self.set_status(id, AgentStatus::Completing)
    }

    /// Transition an entry to [`AgentStatus::Completed`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NotFound`] if `id` is not registered.
    pub fn mark_completed(&mut self, id: Uuid) -> Result<(), AgentError> {
        self.set_status(id, AgentStatus::Completed)
    }

    /// Transition an entry to [`AgentStatus::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NotFound`] if `id` is not registered.
    pub fn mark_failed(&mut self, id: Uuid) -> Result<(), AgentError> {
        self.set_status(id, AgentStatus::Failed)
    }

    fn set_status(&mut self, id: Uuid, status: AgentStatus) -> Result<(), AgentError> {
        match self.entries.get_mut(&id) {
            Some(entry) => {
                entry.status = status;
                Ok(())
            }
            None => Err(AgentError::NotFound {
                path: format!("id:{id}"),
            }),
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

                let active_children = guard
                    .entries
                    .values()
                    .filter(|e| {
                        e.parent_id == Some(pid)
                            && matches!(
                                e.status,
                                AgentStatus::Spawning
                                    | AgentStatus::Active
                                    | AgentStatus::Completing
                            )
                    })
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
    /// removed externally (which should not happen under normal use).
    pub fn confirm(mut self) -> Result<(), AgentError> {
        self.registry.write().mark_active(self.id)?;
        self.confirmed = true;
        Ok(())
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if !self.confirmed {
            let mut guard = self.registry.write();
            if let Some(entry) = guard.entries.remove(&self.id) {
                guard.path_index.remove(&entry.path);
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
        assert_eq!(
            registry.read().get(id).expect("entry").status,
            AgentStatus::Completed
        );
    }

    #[test]
    fn mark_unknown_returns_not_found() {
        let registry = fresh();
        let mut w = registry.write();
        let err = w.mark_active(Uuid::new_v4()).expect_err("unknown");
        assert!(matches!(err, AgentError::NotFound { .. }));
    }

    #[test]
    fn mark_failed_sets_status() {
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
        assert_eq!(
            registry.read().get(id).expect("entry").status,
            AgentStatus::Failed
        );
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
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: AgentEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.path, entry.path);
        assert_eq!(back.status, entry.status);
    }
}
