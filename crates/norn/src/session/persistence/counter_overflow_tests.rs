use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

use chrono::Utc;
use uuid::Uuid;

use super::index::{publish_new_session, read_index};
use super::io::read_session_events_from;
use super::strict::{
    StrictFormatHeader, StrictStoreError, read_strict_event_file, validate_staged_store,
};
use super::{IndexCounters, SessionIndexEntry, SessionPersistError};
use crate::session::SessionManager;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::persistence::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
};
use crate::session::store::{DurabilityPolicy, JsonlSink, PersistenceSink};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "test-model".to_owned(),
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
        provider_state_identity: None,
    }
}

fn distinct_generation(current: Uuid) -> Uuid {
    let mut bytes = *current.as_bytes();
    bytes[15] ^= 1;
    Uuid::from_bytes(bytes)
}

fn user_event(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

fn usage_event(input: u64, output: u64, cache_read: u64) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            ..EventUsage::default()
        },
        stop_reason: String::new(),
        response_id: None,
    }
}

fn strict_bytes<T: serde::Serialize>(rows: &[T]) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(&StrictFormatHeader::current())?;
    bytes.push(b'\n');
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn assert_persist_overflow(error: &SessionPersistError, field: &'static str) -> TestResult {
    if !matches!(
        error,
        SessionPersistError::IndexCounterOverflow {
            field: actual,
            ..
        } if *actual == field
    ) {
        return Err(
            io::Error::other(format!("expected {field} counter overflow, found {error}")).into(),
        );
    }
    Ok(())
}

#[test]
fn exact_counter_primitive_accepts_max_and_rejects_each_next_value() -> TestResult {
    for (maximum, next, field) in [
        (
            usage_event(u64::MAX, 0, 0),
            usage_event(1, 0, 0),
            "total_input_tokens",
        ),
        (
            usage_event(0, u64::MAX, 0),
            usage_event(0, 1, 0),
            "total_output_tokens",
        ),
        (
            usage_event(0, 0, u64::MAX),
            usage_event(0, 0, 1),
            "total_cache_read_tokens",
        ),
    ] {
        let counters = IndexCounters::try_from_events(&[maximum]).map_err(|error| {
            io::Error::other(format!("maximum counter was rejected: {error:?}"))
        })?;
        let error = counters
            .checked_with(&next)
            .err()
            .ok_or_else(|| io::Error::other("overflowing usage total was accepted"))?;
        assert_eq!(error.field(), field);
    }

    let max_events = IndexCounters {
        event_count: u64::MAX,
        ..IndexCounters::default()
    };
    let error = max_events
        .checked_with(&user_event("one-too-many"))
        .err()
        .ok_or_else(|| io::Error::other("overflowing event count was accepted"))?;
    assert_eq!(error.field(), "event_count");
    Ok(())
}

#[test]
fn strict_read_and_replay_return_typed_overflow() -> TestResult {
    let events = [usage_event(u64::MAX, 0, 0), usage_event(1, 0, 0)];
    let bytes = strict_bytes(&events)?;
    let strict_error = read_strict_event_file(Cursor::new(&bytes), Path::new("overflow.jsonl"))
        .err()
        .ok_or_else(|| io::Error::other("strict reader accepted overflowing usage"))?;
    assert!(matches!(
        strict_error,
        StrictStoreError::IndexCounterOverflow {
            field: "total_input_tokens",
            ..
        }
    ));

    let replay_error = read_session_events_from(Cursor::new(bytes), "overflow")
        .err()
        .ok_or_else(|| io::Error::other("strict replay accepted overflowing usage"))?;
    let SessionPersistError::InvalidTimeline(source) = replay_error else {
        return Err(io::Error::other(format!(
            "expected invalid strict timeline, found {replay_error}"
        ))
        .into());
    };
    assert!(matches!(
        *source,
        StrictStoreError::IndexCounterOverflow {
            field: "total_input_tokens",
            ..
        }
    ));
    Ok(())
}

#[test]
fn strict_store_validation_never_relabels_overflow_as_usage_mismatch() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut indexed = entry("overflow-store");
    indexed.event_count = 2;
    indexed.total_input_tokens = u64::MAX;
    fs::write(
        directory.path().join("index.jsonl"),
        strict_bytes(std::slice::from_ref(&indexed))?,
    )?;
    fs::write(
        directory.path().join("overflow-store.jsonl"),
        strict_bytes(&[usage_event(u64::MAX, 0, 0), usage_event(1, 0, 0)])?,
    )?;

    let error = validate_staged_store(directory.path())
        .err()
        .ok_or_else(|| io::Error::other("validator accepted overflowing strict store"))?;
    assert!(matches!(
        error,
        StrictStoreError::IndexCounterOverflow {
            field: "total_input_tokens",
            ..
        }
    ));
    Ok(())
}

