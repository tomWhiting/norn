use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{DurabilityPolicy, JsonlSink, PersistenceSink};
use chrono::Utc;

use super::index::{
    append_index_entry, delete_session_transaction, publish_new_session, read_index,
};
use super::io::{
    append_events, read_session_events, read_session_events_for_entry, retry_prefix_len,
};
use super::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError,
    SessionRecordOrigin, SessionStatus,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "gpt-test".to_owned(),
        working_dir: "/workspace".to_owned(),
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

fn register(data_dir: &std::path::Path, entry: &SessionIndexEntry) -> TestResult {
    publish_new_session(data_dir, entry, &[], None)?;
    Ok(())
}

fn assert_invalid_timeline(error: &SessionPersistError) -> TestResult {
    let SessionPersistError::InvalidTimeline(_) = error else {
        return Err(std::io::Error::other(format!(
            "expected invalid-timeline error, found {error}"
        ))
        .into());
    };
    Ok(())
}

fn assert_invalid_input(error: &SessionPersistError) -> TestResult {
    let SessionPersistError::Io(source) = error else {
        return Err(std::io::Error::other(format!(
            "expected invalid-input I/O error, found {error}"
        ))
        .into());
    };
    assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn creates_exact_format_two_header_and_durable_event() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("session-a.jsonl");
    let mut sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    sink.persist(&event("durable"))?;
    drop(sink);

    let bytes = fs::read(&path)?;
    let mut lines = bytes.split(|byte| *byte == b'\n');
    assert_eq!(
        lines.next(),
        Some(br#"{"norn_session_format":2}"#.as_slice())
    );
    let replay = read_session_events(directory.path(), "session-a")?;
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.format_version, Some(2));
    Ok(())
}

#[test]
fn live_sink_reconciles_an_exact_ambiguous_write_without_duplicate_row() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("ambiguous-live-write");
    register(directory.path(), &indexed)?;
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let durable = event("written-before-error");
    sink.fail_after_write_once();
    let error = sink
        .persist(&durable)
        .err()
        .ok_or_else(|| std::io::Error::other("ambiguous write failure was not injected"))?;
    assert!(matches!(error, SessionPersistError::Io(_)));

    let different = event("must-not-pass-pending-write");
    let conflict = sink
        .persist(&different)
        .err()
        .ok_or_else(|| std::io::Error::other("different event bypassed ambiguous-write state"))?;
    assert!(matches!(
        conflict,
        SessionPersistError::EventAppendConflict { .. }
    ));

    sink.persist(&durable)?;
    sink.checkpoint()?;
    drop(sink);
    let replay = read_session_events_for_entry(directory.path(), &indexed)?;
    assert_eq!(replay.events.len(), 1);
    let rows = read_index(directory.path())?;
    let row = rows
        .first()
        .ok_or_else(|| std::io::Error::other("registered session index row disappeared"))?;
    assert_eq!(row.event_count, 1);
    Ok(())
}

#[test]
fn truncates_only_an_incomplete_final_event_before_append() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("session-a.jsonl");
    let mut sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    sink.persist(&event("before"))?;
    drop(sink);

    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    let mut torn = br#"{"type":"UserMessage","base":{"id":"torn"#.to_vec();
    torn.extend(vec![b'x'; 24 * 1024]);
    file.write_all(&torn)?;
    file.sync_all()?;
    drop(file);

    let mut resumed = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    resumed.persist(&event("after"))?;
    drop(resumed);

    let replay = read_session_events(directory.path(), "session-a")?;
    assert_eq!(replay.events.len(), 2);
    let bytes = fs::read(&path)?;
    assert!(!bytes.windows(4).any(|window| window == b"torn"));
    Ok(())
}

#[test]
fn rejects_internal_corruption_without_mutating_the_file() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("session-a.jsonl");
    let mut sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    sink.persist(&event("before"))?;
    drop(sink);

    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(b"{not-json}\n")?;
    file.sync_all()?;
    drop(file);
    let before = fs::read(&path)?;

    let error = JsonlSink::open(&path)
        .err()
        .ok_or_else(|| std::io::Error::other("corrupt timeline unexpectedly opened for append"))?;
    assert_invalid_timeline(&error)?;
    assert_eq!(fs::read(&path)?, before);
    let error = read_session_events(directory.path(), "session-a")
        .err()
        .ok_or_else(|| std::io::Error::other("corrupt timeline was replayed"))?;
    assert_invalid_timeline(&error)?;
    Ok(())
}

#[test]
fn bound_append_revalidates_the_complete_existing_timeline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("session-a.jsonl");
    let mut sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    sink.persist(&event("before"))?;

    let mut attacker = fs::OpenOptions::new().append(true).open(&path)?;
    attacker.write_all(b"{not-json}\n")?;
    attacker.sync_all()?;
    drop(attacker);
    let before = fs::read(&path)?;

    let error = sink
        .persist(&event("must-not-land"))
        .err()
        .ok_or_else(|| std::io::Error::other("bound append skipped strict validation"))?;
    assert_invalid_timeline(&error)?;
    assert_eq!(fs::read(path)?, before);
    Ok(())
}

