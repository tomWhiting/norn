use std::any::Any;
use std::io;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::persistence::io::read_session_events;
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, JsonlSink, ProviderStateProvenance,
    SessionManager, seal_response_publication_group, validate_provider_state_provenance,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type GroupResult = Result<Vec<SessionEvent>, Box<dyn std::error::Error>>;

const PROCESS_CHILD_ENV: &str = "NORN_D3_BATCH_PROCESS_CHILD";
const PROCESS_ROOT_ENV: &str = "NORN_D3_BATCH_PROCESS_ROOT";
const PROCESS_SESSION_ENV: &str = "NORN_D3_BATCH_PROCESS_SESSION";
const PROCESS_LABEL_ENV: &str = "NORN_D3_BATCH_PROCESS_LABEL";
const PROCESS_READY_ENV: &str = "NORN_D3_BATCH_PROCESS_READY";
const PROCESS_START_ENV: &str = "NORN_D3_BATCH_PROCESS_START";
const PROCESS_TEST_NAME: &str = "session::response_publication_batch_tests::independent_processes_publish_only_contiguous_response_groups";
const PROCESS_WRITERS: usize = 4;

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn response_group(label: &str) -> GroupResult {
    let assistant_id = crate::session::events::EventId::new();
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let boundary_id = boundary.base().id.clone();
    let provenance = ProviderStateProvenance::new(assistant_id.clone(), true)
        .into_custom_event(EventBase::new(Some(boundary_id)))?;
    let mut assistant_base = EventBase::new(Some(provenance.base().id.clone()));
    assistant_base.id = assistant_id;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: label.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(format!("response-{label}")),
    };
    let mut group = vec![boundary, provenance, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn join_error(payload: &(dyn Any + Send)) -> io::Error {
    let detail = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    };
    io::Error::other(format!("batch writer panicked: {detail}"))
}

fn assert_contiguous_response_groups(
    events: &[SessionEvent],
    expected_groups: usize,
) -> TestResult {
    assert_eq!(events.len(), expected_groups.saturating_mul(3));
    validate_provider_state_provenance(events)?;
    assert!(events.chunks_exact(3).all(|group| {
        matches!(
            group[0],
            SessionEvent::ProviderEpochBoundary {
                reason:
                    crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublicationV1(
                        _
                    ),
                ..
            }
        ) && ProviderStateProvenance::from_event(&group[1]).is_ok_and(|record| record.is_some())
            && matches!(group[2], SessionEvent::AssistantMessage { .. })
    }));
    Ok(())
}

#[test]
fn independent_handles_publish_only_contiguous_response_groups() -> TestResult {
    const CASES: usize = 20;
    const WRITERS: usize = 8;

    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    for case in 0..CASES {
        let session_id = format!("batch-concurrency-{case}");
        let mut handles =
            vec![manager.create_with_id(&session_id, options(), DurabilityPolicy::Flush)?];
        for _writer in 1..WRITERS {
            handles.push(manager.resume(&session_id, DurabilityPolicy::Flush)?);
        }

        let barrier = Arc::new(Barrier::new(WRITERS + 1));
        let mut writers = Vec::with_capacity(WRITERS);
        for (writer, opened) in handles.into_iter().enumerate() {
            let barrier = Arc::clone(&barrier);
            let group = response_group(&format!("{case}-{writer}"))?;
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                opened.store.append_batch(&group)
            }));
        }
        barrier.wait();
        for writer in writers {
            writer
                .join()
                .map_err(|payload| join_error(payload.as_ref()))??;
        }

        let entry = manager.resolve(&session_id)?;
        let artifacts = crate::session::read_session_events_for_entry(temp.path(), &entry)?;
        assert_contiguous_response_groups(&artifacts.events, WRITERS)?;
    }
    Ok(())
}

