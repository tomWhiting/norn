//! Crash-recoverable deletion of complete session subtrees.

use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::util::PrivateRoot;

use super::super::io::session_file_relative;
use super::super::timeline_lock::TimelineLockGuard;
use super::super::types::{SessionIndexEntry, SessionPersistError};

pub(super) const DELETION_VERSION: u32 = 1;
pub(super) const DELETION_PREFIX: &str = ".session-deletion.";
pub(super) const DELETION_SUFFIX: &str = ".json";
pub(super) const DELETION_TEMP_SUFFIX: &str = ".json.tmp";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeletionJournal {
    pub(super) norn_session_deletion: u32,
    pub(super) transaction_id: String,
    pub(super) target_id: String,
    pub(super) removed: Vec<SessionIndexEntry>,
    pub(super) root_artifact_directory: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeleteCheckpoint {
    JournalPublished,
    IndexPublished,
}

pub(crate) fn delete_session_transaction(
    data_dir: &Path,
    id_or_name: &str,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    delete_session_transaction_inner(data_dir, id_or_name, lock_deadline, &mut |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn delete_session_transaction_with_hook(
    data_dir: &Path,
    id_or_name: &str,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(DeleteCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    delete_session_transaction_inner(data_dir, id_or_name, lock_deadline, checkpoint)
}

fn delete_session_transaction_inner(
    data_dir: &Path,
    id_or_name: &str,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(DeleteCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let index_lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let mut entries = super::codec::read_index_in(index_lock.root())?;
    let entry = super::resolve::resolve_in_entries(entries.clone(), id_or_name)?;
    super::super::io::ensure_session_id_path_safe(&entry.id)?;
    let removed_ids = descendant_ids(&entries, &entry.id);
    let removed = entries
        .iter()
        .filter(|candidate| removed_ids.contains(candidate.id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let journal = DeletionJournal {
        norn_session_deletion: DELETION_VERSION,
        transaction_id: Uuid::new_v4().to_string(),
        target_id: entry.id.clone(),
        removed,
        root_artifact_directory: entry.rel_path.is_none().then(|| entry.id.clone()),
    };
    let journal_path = write_deletion_journal(index_lock.root(), &journal)?;
    checkpoint(DeleteCheckpoint::JournalPublished)?;
    entries.retain(|candidate| !removed_ids.contains(candidate.id.as_str()));
    super::codec::write_index_atomic_in(index_lock.root(), &entries)?;
    checkpoint(DeleteCheckpoint::IndexPublished)?;
    finish_committed_deletion(index_lock.root(), &journal, &journal_path)?;
    Ok(entry)
}

pub(super) fn descendant_ids(entries: &[SessionIndexEntry], target_id: &str) -> HashSet<String> {
    let mut children = HashMap::<&str, Vec<&str>>::new();
    for entry in entries {
        if let Some(parent_id) = entry.parent_id.as_deref() {
            children.entry(parent_id).or_default().push(&entry.id);
        }
    }

    let mut removed = HashSet::new();
    let mut pending = vec![target_id];
    while let Some(id) = pending.pop() {
        if !removed.insert(id.to_owned()) {
            continue;
        }
        if let Some(child_ids) = children.get(id) {
            pending.extend(child_ids.iter().copied());
        }
    }
    removed
}

pub(super) fn finish_committed_deletion(
    root: &PrivateRoot,
    journal: &DeletionJournal,
    journal_path: &Path,
) -> Result<(), SessionPersistError> {
    cleanup_deleted_artifacts(root, journal)
        .and_then(|()| remove_deletion_journal(root, journal_path))
        .map_err(|source| committed_cleanup_error(journal, source))
}

fn cleanup_deleted_artifacts(
    root: &PrivateRoot,
    journal: &DeletionJournal,
) -> Result<(), SessionPersistError> {
    for entry in &journal.removed {
        let relative = session_file_relative(entry)?;
        let _timeline_lock = TimelineLockGuard::acquire_under(root, &relative)?;
        remove_timeline(root, entry)?;
    }
    if let Some(directory) = journal.root_artifact_directory.as_deref() {
        remove_root_artifacts(root, Path::new(directory))?;
    }
    Ok(())
}

fn remove_timeline(
    root: &PrivateRoot,
    entry: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    let relative = session_file_relative(entry)?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    match root.remove_file(&relative) {
        Ok(()) => sync_nearest_existing_directory(root, parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            sync_nearest_existing_directory(root, parent)
        }
        Err(error) => Err(removal_error(root, &relative, &error, "session file")),
    }
}

fn sync_nearest_existing_directory(
    root: &PrivateRoot,
    relative: &Path,
) -> Result<(), SessionPersistError> {
    let mut candidate = relative;
    loop {
        match root.sync_dir(candidate) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && !candidate.as_os_str().is_empty() =>
            {
                candidate = candidate.parent().unwrap_or_else(|| Path::new(""));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn remove_root_artifacts(root: &PrivateRoot, relative: &Path) -> Result<(), SessionPersistError> {
    match root.remove_dir_all(relative) {
        Ok(()) => root.sync_dir(Path::new("")).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            root.sync_dir(Path::new("")).map_err(Into::into)
        }
        Err(error) => Err(removal_error(
            root,
            relative,
            &error,
            "session artifact directory",
        )),
    }
}

fn write_deletion_journal(
    root: &PrivateRoot,
    journal: &DeletionJournal,
) -> Result<PathBuf, SessionPersistError> {
    let temporary = deletion_path(&journal.transaction_id, DELETION_TEMP_SUFFIX);
    let final_path = deletion_path(&journal.transaction_id, DELETION_SUFFIX);
    let result = (|| {
        let file = root.create_new(&temporary)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, journal)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let file = writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        root.publish_new(&temporary, &final_path)?;
        root.sync_dir(Path::new(""))?;
        Ok(final_path.clone())
    })();
    if result.is_err() {
        let _ = root.remove_file(&temporary);
    }
    result
}

pub(super) fn remove_deletion_journal(
    root: &PrivateRoot,
    path: &Path,
) -> Result<(), SessionPersistError> {
    remove_owned_file(root, path)?;
    root.sync_dir(Path::new(""))?;
    Ok(())
}

pub(super) fn remove_owned_file(
    root: &PrivateRoot,
    path: &Path,
) -> Result<(), SessionPersistError> {
    match root.remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn deletion_path(transaction_id: &str, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{DELETION_PREFIX}{transaction_id}{suffix}"))
}

pub(super) fn deletion_conflict(transaction_id: &str, reason: &'static str) -> SessionPersistError {
    SessionPersistError::DeletionConflict {
        transaction_id: transaction_id.to_owned(),
        reason,
    }
}

fn committed_cleanup_error(
    journal: &DeletionJournal,
    source: SessionPersistError,
) -> SessionPersistError {
    SessionPersistError::DeletionCleanupPending {
        id: journal.target_id.clone(),
        transaction_id: journal.transaction_id.clone(),
        source: Box::new(source),
    }
}

fn removal_error(
    root: &PrivateRoot,
    relative: &Path,
    error: &std::io::Error,
    subject: &str,
) -> SessionPersistError {
    let path = root.display_path(relative);
    if let Some(exhaustion) = crate::resource::classify_descriptor_error(
        error,
        "deleting a registered session artifact",
        Some(&path),
    ) {
        return SessionPersistError::DescriptorExhausted(Box::new(exhaustion));
    }
    SessionPersistError::Io(std::io::Error::new(
        error.kind(),
        format!("failed to delete {subject} {}: {error}", path.display()),
    ))
}
