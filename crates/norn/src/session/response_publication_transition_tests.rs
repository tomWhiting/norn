use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use crate::error::SessionError;
use crate::session::events::{EventBase, EventUsage, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, JsonlSink, PersistenceSink,
    ProviderStateProvenance, ResponseAudioArtifactLink, ResponseAudioArtifactRef, SessionManager,
    SessionPersistError, read_session_events, seal_response_publication_group,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestResultWith<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
struct CountingSink {
    calls: Arc<AtomicUsize>,
}

impl CountingSink {
    fn with_counter(calls: Arc<AtomicUsize>) -> Self {
        Self { calls }
    }
}

impl PersistenceSink for CountingSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn persist_batch(&mut self, _events: &[SessionEvent]) -> Result<(), SessionPersistError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn assistant(base: EventBase, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base,
        response_items: Vec::new(),
        content: "transition answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

fn legacy_direct_group(
    parent_id: Option<crate::session::events::EventId>,
) -> TestResultWith<Vec<SessionEvent>> {
    let boundary_base = EventBase::new(parent_id);
    let provenance_base = EventBase::new(Some(boundary_base.id.clone()));
    let assistant_base = EventBase::new(Some(provenance_base.id.clone()));
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    Ok(vec![
        SessionEvent::ProviderEpochBoundary {
            base: boundary_base,
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
        },
        provenance,
        assistant(assistant_base, "resp_transition_direct"),
    ])
}

fn legacy_audio_group() -> TestResultWith<Vec<SessionEvent>> {
    let boundary_base = EventBase::new(None);
    let provenance_base = EventBase::new(Some(boundary_base.id.clone()));
    let link_base = EventBase::new(Some(provenance_base.id.clone()));
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let response_id = "resp_transition_audio";
    let provenance = ProviderStateProvenance::new(assistant_base.id.clone(), true)
        .into_custom_event(provenance_base)?;
    let artifact: ResponseAudioArtifactRef =
        serde_json::from_value(json!("123e4567-e89b-42d3-a456-426614174000"))?;
    let link = ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        artifact,
        Some(response_id.to_owned()),
    )
    .into_custom_event(link_base)?;
    Ok(vec![
        SessionEvent::ProviderEpochBoundary {
            base: boundary_base,
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
        },
        provenance,
        link,
        assistant(assistant_base, response_id),
    ])
}

fn committed(mut group: Vec<SessionEvent>) -> TestResultWith<Vec<SessionEvent>> {
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn write_fsynced_prefix(path: &Path, events: &[SessionEvent], rows: usize) -> TestResult {
    drop(JsonlSink::open_with(path, DurabilityPolicy::FsyncPerEvent)?);
    let prefix = events
        .get(..rows)
        .ok_or_else(|| std::io::Error::other("response prefix exceeds its group"))?;
    let mut file = OpenOptions::new().append(true).open(path)?;
    for event in prefix {
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn assert_append_error(error: &SessionError) {
    assert!(matches!(
        error,
        SessionError::EventAppendFailed { .. } | SessionError::StorageError { .. }
    ));
}

#[test]
fn single_append_rejects_response_boundaries_before_sink_or_memory_mutation() -> TestResult {
    let legacy = legacy_direct_group(None)?;
    let committed = committed(legacy.clone())?;
    for boundary in [legacy[0].clone(), committed[0].clone()] {
        let sinkless = EventStore::new();
        let error = sinkless
            .append(boundary.clone())
            .err()
            .ok_or_else(|| std::io::Error::other("sinkless boundary append succeeded"))?;
        assert_append_error(&error);
        assert!(sinkless.is_empty());

        let calls = Arc::new(AtomicUsize::new(0));
        let store = EventStore::with_sink(Box::new(CountingSink::with_counter(Arc::clone(&calls))));
        let error = store
            .append(boundary)
            .err()
            .ok_or_else(|| std::io::Error::other("custom-sink boundary append succeeded"))?;
        assert_append_error(&error);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(store.is_empty());
    }
    Ok(())
}

#[test]
fn ordinary_single_events_still_reach_a_custom_sink() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let store = EventStore::with_sink(Box::new(CountingSink::with_counter(Arc::clone(&calls))));
    let user = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "ordinary".to_owned(),
    };
    let user_id = user.base().id.clone();
    store.append(user)?;
    store.append(SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(Some(user_id)),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    })?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.len(), 2);
    Ok(())
}

