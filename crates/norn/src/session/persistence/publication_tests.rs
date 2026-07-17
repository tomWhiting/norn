use std::io::{self, Write as _};
use std::path::Path;

use chrono::Utc;

use super::names::journal_temp_path;
use super::*;
use crate::session::events::{EventBase, EventUsage};
use crate::session::persistence::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
};

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
    }
}

fn event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn usage_event(input_tokens: u64) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage {
            input_tokens,
            ..EventUsage::default()
        },
        stop_reason: String::new(),
        response_id: None,
    }
}

fn injected_failure() -> SessionPersistError {
    io::Error::other("injected publication stop").into()
}

fn require_failed_publication(
    result: &Result<SessionIndexEntry, SessionPersistError>,
) -> Result<(), Box<dyn std::error::Error>> {
    if result.is_ok() {
        return Err(io::Error::other("publication unexpectedly passed its injected stop").into());
    }
    Ok(())
}

#[test]
fn normal_publication_returns_committed_row_without_transaction_residue()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let candidate = entry("successful-candidate");
    let committed = publish_new_session(
        directory.path(),
        &candidate,
        &[event("seeded history")],
        None,
    )?;

    assert_eq!(committed.event_count, 1);
    assert!(
        directory
            .path()
            .join("successful-candidate.jsonl")
            .is_file()
    );
    assert!(!has_publication_residue(directory.path())?);
    Ok(())
}

#[test]
fn durable_publication_seams_converge_on_the_next_index_read()
-> Result<(), Box<dyn std::error::Error>> {
    for stopped_at in [
        PublicationCheckpoint::JournalPublished,
        PublicationCheckpoint::TimelinePublished,
        PublicationCheckpoint::IndexPublished,
    ] {
        let directory = tempfile::tempdir()?;
        let parent = entry("parent-session");
        super::super::append_index_entry(directory.path(), &parent, None)?;
        let candidate = entry(&format!("candidate-{stopped_at:?}"));
        let events = [event("seeded history")];
        let result = publish_new_session_with_hook(
            directory.path(),
            &candidate,
            &events,
            None,
            &mut |checkpoint| {
                if checkpoint == stopped_at {
                    Err(injected_failure())
                } else {
                    Ok(())
                }
            },
        );
        require_failed_publication(&result)?;

        let rows = super::super::read_index(directory.path())?;
        let recovered = rows
            .iter()
            .find(|row| row.id == candidate.id)
            .ok_or_else(|| io::Error::other("recovered publication row is missing"))?;
        assert_eq!(recovered.event_count, 1);
        let replay = crate::session::persistence::read_session_events_for_entry(
            directory.path(),
            recovered,
        )?;
        assert_eq!(replay.events.len(), 1);
        assert!(!has_pending_journal(directory.path())?);
    }
    Ok(())
}

#[test]
fn pre_journal_stage_is_inert_and_never_registers_a_session()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let candidate = entry("uncommitted-candidate");
    let result = publish_new_session_with_hook(
        directory.path(),
        &candidate,
        &[event("not committed")],
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::TimelineStaged {
                Err(injected_failure())
            } else {
                Ok(())
            }
        },
    );
    require_failed_publication(&result)?;

    let rows = super::super::read_index(directory.path())?;
    assert!(!rows.iter().any(|row| row.id == candidate.id));
    assert!(
        !directory
            .path()
            .join(format!("{}.jsonl", candidate.id))
            .exists()
    );
    assert!(!has_timeline_stage(directory.path())?);
    Ok(())
}

#[test]
fn first_publication_crash_is_reclaimed_without_creating_an_index()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let candidate = entry("first-candidate");
    let result = publish_new_session_with_hook(
        directory.path(),
        &candidate,
        &[event("not committed")],
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::TimelineStaged {
                Err(injected_failure())
            } else {
                Ok(())
            }
        },
    );
    require_failed_publication(&result)?;
    assert!(has_timeline_stage(directory.path())?);

    assert!(super::super::read_index(directory.path())?.is_empty());
    assert!(!has_publication_residue(directory.path())?);
    assert!(
        !directory
            .path()
            .join(super::codec::INDEX_FILE_NAME)
            .exists()
    );
    Ok(())
}

