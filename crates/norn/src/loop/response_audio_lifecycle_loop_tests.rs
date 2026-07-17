use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::stream;
use serde_json::json;
use tempfile::tempdir;

use super::LoopContext;
use super::config::AgentStepResult;
use super::runner::{AgentLoopConfig, AgentStepRequest, MockToolExecutor, run_agent_step};
use super::stop_records::PARTIAL_OUTPUT_EVENT_TYPE;
use crate::error::ProviderError;
use crate::integration::hooks::{Hook, HookRegistry, LlmCallSummary, PostLlmHook};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::request::ProviderRequest;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::tools::ProviderCapabilities;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::response_audio::response_audio_artifact_path;
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE,
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponseAudioArtifactState,
    SessionManager, referenced_response_audio_artifacts, response_audio_artifact_links,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct LifecycleAudioProvider {
    events: Vec<ProviderEvent>,
}

impl Provider for LifecycleAudioProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        Ok(Box::pin(stream::iter(
            self.events.clone().into_iter().map(Ok),
        )))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

struct HangingPostLlm {
    entered: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl PostLlmHook for HangingPostLlm {
    async fn after_llm(&self, _summary: &LlmCallSummary) {
        self.entered.store(true, Ordering::Release);
        std::future::pending::<()>().await;
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

fn provider(
    response_id: Option<&str>,
) -> Result<LifecycleAudioProvider, Box<dyn std::error::Error>> {
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
        response_id: response_id.map(str::to_owned),
    });
    Ok(LifecycleAudioProvider { events })
}

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "audio-model".to_owned(),
        working_dir: "/audio-lifecycle-loop".to_owned(),
        name: None,
    }
}

async fn run_step(
    provider: &LifecycleAudioProvider,
    store: &crate::session::EventStore,
    config: &AgentLoopConfig,
    loop_context: &mut LoopContext,
) -> Result<AgentStepResult, crate::error::NornError> {
    run_agent_step(AgentStepRequest {
        provider,
        executor: &MockToolExecutor::empty(),
        store,
        user_prompt: "answer with audio",
        tools: &[],
        output_schema: None,
        model: "audio-model",
        config,
        event_tx: None,
        inbound: None,
        loop_context,
        cancel: None,
    })
    .await
}

fn one_link(
    events: &[SessionEvent],
) -> Result<ResponseAudioArtifactLink, Box<dyn std::error::Error>> {
    let links = response_audio_artifact_links(events)?;
    if links.len() != 1 {
        return Err(
            std::io::Error::other("timeline did not contain exactly one audio link").into(),
        );
    }
    links
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("audio link missing").into())
}

