//! Crash-tail recovery of incomplete response-publication groups.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::session::events::{EventBase, EventId, EventUsage, SessionEvent};
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::persistence::{SessionIndexEntry, resolved_session_file_path};
use crate::session::store::DurabilityPolicy;
use crate::session::{
    ProviderStateProvenance, PublicationTailRecovery, ResponseAudioArtifactLink,
    seal_response_publication_group,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type EventsResult = Result<Vec<SessionEvent>, Box<dyn std::error::Error>>;

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

/// Build one sealed `[boundary, provenance, assistant]` publication group.
fn sealed_group(parent: Option<EventId>, label: &str) -> EventsResult {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(parent),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance_base = EventBase::new(Some(boundary.base().id.clone()));
    let assistant_base = EventBase::new(Some(provenance_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    let assistant = assistant_event(assistant_base, label);
    let mut group = vec![boundary, provenance, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

/// Build one sealed four-row group carrying a response-audio link.
fn sealed_audio_group(parent: Option<EventId>, label: &str) -> EventsResult {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(parent),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance_base = EventBase::new(Some(boundary.base().id.clone()));
    let link_base = EventBase::new(Some(provenance_base.id.clone()));
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    let reference = serde_json::from_value(serde_json::json!(
        uuid::Uuid::new_v4().hyphenated().to_string()
    ))?;
    let link = ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some(format!("resp-{label}")),
    )
    .into_custom_event(link_base)?;
    let assistant = assistant_event(assistant_base, label);
    let mut group = vec![boundary, provenance, link, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn assistant_event(base: EventBase, label: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base,
        response_items: Vec::new(),
        content: label.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(format!("resp-{label}")),
    }
}

fn user_event(parent: Option<EventId>, content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(parent),
        content: content.to_owned(),
    }
}

/// Append raw JSONL rows exactly as a crashed writer would leave them: every
/// row complete and valid, the group simply cut short.
fn append_raw_rows(
    path: &Path,
    events: &[SessionEvent],
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    let mut written = 0_u64;
    for event in events {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        file.write_all(&line)?;
        written += u64::try_from(line.len())?;
    }
    file.sync_all()?;
    Ok(written)
}

struct TornSession {
    _dir: tempfile::TempDir,
    data_dir: PathBuf,
    entry: SessionIndexEntry,
    timeline: PathBuf,
    healthy_events: usize,
    torn: Vec<SessionEvent>,
    torn_bytes: u64,
}

impl TornSession {
    fn quarantine_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let boundary = self.torn.first().ok_or("no torn rows")?.base().id.clone();
        let name = self
            .timeline
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("timeline has no file name")?;
        Ok(self
            .timeline
            .with_file_name(format!("{name}.torn-tail-{}.quarantine", boundary.as_str())))
    }
}

/// Build a store whose healthy history is followed by a durable strict
/// prefix of a response-publication group — the on-disk shape an `ENOSPC`
/// death leaves behind. `tail` supplies the group; `rows` how many of its
/// rows reached the disk.
fn torn_tail_session(
    tail: impl FnOnce(Option<EventId>) -> EventsResult,
    rows: usize,
) -> Result<TornSession, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let data_dir = dir.path().to_path_buf();
    let manager = SessionManager::new(&data_dir);
    let opened = manager.create(options(), DurabilityPolicy::FsyncPerEvent)?;
    opened.store.append(user_event(None, "healthy prefix"))?;
    let healthy = sealed_group(opened.store.last_event_id(), "healthy")?;
    opened.store.append_batch(&healthy)?;
    opened.store.append(SessionEvent::ToolResult {
        base: EventBase::new(opened.store.last_event_id()),
        tool_call_id: "call_a".to_owned(),
        tool_name: "edit".to_owned(),
        output: serde_json::json!({"committed": true}),
        spool_ref: None,
        duration_ms: 38,
    })?;
    let healthy_events = opened.store.events().len();
    let entry = opened.entry.clone();
    let timeline = resolved_session_file_path(&data_dir, &entry);
    let last = opened.store.last_event_id();
    drop(opened);

    let group = tail(last)?;
    let torn = group
        .get(..rows)
        .ok_or("requested more rows than the group holds")?
        .to_vec();
    let torn_bytes = append_raw_rows(&timeline, &torn)?;

    Ok(TornSession {
        _dir: dir,
        data_dir,
        entry,
        timeline,
        healthy_events,
        torn,
        torn_bytes,
    })
}

fn recovery_record(
    event: &SessionEvent,
) -> Result<PublicationTailRecovery, Box<dyn std::error::Error>> {
    Ok(PublicationTailRecovery::from_event(event)?.ok_or("expected a tail-recovery event")?)
}

/// The corpse shape: a boundary and its provenance marker landed, the
/// assistant event they name never did. The session resumes from its healthy
/// prefix, the torn bytes are preserved verbatim in a quarantine sidecar, and
/// the recovery is recorded in the timeline itself.
#[test]
fn torn_marker_tail_recovers_to_the_healthy_prefix() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    let torn_row_bytes = std::fs::read(&torn.timeline)?
        [usize::try_from(std::fs::metadata(&torn.timeline)?.len() - torn.torn_bytes)?..]
        .to_vec();

    let artifacts = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    assert_eq!(
        artifacts.events.len(),
        torn.healthy_events + 1,
        "the healthy prefix plus exactly one recovery event",
    );

    let recovery = recovery_record(
        artifacts
            .events
            .last()
            .ok_or("recovered timeline is empty")?,
    )?;
    let expected_ids = torn
        .torn
        .iter()
        .map(|event| event.base().id.clone())
        .collect::<Vec<_>>();
    assert_eq!(recovery.quarantined_event_ids(), expected_ids.as_slice());
    assert_eq!(recovery.quarantined_bytes(), torn.torn_bytes);
    let orphan = torn.torn.get(1).ok_or("no provenance row")?;
    let orphan_target =
        ProviderStateProvenance::from_event(orphan)?.ok_or("provenance row does not decode")?;
    assert_eq!(
        recovery.orphaned_assistant_event_id(),
        Some(orphan_target.assistant_event_id()),
    );

    // The quarantine holds the removed bytes exactly, byte for byte.
    let quarantine = torn.quarantine_path()?;
    assert_eq!(std::fs::read(&quarantine)?, torn_row_bytes);
    assert_eq!(recovery.quarantine_file(), {
        let name = quarantine.file_name().and_then(|name| name.to_str());
        name.ok_or("quarantine has no file name")?
    });

    // The timeline on disk is exactly what the reader returned.
    let reread = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    assert_eq!(
        serde_json::to_value(&reread.events)?,
        serde_json::to_value(&artifacts.events)?,
    );
    Ok(())
}

/// A tear that landed only the boundary recovers with no orphaned target.
#[test]
fn torn_boundary_only_tail_recovers() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 1)?;
    let artifacts = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    assert_eq!(artifacts.events.len(), torn.healthy_events + 1);
    let recovery = recovery_record(
        artifacts
            .events
            .last()
            .ok_or("recovered timeline is empty")?,
    )?;
    assert_eq!(recovery.quarantined_event_ids().len(), 1);
    assert_eq!(recovery.orphaned_assistant_event_id(), None);
    assert!(torn.quarantine_path()?.exists());
    Ok(())
}

