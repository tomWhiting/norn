//! Task data shapes and the storage abstraction.
//!
//! This module holds the value types ([`TaskStatus`], [`TaskEntry`]) and the
//! [`TaskStore`] trait that abstracts persistence. Concrete implementations
//! (the in-memory store, and any persistent backend) live in sibling modules.

use chrono::{DateTime, Utc};
use norn_macros::ToolArgs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;

/// Task lifecycle status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Created but not yet started.
    Pending,
    /// Actively being worked on.
    InProgress,
    /// Completed successfully.
    Completed,
    /// Blocked by an external condition.
    Blocked,
    /// Terminated without success.
    Failed,
}

/// A single task tracked by the [`TaskStore`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Unique identifier (UUID-v4 generated on create if not supplied).
    pub id: String,
    /// Short human description.
    pub description: String,
    /// Current status.
    pub status: TaskStatus,
    /// IDs of tasks that must complete before this one can start.
    pub depends_on: Vec<String>,
    /// Free-form structured metadata.
    pub metadata: Value,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task was last updated.
    pub updated_at: DateTime<Utc>,
    /// Parent task in a hierarchical task tree, if this is a subtask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// Registry path of the agent that has claimed this task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_agent: Option<String>,
}

/// Abstract storage for [`TaskEntry`] records.
///
/// All methods are synchronous because in-memory implementations don't
/// need to await, and any persistent backend can wrap an executor inside
/// its `Send + Sync` implementation.
///
/// The hierarchy and group operations carry default implementations so that
/// stores predating those features keep compiling: the fallible defaults
/// return [`ToolError::ExecutionFailed`] with a descriptive reason, and the
/// read-only defaults return empty collections.
pub trait TaskStore: Send + Sync {
    /// Insert a fresh task.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if `entry.id` is already taken.
    fn create(&self, entry: TaskEntry) -> Result<(), ToolError>;

    /// Retrieve a task by id.
    fn get(&self, id: &str) -> Option<TaskEntry>;

    /// List tasks, optionally filtered by status.
    fn list(&self, filter: Option<TaskStatus>) -> Vec<TaskEntry>;

    /// Update fields of an existing task. Missing fields are left untouched.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if no task has the given id.
    fn update(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        description: Option<String>,
        depends_on: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<TaskEntry, ToolError>;

    /// Mark a task as [`TaskStatus::Completed`].
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if no task has the given id.
    fn complete(&self, id: &str) -> Result<TaskEntry, ToolError>;

    /// Insert a task as a child of `parent_id`, setting its `parent_task_id`.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if `parent_id` does not exist,
    /// if `entry.id` is already taken, or if the store does not support
    /// hierarchical tasks.
    fn create_subtask(&self, parent_id: &str, entry: TaskEntry) -> Result<(), ToolError> {
        let _ = (parent_id, entry);
        Err(ToolError::ExecutionFailed {
            reason: "create_subtask not supported by this TaskStore".to_string(),
        })
    }

    /// Return the direct children of `parent_id`, ordered by creation time.
    ///
    /// Stores that do not support hierarchical tasks return an empty vector.
    fn children(&self, parent_id: &str) -> Vec<TaskEntry> {
        let _ = parent_id;
        Vec::new()
    }

    /// Walk the parent chain from `task_id` to the root, inclusive.
    ///
    /// The returned chain starts with `task_id` itself and ends at the root
    /// task. Stores that do not support hierarchical tasks return an empty
    /// vector.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if the parent chain contains a
    /// cycle, which indicates a corrupt store.
    fn ancestors(&self, task_id: &str) -> Result<Vec<TaskEntry>, ToolError> {
        let _ = task_id;
        Ok(Vec::new())
    }

    /// Atomically assign `agent_path` to `task_id`.
    ///
    /// The claim succeeds only if the task currently has no `assigned_agent`.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if the task does not exist, if
    /// it is already claimed, or if the store does not support claiming.
    fn claim(&self, task_id: &str, agent_path: &str) -> Result<TaskEntry, ToolError> {
        let _ = (task_id, agent_path);
        Err(ToolError::ExecutionFailed {
            reason: "claim not supported by this TaskStore".to_string(),
        })
    }

    /// Create a named task group.
    ///
    /// # Errors
    /// Returns [`ToolError::ExecutionFailed`] if the store does not support
    /// task groups.
    fn create_group(&self, slug: &str) -> Result<(), ToolError> {
        let _ = slug;
        Err(ToolError::ExecutionFailed {
            reason: "create_group not supported by this TaskStore".to_string(),
        })
    }

    /// Return all known task group slugs.
    ///
    /// Stores that do not support task groups return an empty vector.
    fn list_groups(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn task_status_serde_snake_case() {
        let s = serde_json::to_string(&TaskStatus::InProgress).unwrap();
        assert_eq!(s, "\"in_progress\"");
        let back: TaskStatus = serde_json::from_str("\"completed\"").unwrap();
        assert_eq!(back, TaskStatus::Completed);
    }

    #[test]
    fn task_status_serde_failed_round_trip() {
        let s = serde_json::to_string(&TaskStatus::Failed).unwrap();
        assert_eq!(s, "\"failed\"");
        let back: TaskStatus = serde_json::from_str("\"failed\"").unwrap();
        assert_eq!(back, TaskStatus::Failed);
    }

    #[test]
    fn task_status_json_schema_is_string_enum_with_descriptions() {
        let schema = TaskStatus::json_schema();
        assert_eq!(schema["type"], "string");
        assert_eq!(
            schema["enum"],
            serde_json::json!(["pending", "in_progress", "completed", "blocked", "failed"])
        );
        let description = schema["description"].as_str().unwrap();
        assert!(description.starts_with("Task lifecycle status."));
        assert!(description.contains("in_progress: Actively being worked on."));
    }

    #[test]
    fn task_entry_hierarchy_fields_serialise() {
        let now = Utc::now();
        let entry = TaskEntry {
            id: "child-1".to_string(),
            description: "a subtask".to_string(),
            status: TaskStatus::Pending,
            depends_on: vec![],
            metadata: Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: Some("parent-1".to_string()),
            assigned_agent: Some("root/worker".to_string()),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["parent_task_id"], "parent-1");
        assert_eq!(json["assigned_agent"], "root/worker");

        let back: TaskEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back.parent_task_id.as_deref(), Some("parent-1"));
        assert_eq!(back.assigned_agent.as_deref(), Some("root/worker"));
    }

    #[test]
    fn task_entry_unrooted_omits_hierarchy_fields() {
        let now = Utc::now();
        let entry = TaskEntry {
            id: "solo".to_string(),
            description: "no parent".to_string(),
            status: TaskStatus::Pending,
            depends_on: vec![],
            metadata: Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            assigned_agent: None,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("parent_task_id").is_none());
        assert!(json.get("assigned_agent").is_none());

        // Existing fixtures without the new fields still deserialise.
        let legacy = serde_json::json!({
            "id": "legacy",
            "description": "old",
            "status": "pending",
            "depends_on": [],
            "metadata": null,
            "created_at": now,
            "updated_at": now,
        });
        let back: TaskEntry = serde_json::from_value(legacy).unwrap();
        assert!(back.parent_task_id.is_none());
        assert!(back.assigned_agent.is_none());
    }
}
