use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use futures_util::{StreamExt as _, stream};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::classify::{call_provider, call_provider_with_retry};
use super::compaction::{InFlightPartial, shared_timeout_state};
use super::config::AgentStepResult;
use super::retry::RetryPolicy;
use super::stop_records::{PARTIAL_OUTPUT_EVENT_TYPE, StepStopContext, record_abnormal_step_stop};
use super::summarization::request_compaction_summary;
use crate::error::{NornError, ProviderError};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, ResponseAudioArtifactState, ResponseAudioStore,
    SessionManager,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type ScriptedEvent = Result<ProviderEvent, ProviderError>;

enum ScriptedAttempt {
    Complete(Vec<ScriptedEvent>),
    PendingAfter(Vec<ScriptedEvent>),
}

struct ScriptedProvider {
    attempts: Mutex<VecDeque<ScriptedAttempt>>,
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    fn new(attempts: impl IntoIterator<Item = ScriptedAttempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into_iter().collect()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl Provider for ScriptedProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let attempt =
            self.attempts
                .lock()
                .pop_front()
                .ok_or_else(|| ProviderError::StreamError {
                    reason: "scripted audio provider exhausted".to_owned(),
                    transient: None,
                })?;
        match attempt {
            ScriptedAttempt::Complete(events) => Ok(Box::pin(stream::iter(events))),
            ScriptedAttempt::PendingAfter(events) => Ok(Box::pin(
                stream::iter(events).chain(stream::pending::<ScriptedEvent>()),
            )),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "audio-test-model".to_owned(),
        working_dir: "/audio-test".to_owned(),
        name: None,
    }
}

fn empty_request() -> ProviderRequest {
    ProviderRequest {
        messages: Vec::new(),
        tools: Vec::new(),
        model: "audio-test-model".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    }
}

fn audio_events(
    event_type: &str,
    sequence_number: u64,
    fields: &Value,
) -> Result<[ProviderEvent; 2], Box<dyn std::error::Error>> {
    let mut raw = json!({
        "type": event_type,
        "sequence_number": sequence_number,
    });
    let object = raw
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("audio event fixture was not an object"))?;
    let fields = fields
        .as_object()
        .ok_or_else(|| std::io::Error::other("audio event fields were not an object"))?;
    object.extend(fields.clone());
    let stream_event = ResponseStreamEvent::from_raw(raw)?;
    let event = ResponseAudioEvent::from_stream_event(&stream_event)?
        .ok_or_else(|| std::io::Error::other("audio event fixture was not typed as audio"))?;
    Ok([
        ProviderEvent::ResponseStreamEvent {
            event: Box::new(stream_event.clone()),
        },
        ProviderEvent::ResponseAudioFrame {
            stream_event: Box::new(stream_event),
            event,
        },
    ])
}

fn push_audio(
    events: &mut Vec<ScriptedEvent>,
    event_type: &str,
    sequence_number: u64,
    fields: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    events.extend(audio_events(event_type, sequence_number, fields)?.map(Ok));
    Ok(())
}

fn done(response_id: Option<&str>) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        response_id: response_id.map(str::to_owned),
    }
}

fn required<T>(value: Option<T>, reason: &'static str) -> Result<T, std::io::Error> {
    value.ok_or_else(|| std::io::Error::other(reason))
}

fn audio_store(
    opened: &crate::session::OpenSession,
) -> Result<&ResponseAudioStore, std::io::Error> {
    opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("managed session had no response-audio store"))
}

