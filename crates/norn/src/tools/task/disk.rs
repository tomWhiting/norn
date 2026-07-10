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
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::fs::{self, OpenOptions};

use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use super::types::{TaskEntry, TaskStatus, TaskStore};
use crate::error::ToolError;
use crate::util::{PrivateEntryKind, PrivateRoot, validate_private_component};

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

    fn create_root(&self) -> Result<PrivateRoot, ToolError> {
        PrivateRoot::create(&self.root_dir)
            .map_err(|error| private_io_error(&self.root_dir, &error))
    }

    fn open_root(&self) -> Result<PrivateRoot, ToolError> {
        PrivateRoot::open(&self.root_dir).map_err(|error| private_io_error(&self.root_dir, &error))
    }

    fn entry_exists(&self, task_id: &str) -> Result<bool, ToolError> {
        let root = match PrivateRoot::open(&self.root_dir) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(private_io_error(&self.root_dir, &error)),
        };
        self.entry_exists_in(&root, task_id)
    }

    fn entry_exists_in(&self, root: &PrivateRoot, task_id: &str) -> Result<bool, ToolError> {
        let relative = self.task_relative(task_id)?;
        root.regular_file_exists(&relative)
            .map_err(|error| private_io_error(&root.display_path(&relative), &error))
    }

    fn read_entry(&self, task_id: &str) -> Result<TaskEntry, ToolError> {
        let root = self.open_root()?;
        self.read_entry_in(&root, task_id)
    }

    fn read_entry_in(&self, root: &PrivateRoot, task_id: &str) -> Result<TaskEntry, ToolError> {
        let relative = self.task_relative(task_id)?;
        let mut file = root
            .open_read(&relative)
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!("failed to read task '{task_id}': {error}"),
            })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!("failed to read task '{task_id}': {error}"),
            })?;
        serde_json::from_slice(&bytes).map_err(|err| ToolError::ExecutionFailed {
            reason: format!("failed to deserialise task '{task_id}': {err}"),
        })
    }

    fn write_entry_atomic(&self, entry: &TaskEntry, replace: bool) -> Result<(), ToolError> {
        validate_task_id(&entry.id)?;
        let root = self.create_root()?;
        self.write_entry_atomic_in(&root, entry, replace)
    }

    fn write_entry_atomic_in(
        &self,
        root: &PrivateRoot,
        entry: &TaskEntry,
        replace: bool,
    ) -> Result<(), ToolError> {
        validate_task_id(&entry.id)?;
        let group = self.group_relative()?;
        root.create_dir_all(&group)
            .map_err(|error| private_io_error(&root.display_path(&group), &error))?;
        let final_path = group.join(format!("{}.json", entry.id));
        let tmp_path = group.join(format!("{}.tmp.{}", entry.id, Uuid::new_v4()));

        if let Err(err) = write_json_atomic(root, &tmp_path, &final_path, entry, replace) {
            if let Err(cleanup_error) = root.remove_file(&tmp_path)
                && cleanup_error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    path = %root.display_path(&tmp_path).display(),
                    error = %cleanup_error,
                    "failed to clean up task atomic-write temp file",
                );
            }
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
        let group = self.group_relative()?;
        let root = match PrivateRoot::open(&self.root_dir) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(private_io_error(&self.root_dir, &error)),
        };
        let read_dir = match root.read_dir(&group) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(private_io_error(&root.display_path(&group), &error)),
        };
        let mut out = Vec::new();
        for entry in read_dir {
            let Some(name) = entry.name.to_str() else {
                return Err(ToolError::ExecutionFailed {
                    reason: "task directory contains a non-UTF-8 entry".to_owned(),
                });
            };
            if !is_task_file_name(name) {
                continue;
            }
            let relative = group.join(name);
            if entry.kind != PrivateEntryKind::File {
                return Err(ToolError::ExecutionFailed {
                    reason: format!(
                        "task entry '{}' is not a private regular file",
                        root.display_path(&relative).display(),
                    ),
                });
            }
            let mut file = root
                .open_read(&relative)
                .map_err(|error| private_io_error(&root.display_path(&relative), &error))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| private_io_error(&root.display_path(&relative), &error))?;
            let parsed: TaskEntry =
                serde_json::from_slice(&bytes).map_err(|err| ToolError::ExecutionFailed {
                    reason: format!(
                        "failed to deserialise task file '{}': {err}",
                        root.display_path(&relative).display(),
                    ),
                })?;
            validate_task_id(&parsed.id)?;
            out.push(parsed);
        }
        Ok(out)
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
    ToolError::ExecutionFailed {
        reason: format!(
            "private task storage failed at '{}': {error}",
            path.display()
        ),
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

fn write_json_atomic(
    root: &PrivateRoot,
    tmp_path: &Path,
    final_path: &Path,
    entry: &TaskEntry,
    replace: bool,
) -> Result<(), ToolError> {
    let mut file = root
        .create_new(tmp_path)
        .map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to open task tmp file '{}': {err}",
                root.display_path(tmp_path).display(),
            ),
        })?;
    let bytes = serde_json::to_vec_pretty(entry).map_err(|err| ToolError::ExecutionFailed {
        reason: format!("failed to serialise task '{}': {err}", entry.id),
    })?;
    file.write_all(&bytes)
        .map_err(|err| ToolError::ExecutionFailed {
            reason: format!(
                "failed to write task tmp file '{}': {err}",
                root.display_path(tmp_path).display(),
            ),
        })?;
    file.flush().map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to flush task tmp file '{}': {err}",
            root.display_path(tmp_path).display(),
        ),
    })?;
    file.sync_all().map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to fsync task tmp file '{}': {err}",
            root.display_path(tmp_path).display(),
        ),
    })?;
    drop(file);
    let publish = if replace {
        root.rename(tmp_path, final_path)
    } else {
        root.publish_new(tmp_path, final_path)
    };
    publish.map_err(|err| ToolError::ExecutionFailed {
        reason: format!(
            "failed to publish '{}' as '{}': {err}",
            root.display_path(tmp_path).display(),
            root.display_path(final_path).display(),
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
    root: PrivateRoot,
    relative: PathBuf,
    released: bool,
}

impl LockGuard {
    fn acquire(root: PrivateRoot, relative: PathBuf) -> Result<Self, ToolError> {
        match root.create_new(&relative) {
            Ok(_file) => Ok(Self {
                root,
                relative,
                released: false,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ToolError::ExecutionFailed {
                    reason: format!(
                        "task claim contended: lock file '{}' already exists",
                        root.display_path(&relative).display(),
                    ),
                })
            }
            Err(err) => Err(ToolError::ExecutionFailed {
                reason: format!(
                    "failed to acquire claim lock '{}': {err}",
                    root.display_path(&relative).display(),
                ),
            }),
        }
    }

    fn release(mut self) -> Result<(), ToolError> {
        self.root
            .remove_file(&self.relative)
            .map_err(|error| private_io_error(&self.root.display_path(&self.relative), &error))?;
        self.released = true;
        Ok(())
    }

    fn root(&self) -> &PrivateRoot {
        &self.root
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released
            && let Err(error) = self.root.remove_file(&self.relative)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.root.display_path(&self.relative).display(),
                %error,
                "failed to release task claim lock",
            );
        }
    }
}