/// A four-row group torn after its response-audio link recovers too.
#[test]
fn torn_audio_linked_tail_recovers() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_audio_group(parent, "torn"), 3)?;
    let artifacts = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    assert_eq!(artifacts.events.len(), torn.healthy_events + 1);
    let recovery = recovery_record(
        artifacts
            .events
            .last()
            .ok_or("recovered timeline is empty")?,
    )?;
    assert_eq!(recovery.quarantined_event_ids().len(), 3);
    assert!(recovery.orphaned_assistant_event_id().is_some());
    Ok(())
}

/// Recovery runs once. A second open of the healed session appends nothing
/// and leaves the file byte-for-byte unchanged.
#[test]
fn recovery_is_idempotent_and_leaves_a_healed_session_untouched() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    let healed = std::fs::read(&torn.timeline)?;
    let quarantine = std::fs::read(torn.quarantine_path()?)?;

    for _pass in 0..2 {
        crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
        assert_eq!(
            std::fs::read(&torn.timeline)?,
            healed,
            "healed file is stable"
        );
        assert_eq!(
            std::fs::read(torn.quarantine_path()?)?,
            quarantine,
            "quarantine is written once",
        );
    }
    Ok(())
}

/// A healthy session is never touched: no quarantine, no recovery event.
#[test]
fn healthy_session_is_left_byte_for_byte_unchanged() -> TestResult {
    let dir = tempfile::tempdir()?;
    let manager = SessionManager::new(dir.path());
    let opened = manager.create(options(), DurabilityPolicy::FsyncPerEvent)?;
    opened.store.append(user_event(None, "hello"))?;
    let group = sealed_group(opened.store.last_event_id(), "healthy")?;
    opened.store.append_batch(&group)?;
    let entry = opened.entry.clone();
    let timeline = resolved_session_file_path(dir.path(), &entry);
    drop(opened);

    let before = std::fs::read(&timeline)?;
    let artifacts = crate::session::read_session_events_for_entry(dir.path(), &entry)?;
    assert_eq!(artifacts.events.len(), 4);
    assert_eq!(std::fs::read(&timeline)?, before);
    let siblings = std::fs::read_dir(dir.path())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("quarantine"))
        .count();
    assert_eq!(siblings, 0, "a healthy session produces no quarantine");
    Ok(())
}