#[test]
fn custom_store_rejects_suffix_only_legacy_orphan_completion() -> TestResult {
    for group in [legacy_direct_group(None)?, legacy_audio_group()?] {
        for cut in 1..group.len() {
            let sinkless = EventStore::new();
            for event in &group[..cut] {
                sinkless.append_unvalidated_for_test(event.clone())?;
            }
            let before = serde_json::to_value(sinkless.events())?;
            let error = sinkless
                .append_batch(&group[cut..])
                .err()
                .ok_or_else(|| std::io::Error::other("sinkless store completed a legacy orphan"))?;
            assert_append_error(&error);
            assert_eq!(serde_json::to_value(sinkless.events())?, before);

            let calls = Arc::new(AtomicUsize::new(0));
            let store = EventStore::with_sink_and_events(
                Box::new(CountingSink::with_counter(Arc::clone(&calls))),
                group[..cut].to_vec(),
            );
            let before = serde_json::to_value(store.events())?;
            let error = store
                .append_batch(&group[cut..])
                .err()
                .ok_or_else(|| std::io::Error::other("custom sink completed a legacy orphan"))?;
            assert_append_error(&error);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(serde_json::to_value(store.events())?, before);
        }
    }
    Ok(())
}

#[test]
fn custom_store_single_append_rejects_legacy_orphan_completion() -> TestResult {
    let group = legacy_direct_group(None)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let store = EventStore::with_sink_and_events(
        Box::new(CountingSink::with_counter(Arc::clone(&calls))),
        group[..1].to_vec(),
    );
    let before = serde_json::to_value(store.events())?;
    let error = store
        .append(group[1].clone())
        .err()
        .ok_or_else(|| std::io::Error::other("custom sink completed a legacy orphan"))?;
    assert_append_error(&error);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(serde_json::to_value(store.events())?, before);
    Ok(())
}

#[test]
fn jsonl_sink_rejects_suffix_only_legacy_orphan_completion_without_writes() -> TestResult {
    let temp = tempfile::tempdir()?;
    for (kind, group) in [
        ("direct", legacy_direct_group(None)?),
        ("audio", legacy_audio_group()?),
    ] {
        for cut in 1..group.len() {
            let path = temp.path().join(format!("legacy-{kind}-{cut}.jsonl"));
            write_fsynced_prefix(&path, &group, cut)?;
            let before = fs::read(&path)?;
            let sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
            let store = EventStore::with_sink(Box::new(sink));
            let error = store
                .append_batch(&group[cut..])
                .err()
                .ok_or_else(|| std::io::Error::other("JSONL sink completed a legacy orphan"))?;
            assert_append_error(&error);
            assert_eq!(fs::read(&path)?, before);
            assert!(store.is_empty());
        }
    }

    for (kind, group) in [
        ("direct", legacy_direct_group(None)?),
        ("audio", legacy_audio_group()?),
    ] {
        for cut in 1..group.len() {
            let registered = tempfile::tempdir()?;
            let manager = SessionManager::new(registered.path());
            let session_id = format!("legacy-{kind}-{cut}");
            let opened = manager.create_with_id(
                &session_id,
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                DurabilityPolicy::FsyncPerEvent,
            )?;
            let entry = opened.entry.clone();
            drop(opened);
            let path =
                crate::session::persistence::resolved_session_file_path(registered.path(), &entry);
            write_fsynced_prefix(&path, &group, cut)?;
            let before = fs::read(&path)?;
            let sink = JsonlSink::open_registered(
                registered.path(),
                &entry,
                DurabilityPolicy::FsyncPerEvent,
                None,
            )?;
            let store = EventStore::with_sink(Box::new(sink));
            let error = store.append_batch(&group[cut..]).err().ok_or_else(|| {
                std::io::Error::other("registered sink completed a legacy orphan")
            })?;
            assert_append_error(&error);
            assert_eq!(fs::read(path)?, before);
            assert!(store.is_empty());
        }
    }
    Ok(())
}

#[test]
fn complete_legacy_group_can_precede_a_new_committed_group() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("complete-legacy.jsonl");
    let legacy = legacy_direct_group(None)?;
    write_fsynced_prefix(&path, &legacy, legacy.len())?;
    let parent_id = legacy
        .last()
        .map(|event| event.base().id.clone())
        .ok_or_else(|| std::io::Error::other("legacy fixture is empty"))?;
    let next = committed(legacy_direct_group(Some(parent_id))?)?;
    let sink = JsonlSink::open_with(&path, DurabilityPolicy::FsyncPerEvent)?;
    EventStore::with_sink(Box::new(sink)).append_batch(&next)?;

    let observed = read_session_events(temp.path(), "complete-legacy")?;
    let expected = legacy.into_iter().chain(next).collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(observed.events)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}