#[test]
fn repeated_pre_journal_crashes_never_accumulate_stages() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    for attempt in 0..8 {
        let candidate = entry(&format!("candidate-{attempt}"));
        let result = publish_new_session_with_hook(
            directory.path(),
            &candidate,
            &[event("not committed")],
            None,
            &mut |checkpoint| {
                if checkpoint == PublicationCheckpoint::TimelineStaged {
                    Err(injected_failure())
                } else {
                    Ok(())
                }
            },
        );
        require_failed_publication(&result)?;
        assert_eq!(publication_residue_count(directory.path())?, 1);
    }
    let _rows = super::super::read_index(directory.path())?;
    assert_eq!(publication_residue_count(directory.path())?, 0);
    Ok(())
}

#[test]
fn orphan_exact_journal_temporary_is_reclaimed() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let transaction_id = Uuid::new_v4().hyphenated().to_string();
    let temporary = journal_temp_path(&transaction_id);
    std::fs::write(directory.path().join(&temporary), b"interrupted journal")?;

    assert!(super::super::read_index(directory.path())?.is_empty());
    assert!(!directory.path().join(temporary).exists());
    Ok(())
}

#[test]
fn one_locked_recovery_converges_multiple_independent_journals()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let first = prepare_pending(
        directory.path(),
        entry("candidate-one"),
        &[event("one")],
        None,
    )?;
    let second = prepare_pending(
        directory.path(),
        entry("candidate-two"),
        &[event("two")],
        None,
    )?;

    let rows = super::super::read_index(directory.path())?;
    assert!(rows.iter().any(|row| row.id == first.id));
    assert!(rows.iter().any(|row| row.id == second.id));
    assert!(!has_pending_journal(directory.path())?);
    Ok(())
}

#[test]
fn mismatched_final_timeline_is_never_replaced_or_removed() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let candidate = entry("foreign-collision");
    let final_path = directory.path().join("foreign-collision.jsonl");
    let foreign = b"foreign bytes that are not a norn timeline\n";
    let result = publish_new_session_with_hook(
        directory.path(),
        &candidate,
        &[event("owned stage")],
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::JournalPublished {
                std::fs::write(&final_path, foreign)?;
            }
            Ok(())
        },
    );
    require_failed_publication(&result)?;
    assert_eq!(std::fs::read(&final_path)?, foreign);

    let permit = super::super::super::acquire_private_fs()?;
    let root = PrivateRoot::open(directory.path())?;
    let rows = super::codec::read_index_in(&root)?;
    drop(root);
    drop(permit);
    assert!(!rows.iter().any(|row| row.id == candidate.id));
    assert!(has_pending_journal(directory.path())?);
    Ok(())
}

#[test]
fn exact_owned_artifacts_do_not_turn_an_empty_store_into_ambiguous_data()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let transaction_id = Uuid::new_v4().hyphenated().to_string();
    let permit = super::super::super::acquire_private_fs()?;
    let root = PrivateRoot::open(directory.path())?;
    let stage = timeline_stage_path(&transaction_id);
    let mut file = root.create_new(&stage)?;
    file.write_all(b"inert pre-journal stage")?;
    file.sync_all()?;
    drop(file);
    drop(root);
    drop(permit);

    assert!(super::super::read_index(directory.path())?.is_empty());
    assert!(!is_publication_artifact_name(OsStr::new(
        ".norn-publication-timeline-not-a-uuid.stage"
    )));
    Ok(())
}

#[test]
fn seeded_usage_overflow_fails_typed_before_journal_or_index_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let candidate = entry("overflow-candidate");
    let result = publish_new_session(
        directory.path(),
        &candidate,
        &[usage_event(u64::MAX), usage_event(1)],
        None,
    );
    let Err(error) = result else {
        return Err(io::Error::other("overflowing publication succeeded").into());
    };
    assert!(matches!(
        error,
        SessionPersistError::IndexCounterOverflow {
            field: "total_input_tokens",
            ..
        }
    ));
    let rows = super::super::read_index(directory.path())?;
    assert!(!rows.iter().any(|row| row.id == candidate.id));
    assert!(!has_publication_residue(directory.path())?);
    Ok(())
}

