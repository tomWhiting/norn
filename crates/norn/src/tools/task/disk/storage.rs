//! Permit-assuming private filesystem primitives for disk task storage.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::{DiskTaskStore, is_task_file_name, private_io_error, validate_slug, validate_task_id};
use crate::error::ToolError;
use crate::tools::task::TaskEntry;
use crate::util::{PrivateEntryKind, PrivateRoot};

impl DiskTaskStore {
    pub(super) fn create_root(&self) -> Result<PrivateRoot, ToolError> {
        PrivateRoot::create(&self.root_dir)
            .map_err(|error| private_io_error(&self.root_dir, &error))
    }

    pub(super) fn open_root(&self) -> Result<PrivateRoot, ToolError> {
        PrivateRoot::open(&self.root_dir).map_err(|error| private_io_error(&self.root_dir, &error))
    }

    pub(super) fn entry_exists(&self, task_id: &str) -> Result<bool, ToolError> {
        let root = match PrivateRoot::open(&self.root_dir) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(private_io_error(&self.root_dir, &error)),
        };
        self.entry_exists_in(&root, task_id)
    }

    pub(super) fn entry_exists_in(
        &self,
        root: &PrivateRoot,
        task_id: &str,
    ) -> Result<bool, ToolError> {
        let relative = self.task_relative(task_id)?;
        root.regular_file_exists(&relative)
            .map_err(|error| private_io_error(&root.display_path(&relative), &error))
    }

    pub(super) fn read_entry(&self, task_id: &str) -> Result<TaskEntry, ToolError> {
        let root = self.open_root()?;
        self.read_entry_in(&root, task_id)
    }

    pub(super) fn read_entry_in(
        &self,
        root: &PrivateRoot,
        task_id: &str,
    ) -> Result<TaskEntry, ToolError> {
        let relative = self.task_relative(task_id)?;
        let mut file = root
            .open_read(&relative)
            .map_err(|error| private_io_error(&root.display_path(&relative), &error))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| private_io_error(&root.display_path(&relative), &error))?;
        serde_json::from_slice(&bytes).map_err(|err| ToolError::ExecutionFailed {
            reason: format!("failed to deserialise task '{task_id}': {err}"),
        })
    }

    pub(super) fn write_entry_atomic(
        &self,
        entry: &TaskEntry,
        replace: bool,
    ) -> Result<(), ToolError> {
        validate_task_id(&entry.id)?;
        let root = self.create_root()?;
        self.write_entry_atomic_in(&root, entry, replace)
    }

    pub(super) fn write_entry_atomic_in(
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
    /// claim guards do not show up in
    /// [`TaskStore::list`](crate::tools::task::TaskStore::list) /
    /// [`TaskStore::children`](crate::tools::task::TaskStore::children).
    pub(super) fn entries_in_group(&self) -> Result<Vec<TaskEntry>, ToolError> {
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

    pub(super) fn list_groups_on_disk(&self) -> Result<Vec<String>, ToolError> {
        let root = match PrivateRoot::open(&self.root_dir) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(private_io_error(&self.root_dir, &error)),
        };
        let read_dir = match root.read_dir(Path::new("")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(private_io_error(&self.root_dir, &error)),
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
        Ok(out)
    }
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
        .map_err(|error| private_io_error(&root.display_path(tmp_path), &error))?;
    let bytes = serde_json::to_vec_pretty(entry).map_err(|err| ToolError::ExecutionFailed {
        reason: format!("failed to serialise task '{}': {err}", entry.id),
    })?;
    file.write_all(&bytes)
        .map_err(|error| private_io_error(&root.display_path(tmp_path), &error))?;
    file.flush()
        .map_err(|error| private_io_error(&root.display_path(tmp_path), &error))?;
    file.sync_all()
        .map_err(|error| private_io_error(&root.display_path(tmp_path), &error))?;
    drop(file);
    let publish = if replace {
        root.rename(tmp_path, final_path)
    } else {
        root.publish_new(tmp_path, final_path)
    };
    publish.map_err(|error| private_io_error(&root.display_path(final_path), &error))?;
    Ok(())
}

/// RAII guard for a `.lock` file created via `O_CREAT|O_EXCL`.
///
/// Dropping the guard removes the lock file. The guard is taken by
/// value into [`LockGuard::release`] so callers can release explicitly
/// after a successful claim; on any error path the implicit `Drop`
/// still cleans up.
pub(super) struct LockGuard {
    root: PrivateRoot,
    relative: PathBuf,
    released: bool,
}

impl LockGuard {
    pub(super) fn acquire(root: PrivateRoot, relative: PathBuf) -> Result<Self, ToolError> {
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

    pub(super) fn release(mut self) -> Result<(), ToolError> {
        self.root
            .remove_file(&self.relative)
            .map_err(|error| private_io_error(&self.root.display_path(&self.relative), &error))?;
        self.released = true;
        Ok(())
    }

    pub(super) fn root(&self) -> &PrivateRoot {
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
