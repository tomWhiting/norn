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
        provider_state_identity: None,
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
        spool_inheritance: None,
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

fn spool_fork_events(
    data_dir: &Path,
    source: &SessionIndexEntry,
    destination: &SessionIndexEntry,
    with_audio: bool,
) -> Result<Vec<SessionEvent>, Box<dyn std::error::Error>> {
    use crate::session::events::ChildBranchKind;
    use crate::session::store::DurabilityPolicy;
    let writer = crate::session::spool::SpoolWriter::for_session(
        data_dir,
        source,
        DurabilityPolicy::Flush,
        None,
    );
    let base = EventBase::new(None);
    let reference = writer.write(&base.id, &serde_json::json!({"full": "original-spool"}))?;
    let mut events = vec![SessionEvent::ToolResult {
        base,
        tool_call_id: "call-publication".to_owned(),
        tool_name: "fixture".to_owned(),
        output: serde_json::json!({"bounded": true}),
        spool_ref: Some(reference),
        duration_ms: 1,
    }];
    if with_audio {
        use crate::provider::openai::response_stream_event::ResponseStreamEvent;
        use crate::provider::response_audio::ResponseAudioEvent;
        let audio = crate::session::ResponseAudioStore::for_session(
            data_dir,
            source,
            DurabilityPolicy::Flush,
            None,
        );
        let mut recording = audio.begin(1)?;
        let raw = ResponseStreamEvent::from_raw(
            serde_json::json!({"type": "response.audio.delta", "sequence_number": 1, "delta": "AQID"}),
        )?;
        let audio_event = ResponseAudioEvent::from_stream_event(&raw)?
            .ok_or_else(|| io::Error::other("audio fixture event was not recognized"))?;
        recording.append(&raw, &audio_event)?;
        let artifact = recording.seal(Some("resp_spool_audio"))?;
        let link_base = EventBase::new(events.last().map(|event| event.base().id.clone()));
        let assistant_base = EventBase::new(Some(link_base.id.clone()));
        events.push(
            crate::session::ResponseAudioArtifactLink::new(
                assistant_base.id.clone(),
                artifact,
                Some("resp_spool_audio".to_owned()),
            )
            .into_custom_event(link_base)?,
        );
        events.push(SessionEvent::AssistantMessage {
            base: assistant_base,
            response_items: Vec::new(),
            content: "audio".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: Some("resp_spool_audio".to_owned()),
        });
    }
    let anchor = events
        .last()
        .ok_or_else(|| io::Error::other("spool fixture has no events"))?
        .base()
        .id
        .clone();
    events.push(SessionEvent::ChildBranch {
        base: EventBase::new(Some(anchor.clone())),
        parent_session_id: Some(source.id.clone()),
        child_session_id: Some(destination.id.clone()),
        path_address: crate::session::branch::ROOT_PATH_ADDRESS.to_owned(),
        parent_event_anchor: Some(anchor),
        kind: ChildBranchKind::Fork,
    });
    Ok(events)
}

#[test]
fn inherited_spool_publication_recovers_all_durable_stops_with_and_without_audio()
-> Result<(), Box<dyn std::error::Error>> {
    for with_audio in [false, true] {
        for stopped_at in [
            PublicationCheckpoint::JournalPublished,
            PublicationCheckpoint::SpoolPublished,
            PublicationCheckpoint::TimelinePublished,
            PublicationCheckpoint::IndexPublished,
        ] {
            let directory = tempfile::tempdir()?;
            let source = publish_new_session(directory.path(), &entry("spool-parent"), &[], None)?;
            let destination = entry("spool-child");
            let events = spool_fork_events(directory.path(), &source, &destination, with_audio)?;
            let result = publish_new_fork_session_with_hook(
                directory.path(),
                &destination,
                &events,
                &source,
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
                .find(|row| row.id == destination.id)
                .ok_or_else(|| io::Error::other("recovered spool fork row absent"))?;
            let sidecar = directory
                .path()
                .join(crate::session::spool::inheritance_path(&destination.id));
            let committed = std::fs::read(&sidecar)?;
            let replay = crate::session::persistence::read_session_events_for_entry(
                directory.path(),
                recovered,
            )?;
            assert_eq!(
                serde_json::to_value(&replay.events)?,
                serde_json::to_value(&events)?
            );
            let writer = crate::session::spool::SpoolWriter::for_session(
                directory.path(),
                recovered,
                crate::session::store::DurabilityPolicy::Flush,
                None,
            );
            writer.validate_inherited_history(&replay.events)?;
            super::super::read_index(directory.path())?;
            assert_eq!(std::fs::read(&sidecar)?, committed);
            assert!(!has_publication_residue(directory.path())?);
        }
    }
    Ok(())
}

#[test]
fn prejournal_spool_authority_is_not_published() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = publish_new_session(directory.path(), &entry("spool-parent"), &[], None)?;
    let destination = entry("spool-child");
    let events = spool_fork_events(directory.path(), &source, &destination, false)?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
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
    assert!(!rows.iter().any(|row| row.id == destination.id));
    assert!(
        !directory
            .path()
            .join(crate::session::spool::inheritance_path(&destination.id))
            .exists()
    );
    assert!(!has_publication_residue(directory.path())?);
    Ok(())
}

