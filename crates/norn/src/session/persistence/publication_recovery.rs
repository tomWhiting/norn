use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::util::{PrivateEntryKind, PrivateRoot};

use super::SessionPersistError;
use super::names::{
    journal_id, journal_path, journal_temp_id, journal_temp_path, timeline_stage_id,
    timeline_stage_path,
};
use super::publication_conflict::conflict;

#[derive(Debug)]
pub(super) struct PendingPublications {
    pub(super) journals: Vec<PathBuf>,
    removed_orphans: bool,
}

impl PendingPublications {
    pub(super) fn sync_orphan_cleanup(
        &self,
        root: &PrivateRoot,
    ) -> Result<(), SessionPersistError> {
        if self.removed_orphans {
            root.sync_dir(Path::new(""))?;
        }
        Ok(())
    }
}

pub(super) fn inventory_and_remove_orphans(
    root: &PrivateRoot,
) -> Result<PendingPublications, SessionPersistError> {
    let mut journal_ids = BTreeSet::new();
    let mut journal_temps = Vec::new();
    let mut timeline_stages = Vec::new();

    for entry in root.read_dir(Path::new(""))? {
        let artifact = if let Some(id) = journal_id(&entry.name) {
            journal_ids.insert(id.clone());
            Some((id, ArtifactKind::Journal))
        } else if let Some(id) = journal_temp_id(&entry.name) {
            journal_temps.push(id.clone());
            Some((id, ArtifactKind::JournalTemporary))
        } else if let Some(id) = timeline_stage_id(&entry.name) {
            timeline_stages.push(id.clone());
            Some((id, ArtifactKind::TimelineStage))
        } else {
            None
        };
        if let Some((id, kind)) = artifact
            && entry.kind != PrivateEntryKind::File
        {
            return Err(conflict(&id, kind.non_file_reason()));
        }
    }

    let mut removed_orphans = false;
    for id in journal_temps {
        if !journal_ids.contains(&id) {
            remove_owned(root, &journal_temp_path(&id))?;
            removed_orphans = true;
        }
    }
    for id in timeline_stages {
        if !journal_ids.contains(&id) {
            remove_owned(root, &timeline_stage_path(&id))?;
            removed_orphans = true;
        }
    }
    let journals = journal_ids
        .iter()
        .map(|id| journal_path(id))
        .collect::<Vec<_>>();
    Ok(PendingPublications {
        journals,
        removed_orphans,
    })
}

pub(super) fn allocate_transaction_id(root: &PrivateRoot) -> Result<String, SessionPersistError> {
    loop {
        let transaction_id = Uuid::new_v4().hyphenated().to_string();
        if !root.regular_file_exists(&journal_path(&transaction_id))?
            && !root.regular_file_exists(&journal_temp_path(&transaction_id))?
            && !root.regular_file_exists(&timeline_stage_path(&transaction_id))?
        {
            return Ok(transaction_id);
        }
    }
}

pub(super) fn remove_owned_after_failure(root: &PrivateRoot, path: &Path) {
    if let Err(error) = root.remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %root.display_path(path).display(), %error, "failed to remove owned publication temporary file");
    }
}

fn remove_owned(root: &PrivateRoot, path: &Path) -> Result<(), SessionPersistError> {
    match root.remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Copy, Debug)]
enum ArtifactKind {
    Journal,
    JournalTemporary,
    TimelineStage,
}

impl ArtifactKind {
    const fn non_file_reason(self) -> &'static str {
        match self {
            Self::Journal => "the owned publication-journal name is not a regular file",
            Self::JournalTemporary => {
                "the owned publication journal-temporary name is not a regular file"
            }
            Self::TimelineStage => {
                "the owned publication timeline-stage name is not a regular file"
            }
        }
    }
}
