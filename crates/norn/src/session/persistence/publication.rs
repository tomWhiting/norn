//! Crash-recoverable publication of seeded strict session timelines.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::time::Duration;

use uuid::Uuid;

use crate::session::events::SessionEvent;
use crate::util::PrivateRoot;

use super::super::io::session_file_relative;
use super::super::strict::validate_index_entries;
use super::super::types::{SessionIndexEntry, SessionPersistError};
use super::codec;

#[path = "publication_names.rs"]
mod names;
#[path = "publication_audio.rs"]
mod publication_audio;
#[path = "publication_audio_links.rs"]
mod publication_audio_links;
#[path = "publication_conflict.rs"]
mod publication_conflict;
#[path = "publication_hash.rs"]
mod publication_hash;
#[path = "publication_journal.rs"]
mod publication_journal;
#[path = "publication_parent.rs"]
mod publication_parent;
#[path = "publication_recovery.rs"]
mod publication_recovery;
#[path = "publication_timeline.rs"]
mod publication_timeline;
#[path = "publication_timeline_error.rs"]
mod publication_timeline_error;
use names::{audio_stage_id, journal_id, journal_temp_id, timeline_stage_id, timeline_stage_path};
use publication_audio::{
    audio_stage_is_present, recover_audio_bundle, remove_verified_audio_stage, stage_audio_bundle,
    validate_source_generation,
};
use publication_conflict::{conflict, path_occupied};
use publication_journal::{
    AUDIO_PUBLICATION_VERSION, PublicationJournal, TIMELINE_PUBLICATION_VERSION, read_journal,
    validate_journal_metadata, write_journal,
};
use publication_parent::{
    ParentPrecondition, child_precondition, validate_parent_generation,
    validate_parent_precondition_shape,
};
use publication_recovery::{allocate_transaction_id, inventory_and_remove_orphans};
use publication_timeline::{
    TimelineFacts, apply_timeline_facts, inspect_if_present, inspect_timeline, write_timeline_stage,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationCheckpoint {
    AudioStaged,
    TimelineStaged,
    JournalPublished,
    AudioPublished,
    TimelinePublished,
    IndexPublished,
}

pub(super) fn is_publication_artifact_name(name: &OsStr) -> bool {
    journal_id(name).is_some()
        || journal_temp_id(name).is_some()
        || timeline_stage_id(name).is_some()
        || audio_stage_id(name).is_some()
}

pub(super) fn recover_pending_publications(root: &PrivateRoot) -> Result<(), SessionPersistError> {
    let pending = inventory_and_remove_orphans(root)?;
    pending.sync_orphan_cleanup(root)?;
    if pending.journals.is_empty() {
        return Ok(());
    }
    let mut entries = codec::read_index_in(root)?;
    for journal_path in pending.journals {
        recover_one(root, &journal_path, &mut entries, &mut |_| Ok(()))?;
    }
    let remaining = inventory_and_remove_orphans(root)?;
    remaining.sync_orphan_cleanup(root)?;
    Ok(())
}

pub(crate) fn publish_new_session(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    publish_new_session_with_hook(data_dir, entry, events, lock_deadline, &mut |_| Ok(()))
}

pub(crate) fn publish_new_child_session(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    parent_generation: Uuid,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let parent = child_precondition(entry, parent_generation)?;
    publish_with_precondition(
        data_dir,
        entry,
        events,
        Some(parent),
        None,
        lock_deadline,
        &mut |_| Ok(()),
    )
}

pub(crate) fn publish_new_fork_session(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    source: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    publish_new_fork_session_with_hook(data_dir, entry, events, source, lock_deadline, &mut |_| {
        Ok(())
    })
}

fn publish_new_fork_session_with_hook(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    source: &SessionIndexEntry,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    publish_with_precondition(
        data_dir,
        entry,
        events,
        None,
        Some(source),
        lock_deadline,
        checkpoint,
    )
}

fn publish_new_session_with_hook(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    publish_with_precondition(
        data_dir,
        entry,
        events,
        None,
        None,
        lock_deadline,
        checkpoint,
    )
}

fn publish_with_precondition(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    parent_precondition: Option<ParentPrecondition>,
    artifact_source: Option<&SessionIndexEntry>,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    codec::validate_entry_path(entry)?;
    validate_parent_precondition_shape(entry, parent_precondition.as_ref())?;
    let lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let root = lock.root();
    let mut entries = codec::read_index_in(root)?;
    validate_parent_generation(&entries, parent_precondition.as_ref())?;
    if let Some(source) = artifact_source {
        validate_source_generation(&entries, source)?;
    }
    ensure_candidate_is_unclaimed(root, &entries, entry)?;

    let transaction_id = allocate_transaction_id(root)?;
    let audio_bundle = if let Some(source) = artifact_source {
        stage_audio_bundle(root, &transaction_id, source, entry, events)?
    } else {
        None
    };
    if audio_bundle.is_some() {
        checkpoint(PublicationCheckpoint::AudioStaged)?;
    }
    let stage_path = timeline_stage_path(&transaction_id);
    let facts = write_timeline_stage(root, &stage_path, events, &entry.id)?;
    checkpoint(PublicationCheckpoint::TimelineStaged)?;

    let mut committed_entry = entry.clone();
    apply_timeline_facts(&mut committed_entry, &facts);
    let mut candidate_entries = entries.clone();
    candidate_entries.push(committed_entry.clone());
    validate_index_entries(&candidate_entries)?;

    let journal = PublicationJournal {
        norn_session_publication: if audio_bundle.is_some() {
            AUDIO_PUBLICATION_VERSION
        } else {
            TIMELINE_PUBLICATION_VERSION
        },
        transaction_id,
        parent_precondition,
        entry: committed_entry.clone(),
        timeline_bytes: facts.bytes,
        timeline_sha256: facts.sha256,
        audio_bundle,
    };
    let journal_path = write_journal(root, &journal)?;
    checkpoint(PublicationCheckpoint::JournalPublished)?;
    recover_one(root, &journal_path, &mut entries, checkpoint)?;
    Ok(committed_entry)
}

fn recover_one(
    root: &PrivateRoot,
    journal_path: &Path,
    entries: &mut Vec<SessionIndexEntry>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<(), SessionPersistError> {
    let transaction_id = journal_id(
        journal_path
            .file_name()
            .ok_or_else(|| io::Error::other("publication journal has no file name"))?,
    )
    .ok_or_else(|| io::Error::other("publication journal name is not owned"))?;
    let journal = read_journal(root, journal_path, &transaction_id)?;
    codec::validate_entry_path(&journal.entry)?;
    validate_journal_metadata(&journal)?;
    validate_parent_generation(entries, journal.parent_precondition.as_ref())?;

    let row_exists = ensure_recoverable_row(entries, &journal.entry)?;
    let audio_stage_was_verified = if let Some(manifest) = &journal.audio_bundle {
        let stage_was_verified =
            recover_audio_bundle(root, &transaction_id, &journal.entry, manifest)?;
        checkpoint(PublicationCheckpoint::AudioPublished)?;
        stage_was_verified
    } else {
        if audio_stage_is_present(root, &transaction_id)? {
            return Err(conflict(
                &journal.entry.id,
                "a timeline-only publication has an unexpected response-audio stage",
            ));
        }
        false
    };

    let stage_path = timeline_stage_path(&transaction_id);
    let final_path = session_file_relative(&journal.entry)?;
    let stage_facts = inspect_if_present(root, &stage_path, &journal.entry.id)?;
    if let Some(facts) = &stage_facts {
        ensure_timeline_matches(
            &journal,
            facts,
            "the staged timeline does not match its journal",
        )?;
    }
    let mut final_facts = inspect_if_present(root, &final_path, &journal.entry.id)?;
    if let Some(facts) = &final_facts {
        ensure_timeline_matches(
            &journal,
            facts,
            "the published timeline does not match its journal",
        )?;
    }

    if final_facts.is_none() {
        if stage_facts.is_none() {
            return Err(conflict(
                &journal.entry.id,
                "both the staged and published timelines are missing",
            ));
        }
        publish_staged_timeline(root, &stage_path, &final_path)?;
        final_facts = Some(inspect_timeline(root, &final_path, &journal.entry.id)?);
        let facts = final_facts
            .as_ref()
            .ok_or_else(|| io::Error::other("published timeline disappeared after publication"))?;
        ensure_timeline_matches(
            &journal,
            facts,
            "the published timeline does not match its journal",
        )?;
    }
    checkpoint(PublicationCheckpoint::TimelinePublished)?;

    if !row_exists {
        let mut candidate_entries = entries.clone();
        candidate_entries.push(journal.entry);
        validate_index_entries(&candidate_entries)?;
        codec::write_index_atomic_in(root, &candidate_entries)?;
        *entries = candidate_entries;
    }
    checkpoint(PublicationCheckpoint::IndexPublished)?;
    remove_committed_transaction(
        root,
        &transaction_id,
        &stage_path,
        journal_path,
        stage_facts.is_some(),
        audio_stage_was_verified,
    )
}

fn ensure_candidate_is_unclaimed(
    root: &PrivateRoot,
    entries: &[SessionIndexEntry],
    entry: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    if entries.iter().any(|existing| existing.id == entry.id) {
        return Err(SessionPersistError::IdExists {
            id: entry.id.clone(),
        });
    }
    let final_path = session_file_relative(entry)?;
    if entry.rel_path.as_ref().is_some_and(|target| {
        entries
            .iter()
            .any(|existing| existing.rel_path.as_ref() == Some(target))
    }) || root.regular_file_exists(&final_path)?
    {
        return Err(path_occupied(entry, &final_path));
    }
    Ok(())
}

fn ensure_recoverable_row(
    entries: &[SessionIndexEntry],
    entry: &SessionIndexEntry,
) -> Result<bool, SessionPersistError> {
    if let Some(existing) = entries.iter().find(|existing| existing.id == entry.id) {
        if existing == entry {
            return Ok(true);
        }
        return Err(conflict(
            &entry.id,
            "the indexed row differs from the publication journal",
        ));
    }
    if entry.rel_path.as_ref().is_some_and(|target| {
        entries
            .iter()
            .any(|existing| existing.rel_path.as_ref() == Some(target))
    }) {
        return Err(conflict(
            &entry.id,
            "another indexed session claims the publication timeline path",
        ));
    }
    Ok(false)
}

fn ensure_timeline_matches(
    journal: &PublicationJournal,
    facts: &TimelineFacts,
    reason: &'static str,
) -> Result<(), SessionPersistError> {
    let matches = journal.timeline_bytes == facts.bytes
        && journal.timeline_sha256 == facts.sha256
        && journal.entry.event_count == facts.event_count
        && journal.entry.total_input_tokens == facts.usage.input_tokens
        && journal.entry.total_output_tokens == facts.usage.output_tokens
        && journal.entry.total_cache_read_tokens == facts.usage.cache_read_tokens;
    if matches {
        Ok(())
    } else {
        Err(conflict(&journal.entry.id, reason))
    }
}

fn publish_staged_timeline(
    root: &PrivateRoot,
    stage_path: &Path,
    final_path: &Path,
) -> Result<(), SessionPersistError> {
    let parent = final_path.parent().unwrap_or_else(|| Path::new(""));
    root.create_dir_all(parent)?;
    sync_directory_chain(root, parent)?;
    root.publish_new(stage_path, final_path)?;
    root.sync_dir(parent)?;
    Ok(())
}

fn sync_directory_chain(root: &PrivateRoot, directory: &Path) -> Result<(), SessionPersistError> {
    let mut current = Some(directory);
    while let Some(path) = current {
        root.sync_dir(path)?;
        current = path.parent();
    }
    Ok(())
}

fn remove_committed_transaction(
    root: &PrivateRoot,
    transaction_id: &str,
    stage_path: &Path,
    journal_path: &Path,
    stage_was_verified: bool,
    audio_stage_was_verified: bool,
) -> Result<(), SessionPersistError> {
    if stage_was_verified
        && let Err(error) = root.remove_file(stage_path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    remove_verified_audio_stage(root, transaction_id, audio_stage_was_verified)?;
    root.remove_file(journal_path)?;
    root.sync_dir(Path::new(""))?;
    Ok(())
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "publication_audio_tests.rs"]
mod audio_tests;
