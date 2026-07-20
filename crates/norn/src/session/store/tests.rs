#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]

use std::io::Write;

use super::*;
use crate::session::events::EventBase;
use crate::session::jsonl_sink::write_event_line;

fn user_msg(content: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: content.to_owned(),
    }
}

#[test]
fn append_and_retrieve_by_id() {
    let store = EventStore::new();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
        ids.push(id);
    }
    assert_eq!(store.len(), 5);
    for id in &ids {
        assert!(store.get(id).is_some());
    }
}

#[test]
fn events_in_insertion_order() {
    let store = EventStore::new();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
        ids.push(id);
    }
    let events = store.events();
    for (i, event) in events.iter().enumerate() {
        assert_eq!(event.base().id, ids[i]);
    }
}

#[test]
fn last_events_returns_tail_in_insertion_order() {
    let store = EventStore::new();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
        ids.push(id);
    }

    let events = store.last_events(2);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].base().id, ids[3]);
    assert_eq!(events[1].base().id, ids[4]);
}

#[test]
fn last_events_count_above_len_returns_all_events() {
    let store = EventStore::new();
    store.append(user_msg("first")).expect("append first");
    store.append(user_msg("second")).expect("append second");

    let events = store.last_events(10);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        SessionEvent::UserMessage { content, .. } if content == "first"
    ));
    assert!(matches!(
        &events[1],
        SessionEvent::UserMessage { content, .. } if content == "second"
    ));
}

#[test]
fn last_events_zero_returns_empty_window() {
    let store = EventStore::new();
    store.append(user_msg("first")).expect("append");

    assert!(store.last_events(0).is_empty());
}

#[test]
fn duplicate_id_rejected() {
    let store = EventStore::new();
    let event = user_msg("hello");
    let id = event.base().id.clone();
    store.append(event).expect("first append");

    let dup = SessionEvent::UserMessage {
        base: EventBase {
            id,
            parent_id: None,
            timestamp: chrono::Utc::now(),
        },
        content: "dup".to_owned(),
    };
    assert!(store.append(dup).is_err());
}

#[test]
fn get_nonexistent_returns_none() {
    let store = EventStore::new();
    assert!(store.get(&EventId::new()).is_none());
}

#[test]
fn is_empty_initial() {
    let store = EventStore::new();
    assert!(store.is_empty());
    store.append(user_msg("a")).expect("append");
    assert!(!store.is_empty());
}

/// A sink that fails a configurable number of times, then succeeds.
struct FlakySink {
    failures_left: u32,
    persisted: Vec<String>,
}

impl PersistenceSink for FlakySink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if self.failures_left > 0 {
            self.failures_left -= 1;
            return Err(SessionPersistError::Io(std::io::Error::other(
                "simulated persist failure",
            )));
        }
        self.persisted
            .push(serde_json::to_string(event).expect("serialize"));
        Ok(())
    }
}

/// Regression for H19's write side: a sink failure must surface as a
/// typed error from `append`, the event must not enter the in-memory
/// store, and an immediate retry of the SAME event must succeed
/// (no duplicate-ID trap).
#[test]
fn sink_failure_surfaces_typed_error_and_retry_is_safe() {
    let store = EventStore::with_sink(Box::new(FlakySink {
        failures_left: 1,
        persisted: Vec::new(),
    }));
    let event = user_msg("important");

    let err = store.append(event.clone()).expect_err("sink must fail");
    assert!(
        matches!(err, SessionError::StorageError { .. }),
        "expected StorageError, got {err:?}",
    );
    assert_eq!(
        store.len(),
        0,
        "failed persist must not leave the event in memory",
    );

    let id = store.append(event).expect("retry succeeds");
    assert_eq!(store.len(), 1);
    assert!(store.get(&id).is_some());
}

/// A sink whose `checkpoint` fails a configurable number of times,
/// then succeeds — models a transient index-flush failure.
struct FlakyCheckpointSink {
    checkpoint_failures_left: u32,
    checkpoints_succeeded: u32,
}

impl PersistenceSink for FlakyCheckpointSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        if self.checkpoint_failures_left > 0 {
            self.checkpoint_failures_left -= 1;
            return Err(SessionPersistError::Io(std::io::Error::other(
                "simulated checkpoint failure",
            )));
        }
        self.checkpoints_succeeded += 1;
        Ok(())
    }
}

/// No silent in-memory degradation on the checkpoint path either: a
/// failing sink checkpoint must surface as a typed `StorageError`
/// (never `Ok`), already-persisted events must be unaffected, and a
/// retry must reach the sink again.
#[test]
fn checkpoint_failure_surfaces_typed_error_and_retry_reaches_sink() {
    let store = EventStore::with_sink(Box::new(FlakyCheckpointSink {
        checkpoint_failures_left: 1,
        checkpoints_succeeded: 0,
    }));
    store.append(user_msg("kept")).expect("append succeeds");

    let err = store.checkpoint().expect_err("first checkpoint must fail");
    assert!(
        matches!(err, SessionError::StorageError { .. }),
        "expected StorageError, got {err:?}",
    );
    assert_eq!(store.len(), 1, "persisted events are unaffected");

    store.checkpoint().expect("retry succeeds");
}

/// A sink that records which thread its `checkpoint` ran on.
struct ThreadRecordingSink {
    checkpoint_thread: std::sync::Arc<parking_lot::Mutex<Option<std::thread::ThreadId>>>,
}

