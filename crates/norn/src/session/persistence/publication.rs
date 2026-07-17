//! Crash-recoverable publication of seeded strict session timelines.

use std::ffi::OsStr;
use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::util::PrivateRoot;

use super::super::io::session_file_relative;
use super::super::strict::{StrictFormatHeader, validate_index_entries, visit_strict_event_file};
use super::super::types::{SessionIndexEntry, SessionPersistError};
use super::codec;

#[path = "publication_names.rs"]
mod names;
#[path = "publication_conflict.rs"]
mod publication_conflict;
#[path = "publication_hash.rs"]
mod publication_hash;
#[path = "publication_parent.rs"]
mod publication_parent;
#[path = "publication_recovery.rs"]
mod publication_recovery;
#[path = "publication_timeline_error.rs"]
mod publication_timeline_error;
use names::{
    journal_id, journal_path, journal_temp_id, journal_temp_path, timeline_stage_id,
    timeline_stage_path,
};
use publication_conflict::{conflict, path_occupied};
use publication_hash::HashingReader;
use publication_parent::{
    ParentPrecondition, child_precondition, validate_parent_generation,
    validate_parent_precondition_shape,
};
use publication_recovery::{
    allocate_transaction_id, inventory_and_remove_orphans, remove_owned_after_failure,
};
use publication_timeline_error::map_publication_timeline_error;

const PUBLICATION_VERSION: u32 = 2;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationJournal {
    norn_session_publication: u32,
    transaction_id: String,
    parent_precondition: Option<ParentPrecondition>,
    entry: SessionIndexEntry,
    timeline_bytes: u64,
    timeline_sha256: String,
}

#[derive(Debug)]
struct TimelineFacts {
    bytes: u64,
    sha256: String,
    event_count: u64,
    usage: Usage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationCheckpoint {
    TimelineStaged,
    JournalPublished,
    TimelinePublished,
    IndexPublished,
}

pub(super) fn is_publication_artifact_name(name: &OsStr) -> bool {
    journal_id(name).is_some()
        || journal_temp_id(name).is_some()
        || timeline_stage_id(name).is_some()
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
        lock_deadline,
        &mut |_| Ok(()),
    )
}

fn publish_new_session_with_hook(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    publish_with_precondition(data_dir, entry, events, None, lock_deadline, checkpoint)
}

fn publish_with_precondition(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    events: &[SessionEvent],
    parent_precondition: Option<ParentPrecondition>,
    lock_deadline: Option<Duration>,
    checkpoint: &mut impl FnMut(PublicationCheckpoint) -> Result<(), SessionPersistError>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    codec::validate_entry_path(entry)?;
    validate_parent_precondition_shape(entry, parent_precondition.as_ref())?;
    let lock = super::lock_recovered_index(data_dir, lock_deadline)?;
    let root = lock.root();
    let mut entries = codec::read_index_in(root)?;
    validate_parent_generation(&entries, parent_precondition.as_ref())?;
    ensure_candidate_is_unclaimed(root, &entries, entry)?;

    let transaction_id = allocate_transaction_id(root)?;
    let stage_path = timeline_stage_path(&transaction_id);
    let facts = write_timeline_stage(root, &stage_path, events, &entry.id)?;
    checkpoint(PublicationCheckpoint::TimelineStaged)?;

    let mut committed_entry = entry.clone();
    apply_timeline_facts(&mut committed_entry, &facts);
    let mut candidate_entries = entries.clone();
    candidate_entries.push(committed_entry.clone());
    validate_index_entries(&candidate_entries)?;

    let journal = PublicationJournal {
        norn_session_publication: PUBLICATION_VERSION,
        transaction_id,
        parent_precondition,
        entry: committed_entry.clone(),
        timeline_bytes: facts.bytes,
        timeline_sha256: facts.sha256,
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

    let row_exists = ensure_recoverable_row(entries, &journal.entry)?;
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
    remove_committed_transaction(root, &stage_path, journal_path, stage_facts.is_some())
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

fn write_timeline_stage(
    root: &PrivateRoot,
    stage_path: &Path,
    events: &[SessionEvent],
    session_id: &str,
) -> Result<TimelineFacts, SessionPersistError> {
    let result = (|| {
        let file = root.create_new(stage_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &StrictFormatHeader::current())?;
        writer.write_all(b"\n")?;
        for event in events {
            serde_json::to_writer(&mut writer, event)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        let file = writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        root.sync_dir(Path::new(""))?;
        inspect_timeline(root, stage_path, session_id)
    })();
    if result.is_err() {
        remove_owned_after_failure(root, stage_path);
    }
    result
}

fn inspect_if_present(
    root: &PrivateRoot,
    path: &Path,
    session_id: &str,
) -> Result<Option<TimelineFacts>, SessionPersistError> {
    match inspect_timeline(root, path, session_id) {
        Ok(facts) => Ok(Some(facts)),
        Err(SessionPersistError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn inspect_timeline(
    root: &PrivateRoot,
    path: &Path,
    session_id: &str,
) -> Result<TimelineFacts, SessionPersistError> {
    let file = root.open_read(path)?;
    let initial_length = file.metadata()?.len();
    let mut reader = HashingReader::new(file);
    let (_, _, counters) =
        visit_strict_event_file(BufReader::new(&mut reader), &root.display_path(path), drop)
            .map_err(|error| map_publication_timeline_error(error, session_id))?;
    let usage = counters.tracked_usage();
    let final_length = reader.metadata_len()?;
    if initial_length != final_length || reader.bytes_read() != final_length {
        return Err(io::Error::other("timeline changed while it was being validated").into());
    }
    Ok(TimelineFacts {
        bytes: final_length,
        sha256: reader.finish_sha256(),
        event_count: counters.event_count,
        usage,
    })
}

fn apply_timeline_facts(entry: &mut SessionIndexEntry, facts: &TimelineFacts) {
    entry.event_count = facts.event_count;
    entry.total_input_tokens = facts.usage.input_tokens;
    entry.total_output_tokens = facts.usage.output_tokens;
    entry.total_cache_read_tokens = facts.usage.cache_read_tokens;
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

fn validate_journal_metadata(journal: &PublicationJournal) -> Result<(), SessionPersistError> {
    if journal.norn_session_publication != PUBLICATION_VERSION {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal version is unsupported",
        ));
    }
    if journal.timeline_sha256.len() != 64
        || !journal
            .timeline_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal timeline digest is malformed",
        ));
    }
    validate_parent_precondition_shape(&journal.entry, journal.parent_precondition.as_ref())
}

fn write_journal(
    root: &PrivateRoot,
    journal: &PublicationJournal,
) -> Result<PathBuf, SessionPersistError> {
    let temporary = journal_temp_path(&journal.transaction_id);
    let final_path = journal_path(&journal.transaction_id);
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
        remove_owned_after_failure(root, &temporary);
    }
    result
}

fn read_journal(
    root: &PrivateRoot,
    path: &Path,
    transaction_id: &str,
) -> Result<PublicationJournal, SessionPersistError> {
    let file = root.open_read(path)?;
    let journal: PublicationJournal = serde_json::from_reader(BufReader::new(file))?;
    if journal.transaction_id != transaction_id {
        return Err(conflict(
            &journal.entry.id,
            "the publication journal transaction id does not match its owned file name",
        ));
    }
    Ok(journal)
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
    stage_path: &Path,
    journal_path: &Path,
    stage_was_verified: bool,
) -> Result<(), SessionPersistError> {
    if stage_was_verified
        && let Err(error) = root.remove_file(stage_path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    root.remove_file(journal_path)?;
    root.sync_dir(Path::new(""))?;
    Ok(())
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