#[test]
fn rejects_duplicate_keys_and_event_ids() -> TestResult {
    let duplicate_keys = tempfile::tempdir()?;
    let first_path = duplicate_keys.path().join("session-a.jsonl");
    let row = serde_json::to_string(&event("duplicate-key"))?;
    let duplicated = row.replacen(
        "\"parent_id\":null",
        "\"parent_id\":null,\"parent_id\":null",
        1,
    );
    assert_ne!(duplicated, row);
    fs::write(
        &first_path,
        format!("{{\"norn_session_format\":2}}\n{duplicated}\n"),
    )?;
    let error = read_session_events(duplicate_keys.path(), "session-a")
        .err()
        .ok_or_else(|| std::io::Error::other("duplicate key was accepted"))?;
    assert_invalid_timeline(&error)?;

    let duplicate_ids = tempfile::tempdir()?;
    let second_path = duplicate_ids.path().join("session-b.jsonl");
    let duplicate = serde_json::to_string(&event("duplicate-id"))?;
    fs::write(
        &second_path,
        format!("{{\"norn_session_format\":2}}\n{duplicate}\n{duplicate}\n"),
    )?;
    let error = read_session_events(duplicate_ids.path(), "session-b")
        .err()
        .ok_or_else(|| std::io::Error::other("duplicate event id was accepted"))?;
    assert_invalid_timeline(&error)?;
    Ok(())
}

#[test]
fn rejects_legacy_header_and_complete_unterminated_row() -> TestResult {
    let legacy = tempfile::tempdir()?;
    fs::write(
        legacy.path().join("legacy.jsonl"),
        b"{\"norn_session_format\":1}\n",
    )?;
    let error = read_session_events(legacy.path(), "legacy")
        .err()
        .ok_or_else(|| std::io::Error::other("legacy timeline was accepted"))?;
    assert_invalid_timeline(&error)?;

    let unterminated = tempfile::tempdir()?;
    let path = unterminated.path().join("session-a.jsonl");
    let row = serde_json::to_string(&event("complete-but-unterminated"))?;
    fs::write(&path, format!("{{\"norn_session_format\":2}}\n{row}"))?;
    let before = fs::read(&path)?;
    let error = JsonlSink::open(&path)
        .err()
        .ok_or_else(|| std::io::Error::other("unterminated complete row was repaired"))?;
    assert_invalid_timeline(&error)?;
    assert_eq!(fs::read(path)?, before);
    Ok(())
}

#[test]
fn retry_plan_accepts_only_an_exact_durable_batch_prefix() -> TestResult {
    let first = event("first");
    let second = event("second");
    let requested = vec![first.clone(), second.clone()];
    assert_eq!(
        retry_prefix_len(std::slice::from_ref(&first), &requested)?,
        1
    );

    let mut changed = first.clone();
    if let SessionEvent::UserMessage { content, .. } = &mut changed {
        *content = "changed".to_owned();
    }
    assert!(retry_prefix_len(std::slice::from_ref(&first), &[changed]).is_err());
    assert!(retry_prefix_len(&[], &[second.clone(), second]).is_err());
    Ok(())
}

#[test]
fn registered_missing_timeline_fails_while_direct_absence_is_empty() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("registered-missing");
    append_index_entry(directory.path(), &indexed, None)?;

    let error = read_session_events_for_entry(directory.path(), &indexed)
        .err()
        .ok_or_else(|| std::io::Error::other("registered missing timeline replayed as empty"))?;
    assert!(matches!(error, SessionPersistError::NotFound { .. }));

    let direct = read_session_events(directory.path(), "unregistered-missing")?;
    assert!(direct.events.is_empty());
    Ok(())
}

#[test]
fn stale_registered_reader_cannot_repair_recreated_timeline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let stale = publish_new_session(directory.path(), &entry("reused-id"), &[], None)?;
    delete_session_transaction(directory.path(), &stale.id, None)?;
    let current = publish_new_session(directory.path(), &entry("reused-id"), &[], None)?;
    assert_ne!(stale.generation, current.generation);

    let path = directory.path().join("reused-id.jsonl");
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(b"{")?;
    file.sync_all()?;
    drop(file);
    let before = fs::read(&path)?;

    let error = read_session_events_for_entry(directory.path(), &stale)
        .err()
        .ok_or_else(|| std::io::Error::other("stale registered reader was accepted"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { id } if id == stale.id
    ));
    assert_eq!(
        fs::read(&path)?,
        before,
        "stale read must not repair or otherwise mutate the replacement timeline",
    );

    let replay = read_session_events_for_entry(directory.path(), &current)?;
    assert!(replay.events.is_empty());
    assert!(fs::metadata(path)?.len() < u64::try_from(before.len())?);
    Ok(())
}

#[test]
fn direct_sink_rejects_relative_paths_before_filesystem_access() -> TestResult {
    let relative = PathBuf::from(format!("norn-relative-{}.jsonl", uuid::Uuid::new_v4()));
    let error = JsonlSink::open(&relative)
        .err()
        .ok_or_else(|| std::io::Error::other("relative session path was accepted"))?;
    assert_invalid_input(&error)?;
    assert!(!relative.exists());
    Ok(())
}

#[test]
fn partial_batch_retry_reconciles_index_to_exact_timeline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("batch-retry");
    register(directory.path(), &indexed)?;

    let first = event("already-durable");
    let second = event("pending");
    let path = directory.path().join("batch-retry.jsonl");
    let mut direct = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    direct.persist(&first)?;
    drop(direct);

    append_events(directory.path(), &indexed.id, &[first, second], false)?;
    let replay = read_session_events_for_entry(directory.path(), &indexed)?;
    assert_eq!(replay.events.len(), 2);
    let rows = read_index(directory.path())?;
    assert_eq!(rows.len(), 1);
    let row = rows
        .first()
        .ok_or_else(|| std::io::Error::other("retry index row disappeared"))?;
    assert_eq!(row.event_count, 2);
    Ok(())
}
