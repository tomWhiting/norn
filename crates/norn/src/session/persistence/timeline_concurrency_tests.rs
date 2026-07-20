use std::any::Any;
use std::io::Write as _;
use std::path::PathBuf;

use chrono::Utc;

use crate::session::events::EventUsage;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{DurabilityPolicy, JsonlSink, PersistenceSink};

use super::index::{
    publish_new_child_session, publish_new_session, read_index, read_index_with_deadline,
    update_index_entry,
};
use super::io::{append_events, read_session_events_for_entry};
use super::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError,
    SessionRecordOrigin, SessionStatus,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
        provider_state_identity: None,
    }
}

fn register(data_dir: &std::path::Path, entry: &SessionIndexEntry) -> TestResult {
    publish_new_session(data_dir, entry, &[], None)?;
    Ok(())
}

#[test]
fn concurrent_exact_batch_retries_converge_on_one_event() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("concurrent-exact-retry");
    register(directory.path(), &indexed)?;
    let relative = PathBuf::from(format!("{}.jsonl", indexed.id));
    let held = super::timeline_lock::lock_timeline_for_test(directory.path(), &relative)?;
    let candidate = event("same-event-from-two-writers");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    std::thread::scope(|scope| -> TestResult {
        let data_dir = directory.path();
        let session_id = indexed.id.as_str();
        let first_barrier = std::sync::Arc::clone(&barrier);
        let first_event = candidate.clone();
        let first = scope.spawn(move || {
            first_barrier.wait();
            append_events(
                data_dir,
                session_id,
                std::slice::from_ref(&first_event),
                false,
            )
        });
        let second_barrier = std::sync::Arc::clone(&barrier);
        let second_event = candidate.clone();
        let second = scope.spawn(move || {
            second_barrier.wait();
            append_events(
                data_dir,
                session_id,
                std::slice::from_ref(&second_event),
                false,
            )
        });

        barrier.wait();
        super::timeline_lock::wait_for_timeline_waiters_for_test(directory.path(), &relative, 1)?;
        drop(held);
        join_thread(first)?;
        join_thread(second)
    })?;

    let replay = read_session_events_for_entry(directory.path(), &indexed)?;
    assert_eq!(replay.events.len(), 1);
    assert_eq!(
        replay.events.first().map(|event| event.base().id.as_str()),
        Some(candidate.base().id.as_str()),
    );
    let rows = read_index(directory.path())?;
    let row = rows
        .iter()
        .find(|row| row.id == indexed.id)
        .ok_or_else(|| std::io::Error::other("concurrent retry index row disappeared"))?;
    assert_eq!(row.event_count, 1);
    Ok(())
}

#[test]
fn concurrent_registered_sinks_reconcile_exact_timeline_counters() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = entry("concurrent-root");
    register(directory.path(), &root)?;
    let mut indexed = entry("concurrent-child");
    let root_generation = root.generation;
    indexed.parent_id = Some(root.id);
    indexed.rel_path = Some("concurrent-root/children/concurrent-child.jsonl".to_owned());
    publish_new_child_session(directory.path(), &indexed, &[], root_generation, None)?;
    let mut first_sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let mut second_sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let relative = PathBuf::from(
        indexed
            .rel_path
            .as_deref()
            .ok_or_else(|| std::io::Error::other("nested test entry lost rel_path"))?,
    );
    let held = super::timeline_lock::lock_timeline_for_test(directory.path(), &relative)?;
    let first_event = event("first-distinct-writer");
    let second_event = event("second-distinct-writer");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    std::thread::scope(|scope| -> TestResult {
        let first_barrier = std::sync::Arc::clone(&barrier);
        let first_value = first_event.clone();
        let first = scope.spawn(move || {
            first_barrier.wait();
            first_sink.persist(&first_value)
        });
        let second_barrier = std::sync::Arc::clone(&barrier);
        let second_value = second_event.clone();
        let second = scope.spawn(move || {
            second_barrier.wait();
            second_sink.persist(&second_value)
        });

        barrier.wait();
        // Generation authority serializes registered writers at the index
        // lock, so exactly one writer can queue on this timeline at a time.
        super::timeline_lock::wait_for_timeline_waiters_for_test(directory.path(), &relative, 1)?;
        drop(held);
        join_thread(first)?;
        join_thread(second)
    })?;

    let replay = read_session_events_for_entry(directory.path(), &indexed)?;
    assert_eq!(replay.events.len(), 2);
    assert!(
        replay
            .events
            .iter()
            .any(|event| event.base().id.as_str() == first_event.base().id.as_str())
    );
    assert!(
        replay
            .events
            .iter()
            .any(|event| event.base().id.as_str() == second_event.base().id.as_str())
    );
    let rows = read_index(directory.path())?;
    let row = rows
        .iter()
        .find(|row| row.id == indexed.id)
        .ok_or_else(|| std::io::Error::other("concurrent sink index row disappeared"))?;
    assert_eq!(row.event_count, 2);
    Ok(())
}

