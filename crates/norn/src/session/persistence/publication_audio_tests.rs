use std::io::{self, Write as _};

use chrono::Utc;
use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::session::events::{EventBase, EventUsage};
use crate::session::persistence::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
};
use crate::session::store::DurabilityPolicy;
use crate::session::{
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponseAudioArtifactState,
    ResponseAudioReferenceError, ResponseAudioStore,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
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

fn linked_turn(
    reference: ResponseAudioArtifactRef,
    response_id: Option<&str>,
) -> Result<[SessionEvent; 2], serde_json::Error> {
    let link_base = EventBase::new(None);
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let response_id = response_id.map(str::to_owned);
    let link =
        ResponseAudioArtifactLink::new(assistant_base.id.clone(), reference, response_id.clone())
            .into_custom_event(link_base)?;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: "audio response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id,
    };
    Ok([link, assistant])
}

fn partial_output(reference: ResponseAudioArtifactRef) -> SessionEvent {
    SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "loop.partial_output".to_owned(),
        data: json!({
            "hard_cut": true,
            "response_audio": reference,
        }),
    }
}

fn source_with_audio(
    data_dir: &Path,
    id: &str,
) -> Result<(SessionIndexEntry, ResponseAudioArtifactRef), SessionPersistError> {
    let source = publish_new_session(data_dir, &entry(id), &[], None)?;
    let store = ResponseAudioStore::for_session(data_dir, &source, DurabilityPolicy::Flush, None);
    let mut writer = store.begin(1)?;
    let raw = ResponseStreamEvent::from_raw(json!({
        "type": "response.audio.delta",
        "sequence_number": 1,
        "delta": "YXVkaW8=",
    }))
    .map_err(|error| SessionPersistError::EventStore(error.to_string()))?;
    let event = ResponseAudioEvent::from_stream_event(&raw)
        .map_err(|error| SessionPersistError::EventStore(error.to_string()))?
        .ok_or_else(|| SessionPersistError::EventStore("audio fixture was not audio".to_owned()))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(Some("resp_audio"))?;
    Ok((source, reference))
}

fn source_with_unsealed_audio(
    data_dir: &Path,
    id: &str,
) -> Result<(SessionIndexEntry, ResponseAudioArtifactRef), SessionPersistError> {
    let source = publish_new_session(data_dir, &entry(id), &[], None)?;
    let store = ResponseAudioStore::for_session(data_dir, &source, DurabilityPolicy::Flush, None);
    let mut writer = store.begin(1)?;
    let raw = ResponseStreamEvent::from_raw(json!({
        "type": "response.audio.delta",
        "sequence_number": 1,
        "delta": "YXVkaW8=",
    }))
    .map_err(|error| SessionPersistError::EventStore(error.to_string()))?;
    let event = ResponseAudioEvent::from_stream_event(&raw)
        .map_err(|error| SessionPersistError::EventStore(error.to_string()))?
        .ok_or_else(|| SessionPersistError::EventStore("audio fixture was not audio".to_owned()))?;
    writer.append(&raw, &event)?;
    let reference = writer.reference();
    drop(writer);
    store.checkpoint_reference(reference)?;
    Ok((source, reference))
}

#[test]
fn audio_fork_deduplicates_references_and_survives_source_artifact_deletion() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let mut events = linked_turn(reference, Some("resp_audio"))?.to_vec();
    events.push(partial_output(reference));
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::JournalPublished {
                Err(io::Error::other("simulated process termination").into())
            } else {
                Ok(())
            }
        },
    );
    if result.is_ok() {
        return Err(io::Error::other("journal checkpoint did not stop publication").into());
    }
    std::fs::remove_dir_all(directory.path().join(&source.id))?;

    let rows = super::super::read_index(directory.path())?;
    let recovered = rows
        .iter()
        .find(|row| row.id == destination.id)
        .ok_or_else(|| io::Error::other("recovered destination row is missing"))?;
    let store =
        ResponseAudioStore::for_session(directory.path(), recovered, DurabilityPolicy::Flush, None);
    assert_eq!(store.list()?, vec![reference]);
    assert_eq!(store.read(reference)?.audio, b"audio");
    Ok(())
}