#[test]
fn independent_processes_publish_only_contiguous_response_groups() -> TestResult {
    if std::env::var_os(PROCESS_CHILD_ENV).is_some() {
        return run_process_child();
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let session_id = "batch-process-concurrency";
    let manager = SessionManager::new(&root);
    manager.create_with_id(session_id, options(), DurabilityPolicy::Flush)?;
    let start = root.join("batch-process-start");
    let mut children = Vec::with_capacity(PROCESS_WRITERS);
    let mut ready_paths = Vec::with_capacity(PROCESS_WRITERS);

    for writer in 0..PROCESS_WRITERS {
        let ready = root.join(format!("batch-process-ready-{writer}"));
        let child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                PROCESS_TEST_NAME,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROCESS_CHILD_ENV, "1")
            .env(PROCESS_ROOT_ENV, &root)
            .env(PROCESS_SESSION_ENV, session_id)
            .env(PROCESS_LABEL_ENV, format!("process-{writer}"))
            .env(PROCESS_READY_ENV, &ready)
            .env(PROCESS_START_ENV, &start)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        ready_paths.push(ready);
        children.push(child);
    }

    wait_for_paths(&ready_paths)?;
    std::fs::write(&start, b"start")?;
    for child in children {
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "batch process child failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ))
            .into());
        }
    }

    let entry = manager.resolve(session_id)?;
    let artifacts = crate::session::read_session_events_for_entry(&root, &entry)?;
    assert_contiguous_response_groups(&artifacts.events, PROCESS_WRITERS)
}

fn run_process_child() -> TestResult {
    let root = std::env::var_os(PROCESS_ROOT_ENV)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("batch process child has no session root"))?;
    let session_id = std::env::var(PROCESS_SESSION_ENV)?;
    let label = std::env::var(PROCESS_LABEL_ENV)?;
    let ready = std::env::var_os(PROCESS_READY_ENV)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("batch process child has no ready path"))?;
    let start = std::env::var_os(PROCESS_START_ENV)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("batch process child has no start path"))?;

    std::fs::write(ready, b"ready")?;
    wait_for_paths(std::slice::from_ref(&start))?;
    let opened = SessionManager::new(root).resume(&session_id, DurabilityPolicy::Flush)?;
    opened.store.append_batch(&response_group(&label)?)?;
    Ok(())
}

fn wait_for_paths(paths: &[std::path::PathBuf]) -> Result<(), io::Error> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !paths.iter().all(|path| path.exists()) {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "batch process barrier timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[test]
fn mid_batch_failure_retries_exact_prefix_without_duplicates() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("batch-retry.jsonl");
    let mut sink = JsonlSink::open(&path)?;
    sink.fail_after_write_once();
    let store = EventStore::with_sink(Box::new(sink));
    let group = response_group("retry")?;

    assert!(store.append_batch(&group).is_err());
    assert!(store.is_empty());
    let partial = std::fs::read_to_string(&path)?;
    assert!(partial.contains(group[0].base().id.as_str()));
    assert_eq!(partial.lines().count(), 2);
    assert!(!partial.contains("response-retry"));

    store.append_batch(&group)?;
    assert_eq!(store.len(), 3);
    let artifacts = read_session_events(temp.path(), "batch-retry")?;
    assert_eq!(
        serde_json::to_value(&artifacts.events)?,
        serde_json::to_value(&group)?
    );
    Ok(())
}

#[test]
fn mid_batch_prefix_rejects_a_different_follow_up_group() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("batch-conflict.jsonl");
    let mut sink = JsonlSink::open(&path)?;
    sink.fail_after_write_once();
    let store = EventStore::with_sink(Box::new(sink));

    assert!(store.append_batch(&response_group("first")?).is_err());
    let before = std::fs::read(&path)?;
    assert!(
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "must not extend an orphan batch".to_owned(),
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert!(store.append_batch(&response_group("different")?).is_err());
    assert_eq!(std::fs::read(&path)?, before);
    assert!(store.is_empty());
    Ok(())
}