#[test]
fn reader_waits_for_inflight_tail_before_recovery() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("reader-writer-race");
    register(directory.path(), &indexed)?;
    let first = event("durable-before-race");
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    sink.persist(&first)?;
    drop(sink);

    let relative = PathBuf::from(format!("{}.jsonl", indexed.id));
    let held = super::timeline_lock::lock_timeline_for_test(directory.path(), &relative)?;
    let mut file = held.root().open_read_append(&relative)?;
    file.write_all(br#"{"type":"UserMessage"#)?;
    file.sync_all()?;
    drop(file);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let replay = std::thread::scope(|scope| -> TestResult<_> {
        let reader_barrier = std::sync::Arc::clone(&barrier);
        let data_dir = directory.path();
        let entry = &indexed;
        let reader = scope.spawn(move || {
            reader_barrier.wait();
            read_session_events_for_entry(data_dir, entry)
        });
        barrier.wait();
        super::timeline_lock::wait_for_timeline_waiters_for_test(directory.path(), &relative, 1)?;
        drop(held);
        join_thread(reader)
    })?;

    assert_eq!(replay.events.len(), 1);
    assert_eq!(
        replay.events.first().map(|event| event.base().id.as_str()),
        Some(first.base().id.as_str()),
    );
    Ok(())
}

#[test]
fn delete_waits_for_timeline_owner_and_stale_writers_cannot_recreate() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("delete-race");
    register(directory.path(), &indexed)?;
    let mut stale_sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let relative = PathBuf::from(format!("{}.jsonl", indexed.id));
    let held = super::timeline_lock::lock_timeline_for_test(directory.path(), &relative)?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let deleted = std::thread::scope(|scope| -> TestResult<_> {
        let delete_barrier = std::sync::Arc::clone(&barrier);
        let data_dir = directory.path();
        let session_id = indexed.id.as_str();
        let delete = scope.spawn(move || {
            delete_barrier.wait();
            super::index::delete_session_transaction(data_dir, session_id, None)
        });
        barrier.wait();
        super::timeline_lock::wait_for_timeline_waiters_for_test(directory.path(), &relative, 1)?;
        drop(held);
        join_thread(delete)
    })?;
    assert_eq!(deleted.id, indexed.id);

    let stale = event("must-not-recreate-from-sink");
    assert!(stale_sink.persist(&stale).is_err());
    let batch = event("must-not-recreate-from-batch");
    let error = append_events(
        directory.path(),
        &indexed.id,
        std::slice::from_ref(&batch),
        false,
    )
    .err()
    .ok_or_else(|| std::io::Error::other("stale batch append recreated a deleted session"))?;
    assert!(matches!(error, SessionPersistError::NotFound { .. }));
    assert!(!directory.path().join(&relative).exists());
    assert!(
        read_index(directory.path())?
            .iter()
            .all(|entry| entry.id != indexed.id)
    );
    Ok(())
}

#[test]
fn registered_sink_holds_generation_while_waiting_for_timeline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("registered-lock-order");
    register(directory.path(), &indexed)?;
    let mut sink = JsonlSink::open_registered(
        directory.path(),
        &indexed,
        DurabilityPolicy::FsyncPerEvent,
        None,
    )?;
    let relative = PathBuf::from(format!("{}.jsonl", indexed.id));
    let held = super::timeline_lock::lock_timeline_for_test(directory.path(), &relative)?;

    std::thread::scope(|scope| -> TestResult {
        let candidate = event("generation-pinned-write");
        let writer = scope.spawn(move || sink.persist(&candidate));
        super::timeline_lock::wait_for_timeline_waiters_for_test(directory.path(), &relative, 1)?;

        let error = read_index_with_deadline(directory.path(), Some(std::time::Duration::ZERO))
            .err()
            .ok_or_else(|| std::io::Error::other("writer released its generation lock early"))?;
        assert!(matches!(
            error,
            SessionPersistError::IndexLockTimeout { .. }
        ));

        drop(held);
        join_thread(writer)?;
        Ok(())
    })?;

    let replay = read_session_events_for_entry(directory.path(), &indexed)?;
    assert_eq!(replay.events.len(), 1);
    Ok(())
}