#[test]
fn registered_sink_rejects_overflow_before_writing() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = publish_new_session(
        directory.path(),
        &entry("sink-overflow"),
        &[usage_event(u64::MAX, 0, 0)],
        None,
    )?;
    let path = directory.path().join("sink-overflow.jsonl");
    let before = fs::read(&path)?;
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;

    let error = sink
        .persist(&usage_event(1, 0, 0))
        .err()
        .ok_or_else(|| io::Error::other("sink appended an unrepresentable exact total"))?;
    assert_persist_overflow(&error, "total_input_tokens")?;
    assert_eq!(fs::read(path)?, before);
    Ok(())
}

#[test]
fn exact_tail_retry_counts_once_at_the_maximum_boundary() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = publish_new_session(directory.path(), &entry("ambiguous-max"), &[], None)?;
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let event = usage_event(u64::MAX, 0, 0);
    sink.fail_after_write_once();
    let first = sink
        .persist(&event)
        .err()
        .ok_or_else(|| io::Error::other("ambiguous write failure was not injected"))?;
    assert!(matches!(first, SessionPersistError::Io(_)));
    sink.persist(&event)?;
    sink.checkpoint()?;

    let rows = read_index(directory.path())?;
    let row = rows
        .first()
        .ok_or_else(|| io::Error::other("session index row disappeared"))?;
    assert_eq!(row.event_count, 1);
    assert_eq!(row.total_input_tokens, u64::MAX);
    Ok(())
}

#[test]
fn stale_registered_sink_cannot_recreate_a_deleted_timeline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = publish_new_session(directory.path(), &entry("deleted-session"), &[], None)?;
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    SessionManager::new(directory.path()).delete(&indexed.id)?;

    let error = sink
        .persist(&user_event("must-not-return"))
        .err()
        .ok_or_else(|| io::Error::other("stale sink appended after session deletion"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { .. }
    ));
    assert!(!directory.path().join("deleted-session.jsonl").exists());
    Ok(())
}

#[test]
fn deferred_flush_cannot_apply_an_old_generation_to_a_recreated_id() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = publish_new_session(directory.path(), &entry("recreated-session"), &[], None)?;
    let mut stale_sink =
        JsonlSink::open_registered(directory.path(), &indexed, DurabilityPolicy::Flush, None)?;
    stale_sink.persist(&usage_event(7, 3, 1))?;
    SessionManager::new(directory.path()).delete(&indexed.id)?;

    let mut replacement = entry(&indexed.id);
    replacement.generation = distinct_generation(indexed.generation);
    replacement.created_at = indexed.created_at;
    replacement.updated_at = indexed.updated_at;
    let replacement = publish_new_session(directory.path(), &replacement, &[], None)?;
    let error = stale_sink
        .checkpoint()
        .err()
        .ok_or_else(|| io::Error::other("stale deferred counters reached the recreated row"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { .. }
    ));
    drop(stale_sink);

    let rows = read_index(directory.path())?;
    let current = rows
        .iter()
        .find(|row| row.id == replacement.id)
        .ok_or_else(|| io::Error::other("replacement row disappeared"))?;
    assert_eq!(current.generation, replacement.generation);
    assert_ne!(current.generation, indexed.generation);
    assert_eq!(current.created_at, replacement.created_at);
    assert_eq!(current.event_count, 0);
    assert_eq!(current.total_input_tokens, 0);
    assert_eq!(current.total_output_tokens, 0);
    assert_eq!(current.total_cache_read_tokens, 0);
    Ok(())
}

#[test]
fn stale_registered_sink_cannot_reopen_a_recreated_id() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = publish_new_session(directory.path(), &entry("reopened-session"), &[], None)?;
    let mut stale_sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    SessionManager::new(directory.path()).delete(&indexed.id)?;

    let mut replacement = entry(&indexed.id);
    replacement.generation = distinct_generation(indexed.generation);
    replacement.created_at = indexed.created_at;
    replacement.updated_at = indexed.updated_at;
    let replacement = publish_new_session(directory.path(), &replacement, &[], None)?;
    let timeline = directory.path().join("reopened-session.jsonl");
    let before = fs::read(&timeline)?;
    let error = stale_sink
        .persist(&user_event("must-not-reach-the-replacement"))
        .err()
        .ok_or_else(|| io::Error::other("stale sink reopened the replacement generation"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { .. }
    ));
    assert_eq!(fs::read(timeline)?, before);

    let rows = read_index(directory.path())?;
    let current = rows
        .iter()
        .find(|row| row.id == replacement.id)
        .ok_or_else(|| io::Error::other("replacement row disappeared"))?;
    assert_eq!(current.generation, replacement.generation);
    assert_eq!(current.event_count, 0);
    Ok(())
}
