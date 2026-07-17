use std::sync::Arc;

use futures_util::{StreamExt as _, stream};
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use super::LoopContext;
use super::compaction::{InFlightPartial, shared_timeout_state};
use super::config::AgentStepResult;
use super::runner::{AgentLoopConfig, AgentStepRequest, MockToolExecutor, run_agent_step};
use super::stop_records::{PARTIAL_OUTPUT_EVENT_TYPE, StepStopContext, record_abnormal_step_stop};
use crate::error::ProviderError;
use crate::integration::hooks::{Hook, HookRegistry, SessionEventHook};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE,
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponseAudioArtifactState,
    SessionManager, referenced_response_audio_artifacts, response_audio_artifact_links,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct AudioProvider {
    events: Vec<ProviderEvent>,
}

struct CancelAfterAudioProvider {
    events: Vec<ProviderEvent>,
    cancel: CancellationToken,
}

struct AppendAfterAudioLink {
    store: Arc<EventStore>,
}

#[async_trait::async_trait]
impl SessionEventHook for AppendAfterAudioLink {
    async fn on_event(&self, event: &SessionEvent) {
        let SessionEvent::Custom {
            base, event_type, ..
        } = event
        else {
            return;
        };
        if event_type != RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE {
            return;
        }
        let marker = SessionEvent::Custom {
            base: EventBase::new(Some(base.id.clone())),
            event_type: "test.response_audio.link_observed".to_owned(),
            data: json!({"inserted_by": "session_event_hook"}),
        };
        let _append_succeeded = self.store.append(marker).is_ok();
    }
}

impl Provider for CancelAfterAudioProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let initial = stream::iter(self.events.clone().into_iter().map(Ok));
        let cancel = self.cancel.clone();
        let hard_cut = stream::once(async move {
            cancel.cancel();
            std::future::pending::<Result<ProviderEvent, ProviderError>>().await
        });
        Ok(Box::pin(initial.chain(hard_cut)))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

impl Provider for AudioProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        Ok(Box::pin(stream::iter(
            self.events.clone().into_iter().map(Ok),
        )))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

fn audio_frame(
    event_type: &str,
    sequence_number: u64,
    fields: &serde_json::Value,
) -> Result<[ProviderEvent; 2], Box<dyn std::error::Error>> {
    let mut raw = json!({
        "type": event_type,
        "sequence_number": sequence_number,
    });
    let object = raw
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("audio fixture was not an object"))?;
    let fields = fields
        .as_object()
        .ok_or_else(|| std::io::Error::other("audio fixture fields were not an object"))?;
    object.extend(fields.clone());
    let stream_event = ResponseStreamEvent::from_raw(raw)?;
    let event = ResponseAudioEvent::from_stream_event(&stream_event)?
        .ok_or_else(|| std::io::Error::other("audio fixture was not typed"))?;
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

fn provider() -> Result<AudioProvider, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    events.extend(audio_frame(
        "response.audio.delta",
        1,
        &json!({"delta": "YXVkaW8="}),
    )?);
    events.extend(audio_frame(
        "response.audio.transcript.delta",
        2,
        &json!({"delta": "spoken answer"}),
    )?);
    events.extend(audio_frame("response.audio.done", 3, &json!({}))?);
    events.extend(audio_frame(
        "response.audio.transcript.done",
        4,
        &json!({}),
    )?);
    events.push(ProviderEvent::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        response_id: Some("resp_audio_end_to_end".to_owned()),
    });
    Ok(AudioProvider { events })
}

fn persisted_link(
    events: &[SessionEvent],
) -> Result<ResponseAudioArtifactLink, Box<dyn std::error::Error>> {
    response_audio_artifact_links(events)?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("audio link missing").into())
}

