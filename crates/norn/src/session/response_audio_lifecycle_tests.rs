use std::sync::Arc;

use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use super::events::{ChildBranchKind, EventBase, EventUsage, SessionEvent};
use super::{
    ChildBranchRequest, ChildDurability, CreateSessionOptions, DurabilityPolicy,
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponseAudioArtifactState,
    ResponseAudioStore, SessionBinding, SessionBrancher, SessionManager,
    response_audio_artifact_links,
};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn options(model: &str) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: "/audio-lifecycle".to_owned(),
        name: None,
    }
}

fn write_audio(
    store: &ResponseAudioStore,
) -> Result<ResponseAudioArtifactRef, Box<dyn std::error::Error>> {
    let mut writer = store.begin(1)?;
    let raw = ResponseStreamEvent::from_raw(json!({
        "type": "response.audio.transcript.delta",
        "sequence_number": 1,
        "delta": "durable speech",
    }))?;
    let event = ResponseAudioEvent::from_stream_event(&raw)?
        .ok_or_else(|| std::io::Error::other("audio fixture was not typed"))?;
    writer.append(&raw, &event)?;
    Ok(writer.seal(Some("resp_audio_lifecycle"))?)
}

fn audio_turn(
    parent_id: Option<super::events::EventId>,
    reference: ResponseAudioArtifactRef,
) -> Result<[SessionEvent; 2], serde_json::Error> {
    let link_base = EventBase::new(parent_id);
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let link = ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some("resp_audio_lifecycle".to_owned()),
    )
    .into_custom_event(link_base)?;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_audio_lifecycle".to_owned()),
    };
    Ok([link, assistant])
}

fn persisted_link(
    events: &[SessionEvent],
) -> Result<ResponseAudioArtifactLink, Box<dyn std::error::Error>> {
    response_audio_artifact_links(events)?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("audio link missing").into())
}

#[test]
fn root_resume_preserves_and_resolves_response_audio() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-resume-root",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let audio_store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("root audio store missing"))?;
    let reference = write_audio(audio_store)?;
    for event in audio_turn(opened.store.last_event_id(), reference)? {
        opened.store.append(event)?;
    }
    drop(opened);

    let resumed = manager.resume("audio-resume-root", DurabilityPolicy::Flush)?;
    let link = persisted_link(&resumed.store.events())?;
    assert_eq!(link.reference(), reference);
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed root audio store missing"))?
        .read_linked(&link)?;
    assert_eq!(artifact.transcript, "durable speech");
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[test]
fn persistent_spawn_shares_root_artifacts_and_resolves_after_resume() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let root = manager.create_with_id(
        "audio-spawn-root",
        options("root-model"),
        DurabilityPolicy::Flush,
    )?;
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        root.entry.id.clone(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &root.entry, &root.store.events());
    let child_id = Uuid::new_v4().to_string();
    let child = binding.branch_child(
        &root.store,
        &ChildBranchRequest {
            child_session_id: child_id.clone(),
            name_stem: "spawn".to_owned(),
            kind: ChildBranchKind::Spawn,
            durability: ChildDurability::Persist,
            model: "child-model".to_owned(),
            working_dir: "/audio-lifecycle".to_owned(),
        },
    )?;
    let audio_store = child
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("child audio store missing"))?;
    let reference = write_audio(audio_store)?;
    for event in audio_turn(child.store.last_event_id(), reference)? {
        child.store.append(event)?;
    }
    drop(child);
    drop(root);

    let resumed = manager.resume(&child_id, DurabilityPolicy::Flush)?;
    let link = persisted_link(&resumed.store.events())?;
    assert_eq!(link.reference(), reference);
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed child audio store missing"))?
        .read_linked(&link)?;
    assert_eq!(artifact.transcript, "durable speech");
    assert_eq!(artifact.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[test]
fn resume_preserves_honest_orphan_link_after_precursor_crash_gap() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-orphan-link",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let reference = write_audio(
        opened
            .store
            .response_audio()
            .ok_or_else(|| std::io::Error::other("root audio store missing"))?,
    )?;
    let [link, _assistant] = audio_turn(opened.store.last_event_id(), reference)?;
    opened.store.append(link)?;
    drop(opened);

    let resumed = manager.resume("audio-orphan-link", DurabilityPolicy::Flush)?;
    let link = persisted_link(&resumed.store.events())?;
    assert_eq!(link.reference(), reference);
    let artifact = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?
        .read_linked(&link)?;
    assert_eq!(artifact.transcript, "durable speech");
    Ok(())
}

#[test]
fn linked_read_rejects_terminal_response_id_mismatch() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-link-id-mismatch",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let audio_store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("root audio store missing"))?;
    let reference = write_audio(audio_store)?;
    let mismatched = ResponseAudioArtifactLink::new(
        EventBase::new(None).id,
        reference,
        Some("resp_other".to_owned()),
    );
    assert!(matches!(
        audio_store.read_linked(&mismatched),
        Err(super::SessionPersistError::InvalidResponseAudioArtifact { .. })
    ));
    Ok(())
}

#[test]
fn resume_is_structural_but_missing_sidecar_fails_on_link_resolution() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id(
        "audio-missing-sidecar",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let reference: ResponseAudioArtifactRef =
        serde_json::from_value(json!("123e4567-e89b-42d3-a456-426614174000"))?;
    for event in audio_turn(opened.store.last_event_id(), reference)? {
        opened.store.append(event)?;
    }
    drop(opened);

    let resumed = manager.resume("audio-missing-sidecar", DurabilityPolicy::Flush)?;
    let link = persisted_link(&resumed.store.events())?;
    let audio_store = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed audio store missing"))?;
    assert!(audio_store.read_linked(&link).is_err());
    Ok(())
}