#[test]
fn journal_owned_partial_sidecar_temporary_recovers_beside_audio()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = publish_new_session(directory.path(), &entry("spool-parent"), &[], None)?;
    let destination = entry("spool-child");
    let events = spool_fork_events(directory.path(), &source, &destination, true)?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::AudioPublished {
                Err(injected_failure())
            } else {
                Ok(())
            }
        },
    );
    require_failed_publication(&result)?;
    let mut transaction = None;
    for item in std::fs::read_dir(directory.path())? {
        if let Some(id) = journal_id(&item?.file_name()) {
            transaction = Some(id);
        }
    }
    let transaction =
        transaction.ok_or_else(|| io::Error::other("pending spool journal absent"))?;
    let temporary = directory
        .path()
        .join(&destination.id)
        .join(format!("spool-inheritance.{transaction}.tmp"));
    std::fs::write(&temporary, b"{partial")?;
    let rows = super::super::read_index(directory.path())?;
    assert!(rows.iter().any(|row| row.id == destination.id));
    assert!(!temporary.exists());
    assert!(
        directory
            .path()
            .join(crate::session::spool::inheritance_path(&destination.id))
            .exists()
    );
    Ok(())
}

#[test]
fn conflicting_published_sidecar_stops_recovery_without_overwrite()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = publish_new_session(directory.path(), &entry("spool-parent"), &[], None)?;
    let destination = entry("spool-child");
    let events = spool_fork_events(directory.path(), &source, &destination, false)?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::SpoolPublished {
                Err(injected_failure())
            } else {
                Ok(())
            }
        },
    );
    require_failed_publication(&result)?;
    let sidecar = directory
        .path()
        .join(crate::session::spool::inheritance_path(&destination.id));
    std::fs::write(&sidecar, b"{}")?;
    assert!(super::super::read_index(directory.path()).is_err());
    assert_eq!(std::fs::read(&sidecar)?, b"{}");
    assert!(!directory.path().join("spool-child.jsonl").exists());
    Ok(())
}

#[test]
fn spool_only_recovery_refuses_undeclared_destination_entries()
-> Result<(), Box<dyn std::error::Error>> {
    for (name, is_directory) in [
        ("unrelated.bin", false),
        ("unrelated-directory", true),
        ("spool-inheritance.other-transaction.tmp", false),
        ("spool-inheritance.json", true),
    ] {
        let directory = tempfile::tempdir()?;
        let source = publish_new_session(directory.path(), &entry("spool-parent"), &[], None)?;
        let destination = entry("spool-child");
        let events = spool_fork_events(directory.path(), &source, &destination, false)?;
        let result = publish_new_fork_session_with_hook(
            directory.path(),
            &destination,
            &events,
            &source,
            None,
            &mut |checkpoint| {
                if checkpoint == PublicationCheckpoint::JournalPublished {
                    Err(injected_failure())
                } else {
                    Ok(())
                }
            },
        );
        require_failed_publication(&result)?;
        let index_path = directory.path().join("index.jsonl");
        let index_before = std::fs::read(&index_path)?;
        let destination_directory = directory.path().join(&destination.id);
        std::fs::create_dir(&destination_directory)?;
        let foreign = destination_directory.join(name);
        let payload = if is_directory {
            std::fs::create_dir(&foreign)?;
            foreign.join("unrelated.bin")
        } else {
            foreign
        };
        std::fs::write(&payload, b"unrelated bytes must stay unclaimed")?;
        assert!(matches!(
            super::super::read_index(directory.path()),
            Err(SessionPersistError::PublicationConflict { id, reason })
                if id == destination.id
                    && reason == "the spool-inheritance directory shape disagrees with its journal"
        ));
        assert_eq!(std::fs::read(&index_path)?, index_before);
        assert_eq!(
            std::fs::read(&payload)?,
            b"unrelated bytes must stay unclaimed"
        );
        assert!(!directory.path().join("spool-child.jsonl").exists());
        assert!(has_pending_journal(directory.path())?);
    }
    Ok(())
}