#[tokio::test]
async fn real_agent_step_persists_audio_and_resume_resolves_it() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-agent-step",
        CreateSessionOptions {
            model: "audio-model".to_owned(),
            working_dir: "/audio-end-to-end".to_owned(),
            name: None,
        },
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let provider = provider()?;
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::default();

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &opened.store,
        user_prompt: "answer with audio",
        tools: &[],
        output_schema: None,
        model: "audio-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let link = persisted_link(&opened.store.events())?;
    let reference = link.reference();
    drop(opened);

    let resumed = manager.resume("audio-agent-step", DurabilityPolicy::FsyncPerEvent)?;
    let link = persisted_link(&resumed.store.events())?;
    assert_eq!(link.reference(), reference);
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?
        .read_linked(&link)?;
    assert_eq!(artifact.audio, b"audio");
    assert_eq!(artifact.transcript, "spoken answer");
    assert!(artifact.audio_complete);
    assert!(artifact.transcript_complete);
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[tokio::test]
async fn session_event_hook_may_append_between_audio_link_and_assistant() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-hook-ordering",
        CreateSessionOptions {
            model: "audio-model".to_owned(),
            working_dir: "/audio-end-to-end".to_owned(),
            name: None,
        },
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let store = Arc::new(opened.store);
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::SessionEvent(Box::new(AppendAfterAudioLink {
        store: Arc::clone(&store),
    })));
    let mut loop_context = LoopContext {
        hooks: Some(Arc::new(hooks)),
        ..LoopContext::default()
    };
    let provider = provider()?;
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "answer with audio",
        tools: &[],
        output_schema: None,
        model: "audio-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));

    let events = store.events();
    let link = persisted_link(&events)?;
    let link_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE
            )
        })
        .ok_or_else(|| std::io::Error::other("audio link event missing"))?;
    let marker_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                SessionEvent::Custom { event_type, .. }
                    if event_type == "test.response_audio.link_observed"
            )
        })
        .ok_or_else(|| std::io::Error::other("hook marker event missing"))?;
    let assistant_index = events
        .iter()
        .position(|event| event.base().id == *link.assistant_event_id())
        .ok_or_else(|| std::io::Error::other("linked assistant event missing"))?;
    assert!(link_index < marker_index && marker_index < assistant_index);
    assert_eq!(
        events[assistant_index].base().parent_id.as_ref(),
        Some(&events[link_index].base().id)
    );
    assert_eq!(response_audio_artifact_links(&events)?, vec![link]);

    drop(loop_context);
    drop(store);
    let resumed = manager.resume("audio-hook-ordering", DurabilityPolicy::FsyncPerEvent)?;
    assert_eq!(
        response_audio_artifact_links(&resumed.store.events())?.len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn failed_hard_cut_checkpoint_strips_the_result_and_durable_reference() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-checkpoint-source",
        CreateSessionOptions {
            model: "audio-model".to_owned(),
            working_dir: "/audio-end-to-end".to_owned(),
            name: None,
        },
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let writer = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("source audio store missing"))?
        .begin(1)?;
    let reference = writer.reference();
    drop(writer);

    let store = EventStore::new();
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
            store: &store,
            hooks: None,
            timeout_state: &timeout_state,
            elapsed: std::time::Duration::ZERO,
            step_timeout: Some(std::time::Duration::ZERO),
            max_iterations: None,
        },
        &mut result,
    )
    .await;

    let Ok(AgentStepResult::TimedOut { partial_output, .. }) = result else {
        return Err(std::io::Error::other("test outcome changed variant").into());
    };
    assert!(partial_output.is_none());
    assert!(
        timeout_state
            .lock()
            .in_flight_partial
            .as_ref()
            .is_none_or(|partial| partial.response_audio.is_none())
    );
    assert!(!store.events().iter().any(|event| matches!(
        event,
        SessionEvent::Custom { event_type, .. } if event_type == PARTIAL_OUTPUT_EVENT_TYPE
    )));
    Ok(())
}

#[tokio::test]
async fn cancellation_after_audio_frame_persists_only_unsealed_partial_reference() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-cancelled-step",
        CreateSessionOptions {
            model: "audio-model".to_owned(),
            working_dir: "/audio-end-to-end".to_owned(),
            name: None,
        },
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let cancel = CancellationToken::new();
    let provider = CancelAfterAudioProvider {
        events: Vec::from(audio_frame(
            "response.audio.delta",
            1,
            &json!({"delta": "Y2FuY2VsbGVkIGF1ZGlv"}),
        )?),
        cancel: cancel.clone(),
    };
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_context = LoopContext::default();

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &opened.store,
        user_prompt: "start an audio response",
        tools: &[],
        output_schema: None,
        model: "audio-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: Some(cancel),
    })
    .await?;
    assert!(matches!(result, AgentStepResult::Cancelled { .. }));

    let events = opened.store.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::AssistantMessage { .. }))
    );
    assert!(response_audio_artifact_links(&events)?.is_empty());
    let partial_reference = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == PARTIAL_OUTPUT_EVENT_TYPE => data.get("response_audio").cloned(),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("cancelled step omitted partial audio reference"))?;
    let reference: ResponseAudioArtifactRef = serde_json::from_value(partial_reference)?;
    assert_eq!(
        referenced_response_audio_artifacts(&events)?,
        vec![reference]
    );
    drop(opened);

    let resumed = manager.resume("audio-cancelled-step", DurabilityPolicy::FsyncPerEvent)?;
    assert_eq!(
        referenced_response_audio_artifacts(&resumed.store.events())?,
        vec![reference]
    );
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?
        .read(reference)?;
    assert_eq!(artifact.audio, b"cancelled audio");
    assert_eq!(artifact.state, ResponseAudioArtifactState::Unsealed);
    Ok(())
}