/// Adopting a provider-state identity opens the timeline before the read
/// path does, so it heals a torn tail too — otherwise binding an identity
/// would stay the one door a crash-torn tail still locks.
#[test]
fn provider_identity_adoption_recovers_a_torn_tail() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    let identity = crate::provider::ProviderStateIdentity::derive(
        "norn.test.tail-recovery",
        b"adopting-identity",
    );
    let manager = SessionManager::new(&torn.data_dir);
    let resumed = manager
        .open_with_affinity(Some(identity))
        .resume_with_policy(
            &torn.entry.id,
            DurabilityPolicy::FsyncPerEvent,
            crate::session::ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(resumed.entry.provider_state_identity, Some(identity));
    // The healthy prefix, the recovery event, and the adoption boundary the
    // binding appends.
    assert_eq!(resumed.store.events().len(), torn.healthy_events + 2);
    assert!(torn.quarantine_path()?.exists());
    Ok(())
}

/// The full resume path heals too: `SessionManager::resume` returns the
/// healthy prefix instead of refusing the session.
#[test]
fn resume_recovers_a_torn_tail() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    let manager = SessionManager::new(&torn.data_dir);
    let resumed = manager.resume(&torn.entry.id, DurabilityPolicy::FsyncPerEvent)?;
    assert_eq!(resumed.store.events().len(), torn.healthy_events + 1);
    assert_eq!(
        resumed.entry.event_count,
        u64::try_from(torn.healthy_events + 1)?
    );

    // The healed session keeps working: a new publication group appends and
    // reads back cleanly.
    let next = sealed_group(resumed.store.last_event_id(), "after-recovery")?;
    resumed.store.append_batch(&next)?;
    drop(resumed);
    let artifacts = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)?;
    assert_eq!(artifacts.events.len(), torn.healthy_events + 4);
    Ok(())
}

/// A violation in the interior of the history is corruption, not a crash
/// artifact: it keeps the typed hard failure and the file is not touched.
#[test]
fn interior_violation_still_refuses_the_session() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    let last = torn.torn.last().ok_or("no torn rows")?.base().id.clone();
    append_raw_rows(
        &torn.timeline,
        &[
            user_event(Some(last.clone()), "after the tear"),
            user_event(Some(last.clone()), "and another"),
            user_event(Some(last), "and a third"),
        ],
    )?;
    let before = std::fs::read(&torn.timeline)?;

    let error = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)
        .err()
        .ok_or("an interior violation must stay fatal")?;
    assert_eq!(error.to_string(), "provider state provenance is invalid");
    assert_eq!(
        std::fs::read(&torn.timeline)?,
        before,
        "the file is untouched"
    );
    assert!(!torn.quarantine_path()?.exists());
    Ok(())
}

