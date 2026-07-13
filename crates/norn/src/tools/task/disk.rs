//! Disk-backed [`TaskStore`] implementation.
//!
//! [`DiskTaskStore`] persists tasks to `{root_dir}/{group-slug}/` as
//! individual `{task-id}.json` files. Writes are atomic (write to a
//! sibling `.tmp.{uuid}` file, `fsync`, then `rename` into place) so a
//! crash mid-write never leaves a partially-written task on disk.
//!
//! Mutual exclusion for [`TaskStore::claim`] uses POSIX
//! `O_CREAT|O_EXCL` lock files: a successful `create_new` open is the
//! cross-process atomic primitive. A `LockGuard` RAII type cleans the
//! lock file up on every exit path, including early returns and panics.
//!
//! The store is filesystem-only — no database, no in-memory caching —
//! so multiple processes pointing at the same root directory observe
//! the same task tree. The store is also session-agnostic: a task
//! group lives until explicitly deleted, so multi-session work and
//! cross-session handoffs all read and write the same files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use std::fs::{self, OpenOptions};

use chrono::Utc;
use serde_json::Value;

mod storage;

use self::storage::LockGuard;
use super::types::{TaskEntry, TaskStatus, TaskStore};
use crate::error::ToolError;
use crate::util::validate_private_component;

/// Disk-backed implementation of [`TaskStore`].
///
/// Constructed with a `root_dir` (e.g. `~/.norn/tasks/`) and a
/// `group_slug` identifying which task group this handle reads and
/// writes. The directory `{root_dir}/{group_slug}/` is created lazily
/// on the first write; constructing a store does not touch the
/// filesystem.
pub struct DiskTaskStore {
    root_dir: PathBuf,
    group_slug: String,
    governor: Option<Arc<crate::resource::DescriptorGovernor>>,
}

impl DiskTaskStore {
    /// Construct a store rooted at `root_dir` for task group `group_slug`.
    ///
    /// Neither path is touched until the first write or read; an invalid
    /// slug is reported lazily at the first operation that needs the
    /// group directory.
    #[must_use]
    pub fn new(root_dir: PathBuf, group_slug: String) -> Self {
        Self {
            root_dir,
            group_slug,
            governor: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_governor(
        root_dir: PathBuf,
        group_slug: String,
        governor: Arc<crate::resource::DescriptorGovernor>,
    ) -> Self {
        Self {
            root_dir,
            group_slug,
            governor: Some(governor),
        }
    }

    fn acquire_transaction(&self) -> Result<crate::resource::DescriptorPermit, ToolError> {
        if let Some(governor) = self.governor.as_ref() {
            return governor
                .try_acquire(crate::resource::PRIVATE_FS_OPERATION_PEAK)
                .map_err(|error| ToolError::DescriptorAdmission(Box::new(error)));
        }
        crate::resource::acquire_private_fs()
            .map_err(|error| ToolError::DescriptorAdmission(Box::new(error)))
    }

    /// Return the validated path to the group directory.
    ///
    /// Validation happens here (not in `new`) so the constructor stays
    /// infallible and tests can spin up a store cheaply.
    fn group_relative(&self) -> Result<PathBuf, ToolError> {
        let slug = validate_slug(&self.group_slug)?;
        Ok(PathBuf::from(slug))
    }

    fn task_relative(&self, task_id: &str) -> Result<PathBuf, ToolError> {
        validate_task_id(task_id)?;
        Ok(self.group_relative()?.join(format!("{task_id}.json")))
    }

    fn lock_relative(&self, task_id: &str) -> Result<PathBuf, ToolError> {
        validate_task_id(task_id)?;
        Ok(self.group_relative()?.join(format!("{task_id}.lock")))
    }

    fn update_in_transaction(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        description: Option<String>,
        depends_on: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<TaskEntry, ToolError> {
        if !self.entry_exists(id)? {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{id}' not found"),
            });
        }
        let mut entry = self.read_entry(id)?;
        if let Some(status) = status {
            entry.status = status;
        }
        if let Some(description) = description {
            entry.description = description;
        }
        if let Some(depends_on) = depends_on {
            entry.depends_on = depends_on;
        }
        if let Some(metadata) = metadata {
            entry.metadata = metadata;
        }
        entry.updated_at = Utc::now();
        self.write_entry_atomic(&entry, true)?;
        Ok(entry)
    }
}

fn is_task_file_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn validate_task_id(task_id: &str) -> Result<&str, ToolError> {
    validate_private_component(task_id, "task id").map_err(|error| ToolError::ExecutionFailed {
        reason: error.to_string(),
    })
}

fn private_io_error(path: &Path, error: &std::io::Error) -> ToolError {
    match crate::resource::classify_descriptor_error(
        error,
        "accessing private task storage",
        Some(path),
    ) {
        Some(exhaustion) => ToolError::DescriptorExhausted(Box::new(exhaustion)),
        None => ToolError::ExecutionFailed {
            reason: format!(
                "private task storage failed at '{}': {error}",
                path.display()
            ),
        },
    }
}

/// Validate a task-group slug.
///
/// Accepts ASCII alphanumerics plus `-` and `_`. Rejects everything
/// else — particularly `..`, `/`, and any Unicode — to prevent path
/// traversal and to keep group directories well-behaved on every
/// filesystem we target.
///
/// # Errors
/// Returns [`ToolError::ExecutionFailed`] if `slug` is empty or
/// contains a character outside the accepted set.
pub fn validate_slug(slug: &str) -> Result<&str, ToolError> {
    if slug.is_empty() {
        return Err(ToolError::ExecutionFailed {
            reason: "task group slug must not be empty".to_string(),
        });
    }
    for ch in slug.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return Err(ToolError::ExecutionFailed {
                reason: format!(
                    "invalid task group slug '{slug}': only ASCII alphanumerics, '-' and '_' allowed"
                ),
            });
        }
    }
    Ok(slug)
}