#[test]
fn batch_append_preflights_exact_timeline_counters_before_writing() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("counter-preflight");
    publish_new_session(directory.path(), &indexed, &[usage_event(u64::MAX)], None)?;
    update_index_entry(directory.path(), &indexed.id, None, |entry| {
        entry.total_input_tokens = 0;
    })?;
    let timeline = directory.path().join(format!("{}.jsonl", indexed.id));
    let before = std::fs::read(&timeline)?;
    let candidate = usage_event(1);

    let error = append_events(
        directory.path(),
        &indexed.id,
        std::slice::from_ref(&candidate),
        false,
    )
    .err()
    .ok_or_else(|| std::io::Error::other("overflowing index delta was accepted"))?;
    assert!(matches!(
        error,
        SessionPersistError::IndexCounterOverflow {
            field: "total_input_tokens",
            ..
        }
    ));
    assert_eq!(std::fs::read(timeline)?, before);
    Ok(())
}

#[test]
fn delete_checkpoint_before_index_publication_preserves_registered_history() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("delete-pre-index-stop");
    register(directory.path(), &indexed)?;
    let timeline = directory.path().join(format!("{}.jsonl", indexed.id));
    let index = directory.path().join("index.jsonl");
    let timeline_before = std::fs::read(&timeline)?;
    let index_before = std::fs::read(&index)?;

    let mut stop = |checkpoint| {
        if checkpoint == super::index::DeleteCheckpoint::JournalPublished {
            return Err(std::io::Error::other("injected pre-index stop").into());
        }
        Ok(())
    };
    let result = super::index::delete_session_transaction_with_hook(
        directory.path(),
        &indexed.id,
        None,
        &mut stop,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(timeline)?, timeline_before);
    assert_eq!(std::fs::read(index)?, index_before);
    assert_eq!(deletion_journal_count(directory.path())?, 1);
    assert_eq!(read_index(directory.path())?.len(), 1);
    assert_eq!(deletion_journal_count(directory.path())?, 0);
    Ok(())
}

#[test]
fn delete_checkpoint_after_index_publication_recovers_private_orphans() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("delete-post-index-stop");
    register(directory.path(), &indexed)?;
    let timeline = directory.path().join(format!("{}.jsonl", indexed.id));
    let artifact_directory = directory.path().join(&indexed.id);
    std::fs::create_dir_all(artifact_directory.join("outputs"))?;
    std::fs::write(artifact_directory.join("outputs/result.txt"), b"retained")?;

    let mut stop = |checkpoint| {
        if checkpoint == super::index::DeleteCheckpoint::IndexPublished {
            return Err(std::io::Error::other("injected post-index stop").into());
        }
        Ok(())
    };
    let result = super::index::delete_session_transaction_with_hook(
        directory.path(),
        &indexed.id,
        None,
        &mut stop,
    );
    assert!(result.is_err());
    assert!(!std::fs::read_to_string(directory.path().join("index.jsonl"))?.contains(&indexed.id));
    assert!(timeline.exists());
    assert!(artifact_directory.exists());
    assert_eq!(deletion_journal_count(directory.path())?, 1);

    assert!(read_index(directory.path())?.is_empty());
    assert_eq!(deletion_journal_count(directory.path())?, 0);
    assert!(!artifact_directory.exists());
    assert!(!timeline.exists());
    Ok(())
}

fn deletion_journal_count(data_dir: &std::path::Path) -> TestResult<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(data_dir)? {
        let name = entry?.file_name();
        if name.to_str().is_some_and(|name| {
            name.starts_with(".session-deletion.")
                && std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension == "json")
        }) {
            count += 1;
        }
    }
    Ok(count)
}

fn join_thread<T>(
    thread: std::thread::ScopedJoinHandle<'_, Result<T, SessionPersistError>>,
) -> TestResult<T> {
    let result = thread.join().map_err(|panic| {
        std::io::Error::other(format!(
            "concurrent timeline thread panicked: {}",
            panic_detail(panic.as_ref()),
        ))
    })?;
    Ok(result?)
}

fn panic_detail(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}
