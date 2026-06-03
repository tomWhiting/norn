//! In-memory [`TaskStore`] implementation and the shared-handle wrapper.
//!
//! [`InMemoryTaskStore`] keeps tasks in a `Mutex<HashMap>` and task group
//! slugs in a `Mutex<HashSet>`. It is the backend used by tests and
//! ephemeral sessions; persistent backends live elsewhere.
//!
//! [`SharedTaskStore`] is the type-erased handle installed on the
//! [`ToolContext`](crate::tool::context::ToolContext) extension map so the
//! task tool can reach whichever store the runtime configured.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde_json::Value;

use super::types::{TaskEntry, TaskStatus, TaskStore};
use crate::error::ToolError;

/// In-memory implementation of [`TaskStore`] using a `Mutex<HashMap>`.
pub struct InMemoryTaskStore {
    inner: Mutex<HashMap<String, TaskEntry>>,
    groups: Mutex<HashSet<String>>,
}

impl InMemoryTaskStore {
    /// Constructs an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            groups: Mutex::new(HashSet::new()),
        }
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskStore for InMemoryTaskStore {
    fn create(&self, entry: TaskEntry) -> Result<(), ToolError> {
        let mut guard = self.inner.lock();
        if guard.contains_key(&entry.id) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{}' already exists", entry.id),
            });
        }
        guard.insert(entry.id.clone(), entry);
        Ok(())
    }

    fn get(&self, id: &str) -> Option<TaskEntry> {
        self.inner.lock().get(id).cloned()
    }

    fn list(&self, filter: Option<TaskStatus>) -> Vec<TaskEntry> {
        let guard = self.inner.lock();
        let mut out: Vec<TaskEntry> = guard
            .values()
            .filter(|e| filter.is_none_or(|s| e.status == s))
            .cloned()
            .collect();
        out.sort_by_key(|e| e.created_at);
        out
    }

    fn update(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        description: Option<String>,
        depends_on: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<TaskEntry, ToolError> {
        let mut guard = self.inner.lock();
        let entry = guard
            .get_mut(id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                reason: format!("task '{id}' not found"),
            })?;
        if let Some(s) = status {
            entry.status = s;
        }
        if let Some(desc) = description {
            entry.description = desc;
        }
        if let Some(deps) = depends_on {
            entry.depends_on = deps;
        }
        if let Some(meta) = metadata {
            entry.metadata = meta;
        }
        entry.updated_at = Utc::now();
        Ok(entry.clone())
    }

    fn complete(&self, id: &str) -> Result<TaskEntry, ToolError> {
        self.update(id, Some(TaskStatus::Completed), None, None, None)
    }

    fn create_subtask(&self, parent_id: &str, mut entry: TaskEntry) -> Result<(), ToolError> {
        let mut guard = self.inner.lock();
        if !guard.contains_key(parent_id) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("parent task '{parent_id}' not found"),
            });
        }
        if guard.contains_key(&entry.id) {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{}' already exists", entry.id),
            });
        }
        entry.parent_task_id = Some(parent_id.to_string());
        guard.insert(entry.id.clone(), entry);
        Ok(())
    }

    fn children(&self, parent_id: &str) -> Vec<TaskEntry> {
        let guard = self.inner.lock();
        let mut out: Vec<TaskEntry> = guard
            .values()
            .filter(|e| e.parent_task_id.as_deref() == Some(parent_id))
            .cloned()
            .collect();
        out.sort_by_key(|e| e.created_at);
        out
    }

    fn ancestors(&self, task_id: &str) -> Result<Vec<TaskEntry>, ToolError> {
        let guard = self.inner.lock();
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut cursor = Some(task_id.to_string());
        while let Some(current) = cursor {
            if !visited.insert(current.clone()) {
                return Err(ToolError::ExecutionFailed {
                    reason: "cycle detected in task hierarchy".to_string(),
                });
            }
            let Some(entry) = guard.get(&current) else {
                break;
            };
            let next = entry.parent_task_id.clone();
            chain.push(entry.clone());
            cursor = next;
        }
        Ok(chain)
    }

    fn claim(&self, task_id: &str, agent_path: &str) -> Result<TaskEntry, ToolError> {
        let mut guard = self.inner.lock();
        let entry = guard
            .get_mut(task_id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                reason: format!("task '{task_id}' not found"),
            })?;
        if let Some(existing) = &entry.assigned_agent {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{task_id}' already claimed by {existing}"),
            });
        }
        entry.assigned_agent = Some(agent_path.to_string());
        entry.updated_at = Utc::now();
        Ok(entry.clone())
    }

    fn create_group(&self, slug: &str) -> Result<(), ToolError> {
        self.groups.lock().insert(slug.to_string());
        Ok(())
    }

    fn list_groups(&self) -> Vec<String> {
        let mut out: Vec<String> = self.groups.lock().iter().cloned().collect();
        out.sort();
        out
    }
}