#[tokio::test]
async fn successful_call_seals_and_returns_readable_audio_reference() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-success", options(), DurabilityPolicy::Flush)?;
    let store = audio_store(&opened)?;
    let mut events = Vec::new();
    push_audio(
        &mut events,
        "response.audio.delta",
        1,
        &json!({"delta": "YQ=="}),
    )?;
    push_audio(
        &mut events,
        "response.audio.transcript.delta",
        2,
        &json!({"delta": "spoken"}),
    )?;
    push_audio(&mut events, "response.audio.done", 3, &json!({}))?;
    push_audio(&mut events, "response.audio.transcript.done", 4, &json!({}))?;
    events.push(Ok(done(Some("resp_audio_success"))));
    let provider = ScriptedProvider::new([ScriptedAttempt::Complete(events)]);

    let response = call_provider(
        &provider,
        empty_request(),
        crate::provider::ProviderTurnContext::default(),
        None,
        None,
        Some(store),
        1,
    )
    .await?;
    let reference = required(response.response_audio, "response omitted audio reference")?;
    let artifact = store.read(reference)?;

    assert_eq!(artifact.reference, reference);
    assert_eq!(artifact.audio, b"a");
    assert_eq!(artifact.transcript, "spoken");
    assert!(artifact.audio_complete);
    assert!(artifact.transcript_complete);
    assert_eq!(artifact.response_id.as_deref(), Some("resp_audio_success"));
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    assert_eq!(provider.call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn retry_keeps_failed_attempt_unsealed_and_success_distinct_and_sealed() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-retry", options(), DurabilityPolicy::Flush)?;
    let store = audio_store(&opened)?;
    let mut first = Vec::new();
    push_audio(
        &mut first,
        "response.audio.delta",
        1,
        &json!({"delta": "YQ=="}),
    )?;
    first.push(Err(ProviderError::StreamInterrupted {
        reason: "scripted interruption".to_owned(),
    }));
    let mut second = Vec::new();
    push_audio(
        &mut second,
        "response.audio.delta",
        1,
        &json!({"delta": "Yg=="}),
    )?;
    push_audio(&mut second, "response.audio.done", 2, &json!({}))?;
    second.push(Ok(done(Some("resp_retry_success"))));
    let provider = ScriptedProvider::new([
        ScriptedAttempt::Complete(first),
        ScriptedAttempt::Complete(second),
    ]);
    let policy = RetryPolicy {
        max_retries: 1,
        initial_backoff: std::time::Duration::ZERO,
        backoff_multiplier: 1.0,
        ..RetryPolicy::default()
    };

    let response = call_provider_with_retry(
        &policy,
        &provider,
        empty_request(),
        &crate::provider::ProviderTurnContext::default(),
        None,
        None,
        Some(store),
    )
    .await?;
    let sealed_reference = required(
        response.response_audio,
        "successful retry omitted audio reference",
    )?;
    let mut artifacts = store
        .list()?
        .into_iter()
        .map(|reference| store.read(reference))
        .collect::<Result<Vec<_>, _>>()?;
    artifacts.sort_by_key(|artifact| artifact.attempt);
    let mut artifacts = artifacts.into_iter();
    let failed = required(artifacts.next(), "failed-attempt artifact missing")?;
    let succeeded = required(artifacts.next(), "successful-attempt artifact missing")?;

    assert!(artifacts.next().is_none());
    assert_eq!(failed.attempt, 1);
    assert_eq!(failed.audio, b"a");
    assert_eq!(failed.state, ResponseAudioArtifactState::Unsealed);
    assert_eq!(succeeded.attempt, 2);
    assert_eq!(succeeded.audio, b"b");
    assert_eq!(succeeded.state, ResponseAudioArtifactState::Sealed);
    assert_ne!(failed.reference, succeeded.reference);
    assert_eq!(succeeded.reference, sealed_reference);
    assert_eq!(provider.call_count(), 2);
    Ok(())
}

