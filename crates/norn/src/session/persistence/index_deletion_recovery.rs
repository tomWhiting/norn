//! Validation and recovery for durable session-deletion journals.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::util::{PrivateEntryKind, PrivateRoot};

use super::super::io::{ensure_session_id_not_reserved, session_file_relative};
use super::super::strict::validate_index_entries;
use super::super::types::{SessionIndexEntry, SessionPersistError};
use super::deletion::{
    DELETION_PREFIX, DELETION_SUFFIX, DELETION_TEMP_SUFFIX, DELETION_VERSION, DeletionJournal,
    deletion_conflict, descendant_ids, finish_committed_deletion, remove_deletion_journal,
    remove_owned_file,
};

pub(super) fn recover_pending_deletions(root: &PrivateRoot) -> Result<(), SessionPersistError> {
    let (journals, temporary) = pending_deletion_artifacts(root)?;
    for path in temporary {
        remove_owned_file(root, &path)?;
    }
    if journals.is_empty() {
        return Ok(());
    }
    let entries = super::codec::read_index_in(root)?;
    for path in journals {
        recover_deletion(root, &entries, &path)?;
    }
    Ok(())
}

fn pending_deletion_artifacts(
    root: &PrivateRoot,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), SessionPersistError> {
    let mut journals = Vec::new();
    let mut temporary = Vec::new();
    for entry in root.read_dir(Path::new(""))? {
        let target = if deletion_id(&entry.name, DELETION_SUFFIX).is_some() {
            &mut journals
        } else if deletion_id(&entry.name, DELETION_TEMP_SUFFIX).is_some() {
            &mut temporary
        } else {
            continue;
        };
        if entry.kind != PrivateEntryKind::File {
            return Err(deletion_conflict(
                "unknown",
                "an owned deletion-journal name is not a regular file",
            ));
        }
        target.push(PathBuf::from(entry.name));
    }
    journals.sort();
    temporary.sort();
    Ok((journals, temporary))
}

fn recover_deletion(
    root: &PrivateRoot,
    entries: &[SessionIndexEntry],
    journal_path: &Path,
) -> Result<(), SessionPersistError> {
    let transaction_id = deletion_id(
        journal_path
            .file_name()
            .ok_or_else(|| std::io::Error::other("deletion journal has no file name"))?,
        DELETION_SUFFIX,
    )
    .ok_or_else(|| std::io::Error::other("deletion journal name is not owned"))?;
    let journal = read_deletion_journal(root, journal_path, &transaction_id)?;
    validate_deletion_journal(&journal)?;
    let active_by_id = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let present = journal
        .removed
        .iter()
        .filter_map(|removed| active_by_id.get(removed.id.as_str()).copied())
        .collect::<Vec<_>>();
    if present.len() == journal.removed.len() {
        if present
            .iter()
            .zip(&journal.removed)
            .any(|(current, removed)| *current != removed)
        {
            return Err(deletion_conflict(
                &transaction_id,
                "an indexed row differs from the pre-delete snapshot",
            ));
        }
        return remove_deletion_journal(root, journal_path);
    }
    if !present.is_empty() {
        return Err(deletion_conflict(
            &transaction_id,
            "only part of the atomic index deletion is visible",
        ));
    }
    ensure_paths_are_unclaimed(entries, &journal, &transaction_id)?;
    validate_committed_snapshot(entries, &journal, &transaction_id)?;
    finish_committed_deletion(root, &journal, journal_path)
}

fn validate_deletion_journal(journal: &DeletionJournal) -> Result<(), SessionPersistError> {
    if journal.norn_session_deletion != DELETION_VERSION || journal.removed.is_empty() {
        return Err(deletion_conflict(
            &journal.transaction_id,
            "the deletion journal version or entry set is invalid",
        ));
    }
    ensure_session_id_not_reserved(&journal.target_id)?;
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for entry in &journal.removed {
        let path = session_file_relative(entry)?;
        if !ids.insert(entry.id.clone()) || !paths.insert(path) {
            return Err(deletion_conflict(
                &journal.transaction_id,
                "the deletion journal contains duplicate identities or paths",
            ));
        }
    }
    let target = journal
        .removed
        .iter()
        .find(|entry| entry.id == journal.target_id)
        .ok_or_else(|| {
            deletion_conflict(
                &journal.transaction_id,
                "the deletion target is absent from its removed entry set",
            )
        })?;
    if target
        .parent_id
        .as_deref()
        .is_some_and(|parent_id| ids.contains(parent_id))
        || descendant_ids(&journal.removed, &journal.target_id) != ids
    {
        return Err(deletion_conflict(
            &journal.transaction_id,
            "the removed entry set is not one connected target subtree",
        ));
    }
    match journal.root_artifact_directory.as_deref() {
        Some(directory) if directory == journal.target_id && target.rel_path.is_none() => {
            ensure_session_id_not_reserved(directory)?;
        }
        None if target.rel_path.is_some() => {}
        _ => {
            return Err(deletion_conflict(
                &journal.transaction_id,
                "the deletion artifact directory does not match the target row",
            ));
        }
    }
    Ok(())
}

fn validate_committed_snapshot(
    entries: &[SessionIndexEntry],
    journal: &DeletionJournal,
    transaction_id: &str,
) -> Result<(), SessionPersistError> {
    let mut reconstructed = Vec::with_capacity(entries.len().saturating_add(journal.removed.len()));
    reconstructed.extend_from_slice(entries);
    reconstructed.extend(journal.removed.iter().cloned());
    if let Err(error) = validate_index_entries(&reconstructed) {
        tracing::debug!(
            %transaction_id,
            %error,
            "deletion recovery rejected a reconstructed strict index",
        );
        return Err(deletion_conflict(
            transaction_id,
            "the deletion journal cannot reconstruct a valid strict index",
        ));
    }
    let recorded_ids = journal
        .removed
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<HashSet<_>>();
    if descendant_ids(&reconstructed, &journal.target_id) != recorded_ids {
        return Err(deletion_conflict(
            transaction_id,
            "the deletion journal is not the complete target subtree",
        ));
    }
    Ok(())
}

fn ensure_paths_are_unclaimed(
    entries: &[SessionIndexEntry],
    journal: &DeletionJournal,
    transaction_id: &str,
) -> Result<(), SessionPersistError> {
    let removed_paths = journal
        .removed
        .iter()
        .map(session_file_relative)
        .collect::<Result<HashSet<_>, _>>()?;
    for entry in entries {
        if removed_paths.contains(&session_file_relative(entry)?) {
            return Err(deletion_conflict(
                transaction_id,
                "an active index row has reclaimed a pending deletion path",
            ));
        }
    }
    Ok(())
}

fn read_deletion_journal(
    root: &PrivateRoot,
    path: &Path,
    transaction_id: &str,
) -> Result<DeletionJournal, SessionPersistError> {
    let journal: DeletionJournal = serde_json::from_reader(BufReader::new(root.open_read(path)?))?;
    if journal.transaction_id != transaction_id {
        return Err(deletion_conflict(
            transaction_id,
            "the journal transaction id does not match its owned file name",
        ));
    }
    Ok(journal)
}

fn deletion_id(name: &OsStr, suffix: &str) -> Option<String> {
    let id = name
        .to_str()?
        .strip_prefix(DELETION_PREFIX)?
        .strip_suffix(suffix)?;
    let parsed = Uuid::parse_str(id).ok()?;
    if parsed.to_string() != id {
        return None;
    }
    Some(id.to_owned())
}