/// A torn tail whose *prefix* is itself broken is refused whole: truncation
/// never papers over a second, older violation.
#[test]
fn torn_tail_over_a_broken_prefix_still_refuses_the_session() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "first-tear"), 2)?;
    let second = sealed_group(
        torn.torn.last().map(|event| event.base().id.clone()),
        "second",
    )?;
    append_raw_rows(&torn.timeline, second.get(..2).ok_or("short group")?)?;
    let before = std::fs::read(&torn.timeline)?;

    let error = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)
        .err()
        .ok_or("a broken prefix must stay fatal")?;
    assert_eq!(error.to_string(), "provider state provenance is invalid");
    assert_eq!(
        std::fs::read(&torn.timeline)?,
        before,
        "the file is untouched"
    );
    Ok(())
}

/// Rows that are not part of the group shape do not make a recoverable tail.
#[test]
fn unrelated_rows_after_a_boundary_are_not_recoverable() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 1)?;
    let boundary = torn.torn.first().ok_or("no torn rows")?.base().id.clone();
    append_raw_rows(
        &torn.timeline,
        &[user_event(Some(boundary), "not a marker")],
    )?;
    let before = std::fs::read(&torn.timeline)?;

    assert!(crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry).is_err());
    assert_eq!(std::fs::read(&torn.timeline)?, before);
    Ok(())
}

/// A marker whose named assistant event *does* exist earlier in the history
/// is a misordered or duplicated group, not a truncated one.
#[test]
fn a_marker_naming_an_existing_event_is_not_recoverable() -> TestResult {
    let dir = tempfile::tempdir()?;
    let manager = SessionManager::new(dir.path());
    let opened = manager.create(options(), DurabilityPolicy::FsyncPerEvent)?;
    opened.store.append(user_event(None, "hello"))?;
    let group = sealed_group(opened.store.last_event_id(), "healthy")?;
    opened.store.append_batch(&group)?;
    let entry = opened.entry.clone();
    let timeline = resolved_session_file_path(dir.path(), &entry);
    let last = opened.store.last_event_id();
    drop(opened);

    // A fresh boundary and a marker pointing back at the assistant event of
    // the already-published group.
    let existing_assistant = group.get(2).ok_or("short group")?.base().id.clone();
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(last),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let marker = ProviderStateProvenance::new(existing_assistant, true)
        .into_custom_event(EventBase::new(Some(boundary.base().id.clone())))?;
    append_raw_rows(&timeline, &[boundary, marker])?;
    let before = std::fs::read(&timeline)?;

    assert!(crate::session::read_session_events_for_entry(dir.path(), &entry).is_err());
    assert_eq!(std::fs::read(&timeline)?, before);
    Ok(())
}

/// Quarantined evidence is never overwritten. A sidecar already occupying the
/// deterministic name with different bytes is a typed refusal, and the
/// timeline is left exactly as it was.
#[test]
fn a_conflicting_quarantine_refuses_without_truncating() -> TestResult {
    let torn = torn_tail_session(|parent| sealed_group(parent, "torn"), 2)?;
    std::fs::write(torn.quarantine_path()?, b"someone else's bytes\n")?;
    let before = std::fs::read(&torn.timeline)?;

    let error = crate::session::read_session_events_for_entry(&torn.data_dir, &torn.entry)
        .err()
        .ok_or("a conflicting quarantine must refuse")?;
    assert!(
        error
            .to_string()
            .contains("a quarantine file with different content already occupies the name"),
        "unexpected error: {error}",
    );
    assert_eq!(
        std::fs::read(&torn.timeline)?,
        before,
        "a failed quarantine never truncates the timeline",
    );
    Ok(())
}