fn partial_reference(
    events: &[SessionEvent],
) -> Result<ResponseAudioArtifactRef, Box<dyn std::error::Error>> {
    let references = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == PARTIAL_OUTPUT_EVENT_TYPE => data.get("response_audio").cloned(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if references.len() != 1 {
        return Err(std::io::Error::other(
            "timeline did not contain exactly one partial audio reference",
        )
        .into());
    }
    serde_json::from_value(
        references
            .into_iter()
            .next()
            .ok_or_else(|| std::io::Error::other("partial audio reference missing"))?,
    )
    .map_err(Into::into)
}

fn assert_no_audio_turn(events: &[SessionEvent]) -> TestResult {
    assert!(response_audio_artifact_links(events)?.is_empty());
    assert!(!events.iter().any(|event| matches!(
        event,
        SessionEvent::Custom { event_type, .. }
            if event_type == RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::AssistantMessage { .. }))
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn post_llm_hard_cut_preserves_sealed_audio_without_publishing_turn() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-post-llm-hard-cut",
        options(),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let entered = Arc::new(AtomicBool::new(false));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PostLlm(Box::new(HangingPostLlm {
        entered: Arc::clone(&entered),
    })));
    let mut loop_context = LoopContext {
        hooks: Some(Arc::new(hooks)),
        ..LoopContext::default()
    };
    let config = AgentLoopConfig {
        step_timeout: Some(Duration::from_secs(5)),
        ..AgentLoopConfig::default()
    };

    let result = run_step(
        &provider(Some("resp_audio_hard_cut"))?,
        &opened.store,
        &config,
        &mut loop_context,
    )
    .await?;
    assert!(matches!(result, AgentStepResult::TimedOut { .. }));
    assert!(entered.load(Ordering::Acquire));
    let reference = partial_reference(&opened.store.events())?;
    assert_eq!(
        referenced_response_audio_artifacts(&opened.store.events())?,
        vec![reference]
    );
    assert_no_audio_turn(&opened.store.events())?;
    drop(loop_context);
    drop(opened);

    let resumed = manager.resume("audio-post-llm-hard-cut", DurabilityPolicy::FsyncPerEvent)?;
    assert_eq!(
        referenced_response_audio_artifacts(&resumed.store.events())?,
        vec![reference]
    );
    assert_no_audio_turn(&resumed.store.events())?;
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?
        .read(reference)?;
    assert_eq!(artifact.audio, b"audio");
    assert_eq!(artifact.transcript, "spoken answer");
    assert_eq!(artifact.response_id.as_deref(), Some("resp_audio_hard_cut"));
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[tokio::test]
async fn absent_response_id_survives_sidecar_link_assistant_and_resume() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-no-response-id",
        options(),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let mut loop_context = LoopContext::default();

    let result = run_step(
        &provider(None)?,
        &opened.store,
        &AgentLoopConfig::default(),
        &mut loop_context,
    )
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let link = one_link(&opened.store.events())?;
    assert!(link.response_id().is_none());
    let assistant = opened
        .store
        .events()
        .into_iter()
        .find(|event| event.base().id == *link.assistant_event_id())
        .ok_or_else(|| std::io::Error::other("linked assistant event missing"))?;
    assert!(matches!(
        assistant,
        SessionEvent::AssistantMessage {
            response_id: None,
            ..
        }
    ));
    drop(opened);

    let resumed = manager.resume("audio-no-response-id", DurabilityPolicy::FsyncPerEvent)?;
    let link = one_link(&resumed.store.events())?;
    assert!(link.response_id().is_none());
    let assistant = resumed
        .store
        .events()
        .into_iter()
        .find(|event| event.base().id == *link.assistant_event_id())
        .ok_or_else(|| std::io::Error::other("resumed linked assistant event missing"))?;
    assert!(matches!(
        assistant,
        SessionEvent::AssistantMessage {
            response_id: None,
            ..
        }
    ));
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?
        .read_linked(&link)?;
    assert!(artifact.response_id.is_none());
    assert_eq!(artifact.audio, b"audio");
    assert_eq!(artifact.transcript, "spoken answer");
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[tokio::test]
async fn completed_audio_step_writes_one_final_terminal_jsonl_record() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-one-terminal-record",
        options(),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let mut loop_context = LoopContext::default();

    let result = run_step(
        &provider(Some("resp_audio_one_terminal"))?,
        &opened.store,
        &AgentLoopConfig::default(),
        &mut loop_context,
    )
    .await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let link = one_link(&opened.store.events())?;
    let path = directory.path().join(response_audio_artifact_path(
        "audio-one-terminal-record",
        link.reference(),
    ));
    let contents = std::fs::read_to_string(path)?;
    assert!(contents.ends_with('\n'));
    let records = contents
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let terminal_count = records
        .iter()
        .filter(|record| {
            record.get("record").and_then(serde_json::Value::as_str) == Some("terminal")
        })
        .count();
    assert_eq!(terminal_count, 1);
    assert_eq!(
        records
            .last()
            .and_then(|record| record.get("record"))
            .and_then(serde_json::Value::as_str),
        Some("terminal")
    );
    Ok(())
}