#[test]
fn every_durable_audio_publication_checkpoint_recovers_without_a_dangling_reference() -> TestResult
{
    for stopped_at in [
        PublicationCheckpoint::JournalPublished,
        PublicationCheckpoint::AudioPublished,
        PublicationCheckpoint::TimelinePublished,
        PublicationCheckpoint::IndexPublished,
    ] {
        let directory = tempdir()?;
        let (source, reference) = source_with_audio(directory.path(), "source")?;
        let destination = entry(&format!("destination-{stopped_at:?}"));
        let events = linked_turn(reference, Some("resp_audio"))?;
        let result = publish_new_fork_session_with_hook(
            directory.path(),
            &destination,
            &events,
            &source,
            None,
            &mut |checkpoint| {
                if checkpoint == stopped_at {
                    Err(io::Error::other("simulated process termination").into())
                } else {
                    Ok(())
                }
            },
        );
        if result.is_ok() {
            return Err(io::Error::other("durable checkpoint did not stop publication").into());
        }
        let rows = super::super::read_index(directory.path())?;
        let recovered = rows
            .iter()
            .find(|row| row.id == destination.id)
            .ok_or_else(|| io::Error::other("recovered destination row is missing"))?;
        let store = ResponseAudioStore::for_session(
            directory.path(),
            recovered,
            DurabilityPolicy::Flush,
            None,
        );
        assert_eq!(store.read(reference)?.audio, b"audio");
    }
    Ok(())
}

#[test]
fn pre_journal_audio_stage_is_inert_and_reclaimed() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_audio"))?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::AudioStaged {
                Err(io::Error::other("simulated process termination").into())
            } else {
                Ok(())
            }
        },
    );
    if result.is_ok() {
        return Err(io::Error::other("audio stage checkpoint did not stop publication").into());
    }
    let rows = super::super::read_index(directory.path())?;
    assert!(!rows.iter().any(|row| row.id == destination.id));
    assert!(!directory.path().join(&destination.id).exists());
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn foreign_destination_directory_blocks_publication_without_index_visibility() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_audio"))?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &events,
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::JournalPublished {
                std::fs::create_dir(directory.path().join(&destination.id))?;
            }
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(SessionPersistError::PublicationConflict { .. })
    ));
    assert!(
        !directory
            .path()
            .join(format!("{}.jsonl", destination.id))
            .exists()
    );
    assert!(directory.path().join(&destination.id).is_dir());
    Ok(())
}

