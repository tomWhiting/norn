//! Disk-backed [`TaskStore`] implementation.
//!
//! [`DiskTaskStore`] persists tasks to `{root_dir}/{group-slug}/` as
//! individual `{task-id}.json` files. Writes are atomic (write to a
//! sibling `.tmp.{uuid}` file, `fsync`, then `rename` into place) so a
//! crash mid-write never leaves a partially-written task on disk.
//!
//! Mutual exclusion for [`TaskStore::claim`] uses POSIX
//! `O_CREAT|O_EXCL` lock files: a successful `create_new` open is the
//! cross-process atomic primitive. A [`LockGuard`] RAII type cleans the
//! lock file up on every exit path, including early returns and panics.
//!
//! The store is filesystem-only — no database, no in-memory caching —
//! so multiple processes pointing at the same root directory observe
//! the same task tree. The store is also session-agnostic: a task
//! group lives until explicitly deleted, so multi-session work and
//! cross-session handoffs all read and write the same files.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use super::types::{TaskEntry, TaskStatus, TaskStore};
use crate::error::ToolError;

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
        }
    }

    /// Return the validated path to the group directory.
    ///
    /// Validation happens here (not in `new`) so the constructor stays
    /// infallible and tests can spin up a store cheaply.
    fn group_dir(&self) -> Result<PathBuf, ToolError> {
        let slug = validate_slug(&self.group_slug)?;
        Ok(self.root_dir.join(slug))
    }

    fn task_path(&self, task_id: &str) -> Result<PathBuf, ToolError> {
        Ok(self.group_dir()?.join(format!("{task_id}.json")))
    }

    fn lock_path(&self, task_id: &str) -> Result<PathBuf, ToolError> {
        Ok(self.group_dir()?.join(format!("{task_id}.lock")))
    }

    fn read_entry(&self, task_id: &str) -> Result<TaskEntry, ToolError> {
        let path = self.task_path(task_id)?;
        let bytes = fs::read(&path).map_err(|err| ToolError::ExecutionFailed {
            reason: format!("failed to read task '{task_id}': {err}"),
        })?;
        serde_json::from_slice(&bytes).map_err(|err| ToolError::ExecutionFailed {
            reason: format!("failed to deserialise task '{task_id}': {err}"),
        })
    }

    fn write_entry_atomic(&self, entry: &TaskEntry) -> Result<(), ToolError> {
        let dir = self.group_dir()?;
        fs::create_dir_all(&dir).map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to create task group directory '{}': {err}",
                dir.display()
            ),
        })?;
        let final_path = dir.join(format!("{}.json", entry.id));
        let tmp_path = dir.join(format!("{}.tmp.{}", entry.id, Uuid::new_v4()));

        if let Err(err) = write_json_atomic(&tmp_path, &final_path, entry) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }

    /// Iterate over the persisted `.json` task entries in this group.
    ///
    /// Skips siblings like `.lock` and `.tmp.*` so partial writes and
    /// claim guards do not show up in [`TaskStore::list`] /
    /// [`TaskStore::children`].
    fn entries_in_group(&self) -> Result<Vec<TaskEntry>, ToolError> {
        let dir = self.group_dir()?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let read_dir = fs::read_dir(&dir).map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to read task group directory '{}': {err}",
                dir.display()
            ),
        })?;
        let mut out = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|err| ToolError::ExecutionFailed {
                reason: format!("failed to read task directory entry: {err}"),
            })?;
            let path = entry.path();
            if !is_task_file(&path) {
                continue;
            }
            let bytes = fs::read(&path).map_err(|err| ToolError::ExecutionFailed {
                reason: format!("failed to read task file '{}': {err}", path.display()),
            })?;
            let parsed: TaskEntry =
                serde_json::from_slice(&bytes).map_err(|err| ToolError::ExecutionFailed {
                    reason: format!(
                        "failed to deserialise task file '{}': {err}",
                        path.display()
                    ),
                })?;
            out.push(parsed);
        }
        Ok(out)
    }
}

fn is_task_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("json")
    )
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