/// Type-erased handle for sharing a [`TaskStore`] through the extension
/// map. Tools fetch this and call methods through it.
pub struct SharedTaskStore(pub Arc<dyn TaskStore>);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::missing_const_for_fn,
    clippy::similar_names,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    fn entry(id: &str, status: TaskStatus) -> TaskEntry {
        let now = Utc::now();
        TaskEntry {
            id: id.to_string(),
            description: format!("task {id}"),
            status,
            depends_on: vec![],
            metadata: Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            assigned_agent: None,
        }
    }

    #[test]
    fn create_subtask_sets_parent_and_lists_children() {
        let store = InMemoryTaskStore::new();
        store.create(entry("parent", TaskStatus::Pending)).unwrap();
        for i in 0..3 {
            store
                .create_subtask("parent", entry(&format!("child-{i}"), TaskStatus::Pending))
                .unwrap();
        }
        let children = store.children("parent");
        assert_eq!(children.len(), 3);
        for child in &children {
            assert_eq!(child.parent_task_id.as_deref(), Some("parent"));
        }
    }

    #[test]
    fn create_subtask_missing_parent_errors() {
        let store = InMemoryTaskStore::new();
        let err = store
            .create_subtask("ghost", entry("child", TaskStatus::Pending))
            .expect_err("missing parent");
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn ancestors_walks_three_level_hierarchy_to_root() {
        let store = InMemoryTaskStore::new();
        store.create(entry("root", TaskStatus::Pending)).unwrap();
        store
            .create_subtask("root", entry("mid", TaskStatus::Pending))
            .unwrap();
        store
            .create_subtask("mid", entry("leaf", TaskStatus::Pending))
            .unwrap();
        let chain = store.ancestors("leaf").unwrap();
        let ids: Vec<&str> = chain.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["leaf", "mid", "root"]);
    }

    #[test]
    fn claim_succeeds_once_then_fails() {
        let store = InMemoryTaskStore::new();
        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        let claimed = store.claim("t1", "root/worker-a").unwrap();
        assert_eq!(claimed.assigned_agent.as_deref(), Some("root/worker-a"));
        let err = store
            .claim("t1", "root/worker-b")
            .expect_err("second claim");
        let ToolError::ExecutionFailed { reason } = err else {
            panic!("expected ExecutionFailed, got {err:?}");
        };
        assert!(reason.contains("already claimed"), "{reason}");
    }

    #[test]
    fn create_group_and_list_groups_round_trip() {
        let store = InMemoryTaskStore::new();
        store.create_group("norn-agents-wiring").unwrap();
        store.create_group("implement-hooks").unwrap();
        // Duplicate insert is idempotent.
        store.create_group("implement-hooks").unwrap();
        let groups = store.list_groups();
        assert_eq!(groups, vec!["implement-hooks", "norn-agents-wiring"]);
    }
}