#[test]
fn stale_source_generation_is_rejected_before_any_stage_is_created() -> TestResult {
    let directory = tempdir()?;
    let (mut source, reference) = source_with_audio(directory.path(), "source")?;
    source.generation = uuid::Uuid::new_v4();
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_audio"))?;
    let result = publish_new_fork_session(directory.path(), &destination, &events, &source, None);
    assert!(matches!(
        result,
        Err(SessionPersistError::GenerationChanged { id }) if id == source.id
    ));
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn fork_preserves_response_audio_link_order_diagnostic() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let [link, assistant] = linked_turn(reference, Some("resp_audio"))?;
    let assistant_event_id = assistant.base().id.as_str().to_owned();
    let events = [assistant, link];

    let error = publish_new_fork_session(directory.path(), &destination, &events, &source, None)
        .err()
        .ok_or_else(|| {
            std::io::Error::other("fork accepted a response-audio link after its assistant")
        })?;
    match &error {
        SessionPersistError::InvalidResponseAudioReference(
            ResponseAudioReferenceError::LinkDoesNotPrecedeAssistant {
                assistant_event_id: actual,
            },
        ) => assert_eq!(actual, &assistant_event_id),
        other => {
            return Err(std::io::Error::other(format!(
                "fork returned an unexpected error: {other}"
            ))
            .into());
        }
    }
    assert_eq!(
        error.to_string(),
        format!(
            "invalid response-audio transcript association: response.audio.artifact does not \
             precede assistant event {assistant_event_id}"
        )
    );
    assert!(!directory.path().join(&destination.id).exists());
    assert!(
        !directory
            .path()
            .join(format!("{}.jsonl", destination.id))
            .exists()
    );
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn invalid_source_sidecar_is_rejected_before_journal_publication() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let source_path = directory
        .path()
        .join(&source.id)
        .join("artifacts/response-audio")
        .join(reference.file_name());
    std::fs::OpenOptions::new()
        .append(true)
        .open(source_path)?
        .write_all(b"{}\n")?;
    let destination = entry("destination");

    let events = linked_turn(reference, Some("resp_audio"))?;
    let result = publish_new_fork_session(directory.path(), &destination, &events, &source, None);
    assert!(matches!(
        result,
        Err(SessionPersistError::InvalidResponseAudioArtifact { .. })
    ));
    assert!(!directory.path().join(&destination.id).exists());
    assert!(
        !directory
            .path()
            .join(format!("{}.jsonl", destination.id))
            .exists()
    );
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn missing_source_sidecar_fails_closed_before_journal_publication() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    std::fs::remove_file(
        directory
            .path()
            .join(&source.id)
            .join("artifacts/response-audio")
            .join(reference.file_name()),
    )?;
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_audio"))?;

    let result = publish_new_fork_session(directory.path(), &destination, &events, &source, None);
    assert!(matches!(
        result,
        Err(SessionPersistError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!directory.path().join(&destination.id).exists());
    assert!(
        !directory
            .path()
            .join(format!("{}.jsonl", destination.id))
            .exists()
    );
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn linked_response_id_must_match_the_sealed_sidecar_terminal() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_other"))?;

    let result = publish_new_fork_session(directory.path(), &destination, &events, &source, None);
    assert!(matches!(
        result,
        Err(SessionPersistError::InvalidResponseAudioArtifact { .. })
    ));
    assert!(!directory.path().join(&destination.id).exists());
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn typed_link_rejects_an_unsealed_sidecar() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_unsealed_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let events = linked_turn(reference, Some("resp_audio"))?;

    let result = publish_new_fork_session(directory.path(), &destination, &events, &source, None);
    assert!(matches!(
        result,
        Err(SessionPersistError::InvalidResponseAudioArtifact { .. })
    ));
    assert!(!directory.path().join(&destination.id).exists());
    assert!(!has_audio_publication_stage(directory.path())?);
    Ok(())
}

#[test]
fn hard_cut_partial_reference_preserves_an_unsealed_sidecar() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_unsealed_audio(directory.path(), "source")?;
    let destination = entry("destination");

    let published = publish_new_fork_session(
        directory.path(),
        &destination,
        &[partial_output(reference)],
        &source,
        None,
    )?;
    let store = ResponseAudioStore::for_session(
        directory.path(),
        &published,
        DurabilityPolicy::Flush,
        None,
    );
    assert_eq!(
        store.read(reference)?.state,
        ResponseAudioArtifactState::Unsealed
    );
    Ok(())
}

#[test]
fn orphan_precursor_link_recovers_from_stage_after_source_deletion() -> TestResult {
    let directory = tempdir()?;
    let (source, reference) = source_with_audio(directory.path(), "source")?;
    let destination = entry("destination");
    let [link, _assistant] = linked_turn(reference, Some("resp_audio"))?;
    let result = publish_new_fork_session_with_hook(
        directory.path(),
        &destination,
        &[link],
        &source,
        None,
        &mut |checkpoint| {
            if checkpoint == PublicationCheckpoint::JournalPublished {
                Err(io::Error::other("simulated process termination").into())
            } else {
                Ok(())
            }
        },
    );
    if result.is_ok() {
        return Err(io::Error::other("journal checkpoint did not stop publication").into());
    }
    std::fs::remove_dir_all(directory.path().join(&source.id))?;

    let rows = super::super::read_index(directory.path())?;
    let recovered = rows
        .iter()
        .find(|row| row.id == destination.id)
        .ok_or_else(|| io::Error::other("recovered destination row is missing"))?;
    let store =
        ResponseAudioStore::for_session(directory.path(), recovered, DurabilityPolicy::Flush, None);
    assert_eq!(store.read(reference)?.audio, b"audio");
    Ok(())
}

fn has_audio_publication_stage(data_dir: &Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(data_dir)?
        .filter_map(Result::ok)
        .any(|entry| names::audio_stage_id(&entry.file_name()).is_some()))
}