impl TaskStore for DiskTaskStore {
    fn create(&self, entry: TaskEntry) -> Result<(), ToolError> {
        let _permit = self.acquire_transaction()?;
        self.write_entry_atomic(&entry, false)
    }

    fn get(&self, id: &str) -> Result<Option<TaskEntry>, ToolError> {
        let _permit = self.acquire_transaction()?;
        if !self.entry_exists(id)? {
            return Ok(None);
        }
        self.read_entry(id).map(Some)
    }

    fn list(&self, filter: Option<TaskStatus>) -> Result<Vec<TaskEntry>, ToolError> {
        let _permit = self.acquire_transaction()?;
        let mut entries = self.entries_in_group()?;
        if let Some(status) = filter {
            entries.retain(|e| e.status == status);
        }
        entries.sort_by_key(|e| e.created_at);
        Ok(entries)
    }

    fn update(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        description: Option<String>,
        depends_on: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<TaskEntry, ToolError> {
        let _permit = self.acquire_transaction()?;
        self.update_in_transaction(id, status, description, depends_on, metadata)
    }

    fn complete(&self, id: &str) -> Result<TaskEntry, ToolError> {
        let _permit = self.acquire_transaction()?;
        self.update_in_transaction(id, Some(TaskStatus::Completed), None, None, None)
    }

    fn create_subtask(&self, parent_id: &str, mut entry: TaskEntry) -> Result<(), ToolError> {
        let _permit = self.acquire_transaction()?;
        if !self.entry_exists(parent_id)? {
            return Err(ToolError::ExecutionFailed {
                reason: format!("parent task '{parent_id}' not found"),
            });
        }
        entry.parent_task_id = Some(parent_id.to_string());
        self.write_entry_atomic(&entry, false)
    }

    fn children(&self, parent_id: &str) -> Result<Vec<TaskEntry>, ToolError> {
        let _permit = self.acquire_transaction()?;
        let mut entries = self.entries_in_group()?;
        entries.retain(|e| e.parent_task_id.as_deref() == Some(parent_id));
        entries.sort_by_key(|e| e.created_at);
        Ok(entries)
    }

    fn ancestors(&self, task_id: &str) -> Result<Vec<TaskEntry>, ToolError> {
        let _permit = self.acquire_transaction()?;
        let mut chain = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut cursor = Some(task_id.to_string());
        while let Some(current) = cursor {
            if !visited.insert(current.clone()) {
                return Err(ToolError::ExecutionFailed {
                    reason: "cycle detected in task hierarchy".to_string(),
                });
            }
            if !self.entry_exists(&current)? {
                break;
            }
            let entry = self.read_entry(&current)?;
            let next = entry.parent_task_id.clone();
            chain.push(entry);
            cursor = next;
        }
        Ok(chain)
    }

    fn claim(&self, task_id: &str, agent_path: &str) -> Result<TaskEntry, ToolError> {
        let _permit = self.acquire_transaction()?;
        let lock_relative = self.lock_relative(task_id)?;
        let root = self.create_root()?;
        root.create_dir_all(&self.group_relative()?)
            .map_err(|error| private_io_error(root.path(), &error))?;
        let guard = LockGuard::acquire(root, lock_relative)?;

        if !self.entry_exists_in(guard.root(), task_id)? {
            drop(guard);
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{task_id}' not found"),
            });
        }
        let mut entry = match self.read_entry_in(guard.root(), task_id) {
            Ok(entry) => entry,
            Err(err) => {
                drop(guard);
                return Err(err);
            }
        };
        if let Some(existing) = &entry.assigned_agent {
            let reason = format!("task '{task_id}' already claimed by {existing}");
            drop(guard);
            return Err(ToolError::ExecutionFailed { reason });
        }
        entry.assigned_agent = Some(agent_path.to_string());
        entry.updated_at = Utc::now();
        if let Err(err) = self.write_entry_atomic_in(guard.root(), &entry, true) {
            drop(guard);
            return Err(err);
        }
        guard.release()?;
        Ok(entry)
    }

    fn create_group(&self, slug: &str) -> Result<(), ToolError> {
        let _permit = self.acquire_transaction()?;
        let validated = validate_slug(slug)?;
        let root = self.create_root()?;
        root.create_dir_all(Path::new(validated))
            .map_err(|error| private_io_error(&root.display_path(Path::new(validated)), &error))
    }

    fn list_groups(&self) -> Result<Vec<String>, ToolError> {
        let _permit = self.acquire_transaction()?;
        self.list_groups_on_disk()
    }
}

#[cfg(test)]
#[path = "disk/security_tests.rs"]
mod security_tests;

#[cfg(test)]
#[path = "disk/admission_tests.rs"]
mod admission_tests;

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
#[path = "disk/tests.rs"]
mod tests;