impl TaskStore for DiskTaskStore {
    fn create(&self, entry: TaskEntry) -> Result<(), ToolError> {
        self.write_entry_atomic(&entry, false)
    }

    fn get(&self, id: &str) -> Option<TaskEntry> {
        match self.entry_exists(id) {
            Ok(false) => return None,
            Ok(true) => {}
            Err(err) => {
                tracing::warn!(task_id = %id, error = ?err, "DiskTaskStore::get path check failed");
                return None;
            }
        }
        match self.read_entry(id) {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(task_id = %id, error = ?err, "DiskTaskStore::get read failed");
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
        if !self.entry_exists(id)? {
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
        self.write_entry_atomic(&entry, true)?;
        Ok(entry)
    }

    fn complete(&self, id: &str) -> Result<TaskEntry, ToolError> {
        self.update(id, Some(TaskStatus::Completed), None, None, None)
    }

    fn create_subtask(&self, parent_id: &str, mut entry: TaskEntry) -> Result<(), ToolError> {
        if !self.entry_exists(parent_id)? {
            return Err(ToolError::ExecutionFailed {
                reason: format!("parent task '{parent_id}' not found"),
            });
        }
        entry.parent_task_id = Some(parent_id.to_string());
        self.write_entry_atomic(&entry, false)
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
        let validated = validate_slug(slug)?;
        let root = self.create_root()?;
        root.create_dir_all(Path::new(validated))
            .map_err(|error| private_io_error(&root.display_path(Path::new(validated)), &error))
    }

    fn list_groups(&self) -> Vec<String> {
        let root = match PrivateRoot::open(&self.root_dir) {
            Ok(root) => root,
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
        let read_dir = match root.read_dir(Path::new("")) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(
                    root = %self.root_dir.display(),
                    error = ?err,
                    "DiskTaskStore::list_groups failed to read root directory",
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for entry in read_dir {
            let Ok(name) = entry.name.into_string() else {
                tracing::warn!("ignoring non-UTF-8 task group entry");
                continue;
            };
            if validate_slug(&name).is_err() {
                continue;
            }
            if entry.kind == PrivateEntryKind::Directory {
                out.push(name);
            } else {
                tracing::warn!(group = %name, "ignoring non-directory task group entry");
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    fn entry(id: &str) -> TaskEntry {
        let now = Utc::now();
        TaskEntry {
            id: id.to_owned(),
            description: "private task".to_owned(),
            status: TaskStatus::Pending,
            depends_on: Vec::new(),
            metadata: Value::Null,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            assigned_agent: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn task_files_are_private_and_hostile_ids_cannot_escape()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let container = tempfile::tempdir()?;
        let root_path = container.path().join("tasks");
        let store = DiskTaskStore::new(root_path.clone(), "group".to_owned());
        store.create(entry("_legal-task"))?;

        let mode = |path: &Path| -> std::io::Result<u32> {
            Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
        };
        assert_eq!(mode(&root_path)?, 0o700);
        assert_eq!(mode(&root_path.join("group"))?, 0o700);
        assert_eq!(mode(&root_path.join("group/_legal-task.json"))?, 0o600);

        std::fs::set_permissions(&root_path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(
            root_path.join("group"),
            std::fs::Permissions::from_mode(0o755),
        )?;
        std::fs::set_permissions(
            root_path.join("group/_legal-task.json"),
            std::fs::Permissions::from_mode(0o644),
        )?;
        assert_eq!(
            store
                .get("_legal-task")
                .ok_or("task disappeared during read-only reopen")?
                .id,
            "_legal-task",
        );
        assert_eq!(mode(&root_path)?, 0o700);
        assert_eq!(mode(&root_path.join("group"))?, 0o700);
        assert_eq!(mode(&root_path.join("group/_legal-task.json"))?, 0o600);

        assert!(store.create(entry("../outside")).is_err());
        assert!(!container.path().join("outside.json").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn task_reads_and_rewrites_reject_links_without_touching_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let container = tempfile::tempdir()?;
        let root_path = container.path().join("tasks");
        let group = root_path.join("group");
        std::fs::create_dir_all(&group)?;
        let target = container.path().join("outside.json");
        std::fs::write(&target, serde_json::to_vec(&entry("outside"))?)?;
        symlink(&target, group.join("linked.json"))?;
        let store = DiskTaskStore::new(root_path, "group".to_owned());

        assert!(
            store
                .update("linked", None, Some("changed".to_owned()), None, None)
                .is_err()
        );
        let persisted: TaskEntry = serde_json::from_slice(&std::fs::read(target)?)?;
        assert_eq!(persisted.id, "outside");
        assert_eq!(persisted.description, "private task");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn claim_transaction_remains_on_locked_root_after_root_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let container = tempfile::tempdir()?;
        let root_path = container.path().join("tasks");
        let parked = container.path().join("parked");
        let store = DiskTaskStore::new(root_path.clone(), "group".to_owned());
        store.create(entry("task"))?;

        let root = store.create_root()?;
        root.create_dir_all(&store.group_relative()?)?;
        let guard = LockGuard::acquire(root, store.lock_relative("task")?)?;
        std::fs::rename(&root_path, &parked)?;

        let replacement = DiskTaskStore::new(root_path.clone(), "group".to_owned());
        let mut replacement_entry = entry("task");
        replacement_entry.description = "replacement".to_owned();
        replacement.create(replacement_entry)?;

        let mut claimed = store.read_entry_in(guard.root(), "task")?;
        claimed.assigned_agent = Some("agent/pinned".to_owned());
        store.write_entry_atomic_in(guard.root(), &claimed, true)?;
        guard.release()?;

        let parked_entry: TaskEntry =
            serde_json::from_slice(&std::fs::read(parked.join("group/task.json"))?)?;
        let replacement_entry = replacement
            .get("task")
            .ok_or("replacement task unexpectedly missing")?;
        assert_eq!(parked_entry.assigned_agent.as_deref(), Some("agent/pinned"));
        assert_eq!(replacement_entry.description, "replacement");
        assert_eq!(replacement_entry.assigned_agent, None);
        assert!(!root_path.join("group/task.lock").exists());
        assert!(!parked.join("group/task.lock").exists());
        Ok(())
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