fn write_json_atomic(
    tmp_path: &Path,
    final_path: &Path,
    entry: &TaskEntry,
) -> Result<(), ToolError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to open task tmp file '{}': {err}",
                tmp_path.display()
            ),
        })?;
    let bytes = serde_json::to_vec_pretty(entry).map_err(|err| ToolError::ExecutionFailed {
        reason: format!("failed to serialise task '{}': {err}", entry.id),
    })?;
    file.write_all(&bytes)
        .map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to write task tmp file '{}': {err}",
                tmp_path.display()
            ),
        })?;
    file.flush().map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to flush task tmp file '{}': {err}",
            tmp_path.display()
        ),
    })?;
    file.sync_all().map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to fsync task tmp file '{}': {err}",
            tmp_path.display()
        ),
    })?;
    drop(file);
    fs::rename(tmp_path, final_path).map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to rename '{}' over '{}': {err}",
            tmp_path.display(),
            final_path.display()
        ),
    })?;
    Ok(())
}

/// RAII guard for a `.lock` file created via `O_CREAT|O_EXCL`.
///
/// Dropping the guard removes the lock file. The guard is taken by
/// value into [`LockGuard::release`] so callers can release explicitly
/// after a successful claim; on any error path the implicit `Drop`
/// still cleans up.
struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    fn acquire(path: PathBuf) -> Result<Self, ToolError> {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_file) => Ok(Self {
                path,
                released: false,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ToolError::ExecutionFailed {
                    reason: format!(
                        "task claim contended: lock file '{}' already exists",
                        path.display()
                    ),
                })
            }
            Err(err) => Err(ToolError::ExecutionFailed {
                reason: format!("failed to acquire claim lock '{}': {err}", path.display()),
            }),
        }
    }

    fn release(mut self) {
        let _ = fs::remove_file(&self.path);
        self.released = true;
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl TaskStore for DiskTaskStore {
    fn create(&self, entry: TaskEntry) -> Result<(), ToolError> {
        let final_path = self.task_path(&entry.id)?;
        if final_path.exists() {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{}' already exists", entry.id),
            });
        }
        self.write_entry_atomic(&entry)
    }

    fn get(&self, id: &str) -> Option<TaskEntry> {
        let path = match self.task_path(id) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(task_id = %id, error = ?err, "DiskTaskStore::get resolve path failed");
                return None;
            }
        };
        if !path.exists() {
            return None;
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(task_id = %id, error = ?err, "DiskTaskStore::get read failed");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(task_id = %id, error = ?err, "DiskTaskStore::get deserialise failed");
                None
            }
        }
    }

    fn list(&self, filter: Option<TaskStatus>) -> Vec<TaskEntry> {
        let mut entries = match self.entries_in_group() {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    group = %self.group_slug,
                    error = ?err,
                    "DiskTaskStore::list failed to read group directory",
                );
                return Vec::new();
            }
        };
        if let Some(status) = filter {
            entries.retain(|e| e.status == status);
        }
        entries.sort_by_key(|e| e.created_at);
        entries
    }

    fn update(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        description: Option<String>,
        depends_on: Option<Vec<String>>,
        metadata: Option<Value>,
    ) -> Result<TaskEntry, ToolError> {
        let path = self.task_path(id)?;
        if !path.exists() {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{id}' not found"),
            });
        }
        let mut entry = self.read_entry(id)?;
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
        self.write_entry_atomic(&entry)?;
        Ok(entry)
    }

    fn complete(&self, id: &str) -> Result<TaskEntry, ToolError> {
        self.update(id, Some(TaskStatus::Completed), None, None, None)
    }

    fn create_subtask(&self, parent_id: &str, mut entry: TaskEntry) -> Result<(), ToolError> {
        let parent_path = self.task_path(parent_id)?;
        if !parent_path.exists() {
            return Err(ToolError::ExecutionFailed {
                reason: format!("parent task '{parent_id}' not found"),
            });
        }
        let child_path = self.task_path(&entry.id)?;
        if child_path.exists() {
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{}' already exists", entry.id),
            });
        }
        entry.parent_task_id = Some(parent_id.to_string());
        self.write_entry_atomic(&entry)
    }

    fn children(&self, parent_id: &str) -> Vec<TaskEntry> {
        let mut entries = match self.entries_in_group() {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    group = %self.group_slug,
                    parent = %parent_id,
                    error = ?err,
                    "DiskTaskStore::children failed to read group directory",
                );
                return Vec::new();
            }
        };
        entries.retain(|e| e.parent_task_id.as_deref() == Some(parent_id));
        entries.sort_by_key(|e| e.created_at);
        entries
    }

    fn ancestors(&self, task_id: &str) -> Result<Vec<TaskEntry>, ToolError> {
        let mut chain = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut cursor = Some(task_id.to_string());
        while let Some(current) = cursor {
            if !visited.insert(current.clone()) {
                return Err(ToolError::ExecutionFailed {
                    reason: "cycle detected in task hierarchy".to_string(),
                });
            }
            let path = self.task_path(&current)?;
            if !path.exists() {
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
        let lock_path = self.lock_path(task_id)?;
        let task_path = self.task_path(task_id)?;
        let guard = LockGuard::acquire(lock_path)?;

        if !task_path.exists() {
            drop(guard);
            return Err(ToolError::ExecutionFailed {
                reason: format!("task '{task_id}' not found"),
            });
        }
        let mut entry = match self.read_entry(task_id) {
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
        if let Err(err) = self.write_entry_atomic(&entry) {
            drop(guard);
            return Err(err);
        }
        guard.release();
        Ok(entry)
    }

    fn create_group(&self, slug: &str) -> Result<(), ToolError> {
        let validated = validate_slug(slug)?;
        let dir = self.root_dir.join(validated);
        fs::create_dir_all(&dir).map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to create task group directory '{}': {err}",
                dir.display()
            ),
        })?;
        Ok(())
    }

    fn list_groups(&self) -> Vec<String> {
        let read_dir = match fs::read_dir(&self.root_dir) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(err) => {
                tracing::warn!(
                    root = %self.root_dir.display(),
                    error = ?err,
                    "DiskTaskStore::list_groups failed to read root directory",
                );
                return Vec::new();
            }
        };
        let mut out: Vec<String> = read_dir
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| validate_slug(name).is_ok())
            .collect();
        out.sort();
        out
    }
}

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

    fn store(tmp: &tempfile::TempDir, slug: &str) -> DiskTaskStore {
        DiskTaskStore::new(tmp.path().to_path_buf(), slug.to_string())
    }

    #[test]
    fn create_get_list_update_complete_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "g1");

        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        store.create(entry("t2", TaskStatus::Pending)).unwrap();

        let got = store.get("t1").unwrap();
        assert_eq!(got.id, "t1");

        let all = store.list(None);
        assert_eq!(all.len(), 2);

        let updated = store
            .update("t1", Some(TaskStatus::InProgress), None, None, None)
            .unwrap();
        assert_eq!(updated.status, TaskStatus::InProgress);
        let in_progress = store.list(Some(TaskStatus::InProgress));
        assert_eq!(in_progress.len(), 1);
        assert_eq!(in_progress[0].id, "t1");

        let completed = store.complete("t1").unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        let still_two = store.list(None);
        assert_eq!(still_two.len(), 2);
    }

    #[test]
    fn lazy_directory_creation_does_not_touch_fs_on_construction() {
        let tmp = tempfile::tempdir().unwrap();
        let _store = store(&tmp, "lazy");
        assert!(
            !tmp.path().join("lazy").exists(),
            "construction must not mkdir the group directory",
        );
    }

    #[test]
    fn directory_created_on_first_write() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "first-write");
        store.create(entry("a", TaskStatus::Pending)).unwrap();
        assert!(tmp.path().join("first-write").exists());
        assert!(tmp.path().join("first-write").join("a.json").exists());
    }

    #[test]
    fn create_subtask_writes_parent_link_and_children_lists_them() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "hier");
        store.create(entry("root", TaskStatus::Pending)).unwrap();
        store
            .create_subtask("root", entry("c1", TaskStatus::Pending))
            .unwrap();
        store
            .create_subtask("root", entry("c2", TaskStatus::Pending))
            .unwrap();
        let kids = store.children("root");
        assert_eq!(kids.len(), 2);
        for kid in &kids {
            assert_eq!(kid.parent_task_id.as_deref(), Some("root"));
        }
    }

    #[test]
    fn three_level_hierarchy_on_disk_walks_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "ladder");
        store.create(entry("root", TaskStatus::Pending)).unwrap();
        store
            .create_subtask("root", entry("mid", TaskStatus::Pending))
            .unwrap();
        store
            .create_subtask("mid", entry("leaf", TaskStatus::Pending))
            .unwrap();
        assert_eq!(store.children("root").len(), 1);
        let chain = store.ancestors("leaf").unwrap();
        let ids: Vec<&str> = chain.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["leaf", "mid", "root"]);
    }

    #[test]
    fn create_subtask_missing_parent_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "orphan");
        let err = store
            .create_subtask("ghost", entry("child", TaskStatus::Pending))
            .expect_err("missing parent");
        let ToolError::ExecutionFailed { reason } = err else {
            panic!("expected ExecutionFailed");
        };
        assert!(reason.contains("ghost"), "{reason}");
    }

    #[test]
    fn first_claim_succeeds_writes_assigned_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "claims");
        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        let claimed = store.claim("t1", "root/worker-a").unwrap();
        assert_eq!(claimed.assigned_agent.as_deref(), Some("root/worker-a"));
        let on_disk = store.get("t1").unwrap();
        assert_eq!(on_disk.assigned_agent.as_deref(), Some("root/worker-a"));
    }

    #[test]
    fn second_claim_fails_with_already_claimed_and_removes_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "claims2");
        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        store.claim("t1", "root/worker-a").unwrap();
        let err = store
            .claim("t1", "root/worker-b")
            .expect_err("second claim must fail");
        let ToolError::ExecutionFailed { reason } = err else {
            panic!("expected ExecutionFailed");
        };
        assert!(reason.contains("already claimed"), "{reason}");
        let lock = tmp.path().join("claims2").join("t1.lock");
        assert!(!lock.exists(), "lock file must be removed after failure");
    }

    #[test]
    fn lock_removed_after_successful_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "claims3");
        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        store.claim("t1", "root/worker-a").unwrap();
        let lock = tmp.path().join("claims3").join("t1.lock");
        assert!(!lock.exists(), "lock file must be removed after success");
    }

    #[test]
    fn claim_contended_returns_contended_message() {
        // Pre-create the lock file to simulate a concurrent claim in progress.
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "contend");
        store.create(entry("t1", TaskStatus::Pending)).unwrap();

        let dir = tmp.path().join("contend");
        let lock = dir.join("t1.lock");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .unwrap();

        let err = store
            .claim("t1", "root/worker")
            .expect_err("contended claim must fail");
        let ToolError::ExecutionFailed { reason } = err else {
            panic!("expected ExecutionFailed");
        };
        assert!(reason.contains("contended"), "{reason}");
        // The pre-existing lock must NOT be removed by the failed claim —
        // it belongs to whatever else created it.
        assert!(lock.exists(), "external lock must not be removed");
        // Clean up so the tempdir Drop doesn't trip on stragglers.
        let _ = fs::remove_file(&lock);
    }

    #[test]
    fn create_group_and_list_groups_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "main");
        store.create_group("norn-agents-wiring").unwrap();
        store.create_group("implement-hooks").unwrap();
        // Idempotent: creating again is OK.
        store.create_group("implement-hooks").unwrap();
        let groups = store.list_groups();
        assert_eq!(groups, vec!["implement-hooks", "norn-agents-wiring"]);
    }

    #[test]
    fn list_groups_on_missing_root_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DiskTaskStore::new(tmp.path().join("does-not-exist"), "anything".to_string());
        assert!(store.list_groups().is_empty());
    }

    #[test]
    fn invalid_slugs_rejected() {
        assert!(validate_slug("has/slash").is_err());
        assert!(validate_slug("..").is_err());
        assert!(validate_slug("dot.dot").is_err());
        assert!(validate_slug("space here").is_err());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("ok-slug_1").is_ok());
    }

    #[test]
    fn create_group_rejects_invalid_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "main");
        assert!(store.create_group("has/slash").is_err());
        assert!(store.create_group("..").is_err());
    }

    #[test]
    fn list_skips_tmp_and_lock_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp, "noise");
        store.create(entry("t1", TaskStatus::Pending)).unwrap();
        // Drop stray .tmp and .lock files in the group dir.
        let dir = tmp.path().join("noise");
        fs::write(dir.join("garbage.tmp.abc"), b"junk").unwrap();
        fs::write(dir.join("garbage.lock"), b"").unwrap();
        let listed = store.list(None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "t1");
    }

    #[test]
    fn data_survives_dropping_and_reconstructing_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        {
            let store = DiskTaskStore::new(root.clone(), "persist".to_string());
            store.create(entry("t1", TaskStatus::InProgress)).unwrap();
            store
                .create_subtask("t1", entry("t1-child", TaskStatus::Pending))
                .unwrap();
        }
        let reopened = DiskTaskStore::new(root, "persist".to_string());
        let all = reopened.list(None);
        assert_eq!(all.len(), 2);
        let kids = reopened.children("t1");
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].id, "t1-child");
    }
}