#[test]
fn recovery_refuses_a_changed_parent_generation() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let mut child = entry("child-session");
    child.parent_id = Some(parent.id.clone());
    child.rel_path = Some("parent-session/children/child-session.jsonl".to_owned());
    let precondition = child_precondition(&child, parent.generation)?;
    prepare_pending(
        directory.path(),
        child,
        &[event("child history")],
        Some(precondition),
    )?;

    let lock = super::super::super::lock::lock_index(directory.path(), None)?;
    let mut rows = super::codec::read_index_in(lock.root())?;
    rows[0].generation = Uuid::new_v4();
    super::codec::write_index_atomic_in(lock.root(), &rows)?;
    drop(lock);

    let error = super::super::read_index(directory.path())
        .err()
        .ok_or_else(|| io::Error::other("stale-parent publication unexpectedly recovered"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { id } if id == parent.id
    ));
    assert!(
        !directory
            .path()
            .join("parent-session/children/child-session.jsonl")
            .exists()
    );
    assert!(has_pending_journal(directory.path())?);
    Ok(())
}

#[test]
fn stale_parent_generation_is_rejected_before_child_staging()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = entry("parent-session");
    super::super::append_index_entry(directory.path(), &parent, None)?;
    let mut child = entry("child-session");
    child.parent_id = Some(parent.id.clone());
    child.rel_path = Some("parent-session/children/child-session.jsonl".to_owned());

    let error = publish_new_child_session(
        directory.path(),
        &child,
        &[event("child history")],
        Uuid::new_v4(),
        None,
    )
    .err()
    .ok_or_else(|| io::Error::other("stale-parent child publication unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { id } if id == parent.id
    ));
    assert!(!has_publication_residue(directory.path())?);
    assert!(
        !directory
            .path()
            .join("parent-session/children/child-session.jsonl")
            .exists()
    );
    Ok(())
}

fn prepare_pending(
    data_dir: &Path,
    candidate: SessionIndexEntry,
    events: &[SessionEvent],
    parent_precondition: Option<ParentPrecondition>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let lock = super::super::super::lock::lock_index(data_dir, None)?;
    let root = lock.root();
    let entries = super::codec::read_index_in(root)?;
    ensure_candidate_is_unclaimed(root, &entries, &candidate)?;
    let transaction_id = allocate_transaction_id(root)?;
    let stage_path = timeline_stage_path(&transaction_id);
    let facts = write_timeline_stage(root, &stage_path, events, &candidate.id)?;
    let mut committed = candidate;
    apply_timeline_facts(&mut committed, &facts);
    let journal = PublicationJournal {
        norn_session_publication: TIMELINE_PUBLICATION_VERSION,
        transaction_id,
        parent_precondition,
        entry: committed.clone(),
        timeline_bytes: facts.bytes,
        timeline_sha256: facts.sha256,
        audio_bundle: None,
    };
    write_journal(root, &journal)?;
    Ok(committed)
}

fn has_pending_journal(data_dir: &Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(data_dir)?
        .filter_map(Result::ok)
        .any(|entry| journal_id(&entry.file_name()).is_some()))
}

fn has_timeline_stage(data_dir: &Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(data_dir)?
        .filter_map(Result::ok)
        .any(|entry| timeline_stage_id(&entry.file_name()).is_some()))
}

fn has_publication_residue(data_dir: &Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(data_dir)?
        .filter_map(Result::ok)
        .any(|entry| is_publication_artifact_name(&entry.file_name())))
}

fn publication_residue_count(data_dir: &Path) -> io::Result<usize> {
    Ok(std::fs::read_dir(data_dir)?
        .filter_map(Result::ok)
        .filter(|entry| is_publication_artifact_name(&entry.file_name()))
        .count())
}