#[tokio::test]
async fn local_sidecar_failure_is_terminal_and_never_provider_retried() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened =
        manager.create_with_id("audio-local-failure", options(), DurabilityPolicy::Flush)?;
    let stale = audio_store(&opened)?.clone();
    drop(opened);
    manager.delete("audio-local-failure")?;
    let replacement =
        manager.create_with_id("audio-local-failure", options(), DurabilityPolicy::Flush)?;
    let mut attempt = Vec::new();
    push_audio(
        &mut attempt,
        "response.audio.delta",
        1,
        &json!({"delta": "YQ=="}),
    )?;
    attempt.push(Ok(done(None)));
    let provider = ScriptedProvider::new([
        ScriptedAttempt::Complete(attempt.clone()),
        ScriptedAttempt::Complete(attempt),
    ]);
    let policy = RetryPolicy {
        max_retries: 1,
        initial_backoff: std::time::Duration::ZERO,
        backoff_multiplier: 1.0,
        ..RetryPolicy::default()
    };

    let error = call_provider_with_retry(
        &policy,
        &provider,
        empty_request(),
        &crate::provider::ProviderTurnContext::default(),
        None,
        None,
        Some(&stale),
    )
    .await
    .err()
    .ok_or_else(|| std::io::Error::other("stale sidecar authority was accepted"))?;

    assert!(matches!(error, NornError::Session(_)));
    assert_eq!(provider.call_count(), 1);
    assert!(audio_store(&replacement)?.list()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn summarization_rejects_audio_with_typed_unsupported_media_error() -> TestResult {
    let mut events = Vec::new();
    push_audio(
        &mut events,
        "response.audio.delta",
        1,
        &json!({"delta": "YQ=="}),
    )?;
    events.push(Ok(done(None)));
    let provider = ScriptedProvider::new([ScriptedAttempt::Complete(events)]);

    let error = request_compaction_summary(&provider, "audio-test-model", &[])
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("sinkless summarization accepted audio"))?;

    assert!(matches!(
        error,
        NornError::Provider(ProviderError::UnsupportedResponseMedia)
    ));
    assert_eq!(provider.call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn dropped_pending_call_keeps_unsealed_reference_in_timeout_state() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-cancel", options(), DurabilityPolicy::Flush)?;
    let store = audio_store(&opened)?;
    let mut events = Vec::new();
    push_audio(
        &mut events,
        "response.audio.transcript.delta",
        1,
        &json!({"delta": "partial speech"}),
    )?;
    let provider = ScriptedProvider::new([ScriptedAttempt::PendingAfter(events)]);
    let timeout_state = shared_timeout_state();

    {
        let call = call_provider(
            &provider,
            empty_request(),
            crate::provider::ProviderTurnContext::default(),
            None,
            Some(&timeout_state),
            Some(store),
            1,
        );
        tokio::pin!(call);
        assert!(matches!(futures_util::poll!(&mut call), Poll::Pending));
    }

    let reference = {
        let snapshot = timeout_state.lock();
        let partial = required(
            snapshot.in_flight_partial.as_ref(),
            "pending call did not retain partial state",
        )?;
        required(
            partial.response_audio,
            "pending call did not retain audio reference",
        )?
    };
    let artifact = store.read(reference)?;

    assert_eq!(artifact.reference, reference);
    assert_eq!(artifact.transcript, "partial speech");
    assert_eq!(artifact.state, ResponseAudioArtifactState::Unsealed);
    assert_eq!(provider.call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn hard_cut_stop_record_persists_the_unsealed_audio_reference() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened =
        manager.create_with_id("audio-hard-cut", options(), DurabilityPolicy::FsyncPerEvent)?;
    let store = audio_store(&opened)?;
    let mut writer = store.begin(1)?;
    let [_, typed] = audio_events(
        "response.audio.transcript.delta",
        1,
        &json!({"delta": "partial speech"}),
    )?;
    let ProviderEvent::ResponseAudioFrame {
        stream_event,
        event,
    } = typed
    else {
        return Err(std::io::Error::other("typed fixture was not an audio frame").into());
    };
    writer.append(&stream_event, &event)?;
    let reference = writer.reference();
    drop(writer);

    let timeout_state = shared_timeout_state();
    timeout_state.lock().in_flight_partial = Some(InFlightPartial {
        response_audio: Some(reference),
        ..InFlightPartial::default()
    });
    let mut result = Ok(AgentStepResult::TimedOut {
        elapsed: std::time::Duration::ZERO,
        iterations: 0,
        partial_output: Some(json!({"response_audio": reference})),
        usage: Usage::default(),
        children_usage: Usage::default(),
    });
    record_abnormal_step_stop(
        StepStopContext {
            store: &opened.store,
            hooks: None,
            timeout_state: &timeout_state,
            elapsed: std::time::Duration::ZERO,
            step_timeout: Some(std::time::Duration::ZERO),
            max_iterations: None,
        },
        &mut result,
    )
    .await;

    drop(opened);
    let resumed = manager.resume("audio-hard-cut", DurabilityPolicy::FsyncPerEvent)?;
    let recorded_reference = resumed
        .store
        .events()
        .into_iter()
        .find_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == PARTIAL_OUTPUT_EVENT_TYPE => data.get("response_audio").cloned(),
            _ => None,
        });
    assert_eq!(recorded_reference, Some(json!(reference)));
    assert_eq!(
        audio_store(&resumed)?.read(reference)?.state,
        ResponseAudioArtifactState::Unsealed
    );
    Ok(())
}