impl PersistenceSink for ThreadRecordingSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        *self.checkpoint_thread.lock() = Some(std::thread::current().id());
        Ok(())
    }
}

/// R2's off-executor guarantee: `checkpoint_off_executor` must run
/// the sink's critical section on the blocking pool, never on the
/// executor thread. On a current-thread runtime every task polls on
/// the test thread, so the sink observing a DIFFERENT thread proves
/// the offload.
#[tokio::test]
async fn checkpoint_off_executor_runs_critical_section_off_the_executor_thread() {
    let checkpoint_thread = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let store = std::sync::Arc::new(EventStore::with_sink(Box::new(ThreadRecordingSink {
        checkpoint_thread: std::sync::Arc::clone(&checkpoint_thread),
    })));
    store.append(user_msg("step")).expect("append");

    std::sync::Arc::clone(&store)
        .checkpoint_off_executor()
        .await
        .expect("checkpoint succeeds");

    let recorded = checkpoint_thread.lock().expect("sink checkpoint ran");
    assert_ne!(
        recorded,
        std::thread::current().id(),
        "the checkpoint critical section must not run on the executor thread",
    );
}

/// Failure path parity with the sync `checkpoint`: a failing sink
/// checkpoint surfaces the typed `StorageError` through the
/// off-executor path, the delta stays retained, and a retry reaches
/// the sink again.
#[tokio::test]
async fn checkpoint_off_executor_surfaces_typed_error_and_retry_reaches_sink() {
    let store = std::sync::Arc::new(EventStore::with_sink(Box::new(FlakyCheckpointSink {
        checkpoint_failures_left: 1,
        checkpoints_succeeded: 0,
    })));
    store.append(user_msg("kept")).expect("append succeeds");

    let err = std::sync::Arc::clone(&store)
        .checkpoint_off_executor()
        .await
        .expect_err("first checkpoint must fail");
    assert!(
        matches!(err, SessionError::StorageError { .. }),
        "expected StorageError, got {err:?}",
    );
    assert_eq!(store.len(), 1, "persisted events are unaffected");

    std::sync::Arc::clone(&store)
        .checkpoint_off_executor()
        .await
        .expect("retry succeeds");
}

/// A writer that fails after writing a fixed number of bytes, then
/// writes normally — simulates ENOSPC mid-line.
struct TornWriter {
    bytes_before_failure: usize,
    written: Vec<u8>,
    failed: bool,
}

impl Write for TornWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.failed && self.written.len() + buf.len() > self.bytes_before_failure {
            let room = self.bytes_before_failure - self.written.len();
            self.written.extend_from_slice(&buf[..room]);
            self.failed = true;
            return Err(std::io::Error::other("disk full"));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Regression for H19's torn-line corruption: after a partial write,
/// the next line must NOT be concatenated onto the torn bytes — the
/// tear is terminated with a newline so the corrupt prefix is exactly
/// one skippable line and the next line parses cleanly.
#[test]
fn torn_line_is_terminated_not_continued() {
    let mut writer = TornWriter {
        bytes_before_failure: 5,
        written: Vec::new(),
        failed: false,
    };
    let mut needs_newline = false;

    let first = b"{\"type\":\"first\"}\n";
    let err = write_event_line(&mut writer, &mut needs_newline, first);
    assert!(err.is_err(), "first write must tear");
    assert!(needs_newline, "tear must be remembered");

    let second = b"{\"second\":true}\n";
    write_event_line(&mut writer, &mut needs_newline, second).expect("second write succeeds");
    assert!(!needs_newline);

    let content = String::from_utf8(writer.written).expect("utf8");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "torn bytes and new line must be separate");
    assert!(
        serde_json::from_str::<serde_json::Value>(lines[0]).is_err(),
        "torn prefix is corrupt (skippable)",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(lines[1]).expect("second line must parse cleanly");
    assert_eq!(parsed["second"], true);
}

/// A sink that blocks inside `persist` until released, to prove disk
/// I/O no longer runs under the in-memory state lock.
struct BlockingSink {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl PersistenceSink for BlockingSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.entered.send(()).expect("notify entered");
        self.release.recv().expect("wait for release");
        Ok(())
    }
}

/// Regression for the executor-thread stall: while a slow sink write
/// is in flight, readers of the in-memory state must not block.
#[test]
fn slow_sink_write_does_not_block_readers() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = std::sync::Arc::new(EventStore::with_sink(Box::new(BlockingSink {
        entered: entered_tx,
        release: release_rx,
    })));

    let appender = {
        let store = std::sync::Arc::clone(&store);
        std::thread::spawn(move || {
            store.append(user_msg("slow")).expect("append");
        })
    };

    // Wait until the sink is mid-write (holding the sink mutex).
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("sink entered");

    // Reads must complete while the write is still blocked.
    let reader = {
        let store = std::sync::Arc::clone(&store);
        std::thread::spawn(move || (store.len(), store.is_empty(), store.events().len()))
    };
    let (len, empty, events_len) = reader.join().expect("reader must not deadlock");
    assert_eq!(len, 0, "event is not visible until persisted");
    assert!(empty);
    assert_eq!(events_len, 0);

    release_tx.send(()).expect("release sink");
    appender.join().expect("appender finishes");
    assert_eq!(store.len(), 1);
}
