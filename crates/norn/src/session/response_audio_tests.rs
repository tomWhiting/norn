use std::fs::OpenOptions;
use std::io::Write as _;

use serde_json::json;
use tempfile::tempdir;

use super::{ResponseAudioArtifactState, ResponseAudioStore};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::store::DurabilityPolicy;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn options(model: &str) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn frame(
    event_type: &str,
    sequence_number: u64,
    fields: &serde_json::Value,
) -> Result<(ResponseStreamEvent, ResponseAudioEvent), Box<dyn std::error::Error>> {
    let mut raw = json!({
        "type": event_type,
        "sequence_number": sequence_number,
    });
    let object = raw
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("test event was not an object"))?;
    let fields = fields
        .as_object()
        .ok_or_else(|| std::io::Error::other("test fields were not an object"))?;
    object.extend(fields.clone());
    let envelope = ResponseStreamEvent::from_raw(raw)?;
    let event = ResponseAudioEvent::from_stream_event(&envelope)?
        .ok_or_else(|| std::io::Error::other("test frame was not audio"))?;
    Ok((envelope, event))
}

fn artifact_path(
    store: &ResponseAudioStore,
    reference: super::ResponseAudioArtifactRef,
) -> std::path::PathBuf {
    store.data_dir.join(store.artifact_path(reference))
}

#[test]
fn sidecar_decodes_each_padded_delta_and_tracks_independent_completion() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    for (event_type, sequence, fields) in [
        ("response.audio.delta", 1, json!({"delta": "YQ=="})),
        (
            "response.audio.transcript.delta",
            3,
            json!({"delta": "hello "}),
        ),
        ("response.audio.delta", 5, json!({"delta": "Yg=="})),
        ("response.audio.transcript.done", 8, json!({})),
        ("response.audio.done", 13, json!({})),
    ] {
        let (raw, event) = frame(event_type, sequence, &fields)?;
        writer.append(&raw, &event)?;
    }
    let reference = writer.seal(Some("resp_audio"))?;
    let decoded = store.read(reference)?;

    assert_eq!(decoded.audio, b"ab");
    assert_eq!(decoded.transcript, "hello ");
    assert!(decoded.audio_complete);
    assert!(decoded.transcript_complete);
    assert_eq!(decoded.response_id.as_deref(), Some("resp_audio"));
    assert_eq!(decoded.state, ResponseAudioArtifactState::Sealed);
    assert_eq!(store.list()?, vec![reference]);
    Ok(())
}

#[test]
fn missing_channel_done_is_preserved_without_fabricating_completion() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame("response.audio.delta", 1, &json!({"delta": "YQ=="}))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(None)?;

    let decoded = store.read(reference)?;
    assert_eq!(decoded.audio, b"a");
    assert!(!decoded.audio_complete);
    assert!(!decoded.transcript_complete);
    assert_eq!(decoded.state, ResponseAudioArtifactState::Sealed);
    Ok(())
}

#[test]
fn dropped_writer_remains_an_explicit_unsealed_artifact() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame(
        "response.audio.transcript.delta",
        1,
        &json!({"delta": "partial"}),
    )?;
    writer.append(&raw, &event)?;
    drop(writer);

    let references = store.list()?;
    assert_eq!(references.len(), 1);
    let reference = references
        .first()
        .copied()
        .ok_or_else(|| std::io::Error::other("unsealed reference missing"))?;
    let decoded = store.read(reference)?;
    assert_eq!(decoded.transcript, "partial");
    assert_eq!(decoded.state, ResponseAudioArtifactState::Unsealed);
    assert!(decoded.response_id.is_none());
    Ok(())
}

#[test]
fn strict_reader_rejects_bytes_after_terminal_integrity_record() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let writer = store.begin(1)?;
    let reference = writer.seal(None)?;
    OpenOptions::new()
        .append(true)
        .open(artifact_path(store, reference))?
        .write_all(b"corrupt\n")?;

    let error = store
        .read(reference)
        .err()
        .ok_or_else(|| std::io::Error::other("corrupt sidecar was accepted"))?;
    assert!(matches!(
        error,
        crate::session::SessionPersistError::InvalidResponseAudioArtifact { .. }
    ));
    Ok(())
}

#[test]
fn writer_does_not_retain_the_global_index_lock_between_frames() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let writer = store.begin(1)?;

    let other = manager.create(options("other"), DurabilityPolicy::Flush)?;
    assert_ne!(other.entry.id, opened.entry.id);
    drop(writer);
    Ok(())
}

#[test]
fn stale_generation_cannot_create_or_seal_sidecars() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let first = manager.create_with_id("audio-aba", options("first"), DurabilityPolicy::Flush)?;
    let stale = ResponseAudioStore::for_session(
        directory.path(),
        &first.entry,
        DurabilityPolicy::Flush,
        None,
    );
    let stale_writer = stale.begin(1)?;
    let stale_reference = stale
        .list()?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("stale reference missing"))?;
    drop(first.store);
    manager.delete("audio-aba")?;
    let replacement =
        manager.create_with_id("audio-aba", options("replacement"), DurabilityPolicy::Flush)?;
    assert_ne!(first.entry.generation, replacement.entry.generation);

    let error = stale
        .begin(1)
        .err()
        .ok_or_else(|| std::io::Error::other("stale writer unexpectedly began"))?;
    assert!(matches!(
        error,
        crate::session::SessionPersistError::GenerationChanged { .. }
    ));
    let seal_error = stale_writer
        .seal(None)
        .err()
        .ok_or_else(|| std::io::Error::other("stale writer unexpectedly sealed"))?;
    assert!(matches!(
        seal_error,
        crate::session::SessionPersistError::GenerationChanged { .. }
    ));
    assert!(matches!(
        stale.list(),
        Err(crate::session::SessionPersistError::GenerationChanged { .. })
    ));
    assert!(matches!(
        stale.read(stale_reference),
        Err(crate::session::SessionPersistError::GenerationChanged { .. })
    ));
    Ok(())
}

#[test]
fn decoded_artifact_debug_redacts_audio_transcript_and_response_id() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame(
        "response.audio.transcript.delta",
        1,
        &json!({"delta": "sentinel transcript"}),
    )?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(Some("sentinel-response-id"))?;

    let rendered = format!("{:?}", store.read(reference)?);
    assert!(!rendered.contains("sentinel transcript"));
    assert!(!rendered.contains("sentinel-response-id"));
    assert!(rendered.contains("transcript_bytes"));
    assert!(rendered.contains("has_response_id: true"));
    Ok(())
}

#[test]
fn writer_rejects_projection_that_disagrees_with_retained_envelope() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, _) = frame("response.audio.delta", 1, &json!({"delta": "YQ=="}))?;
    let mismatched = ResponseAudioEvent::AudioDelta {
        sequence_number: 1,
        bytes: b"b".to_vec(),
    };

    let error = writer
        .append(&raw, &mismatched)
        .err()
        .ok_or_else(|| std::io::Error::other("mismatched projection was accepted"))?;
    assert!(matches!(
        error,
        crate::session::SessionPersistError::InvalidResponseAudioArtifact { .. }
    ));
    Ok(())
}

#[test]
fn torn_final_record_is_reported_as_unsealed_not_corrupt() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame("response.audio.delta", 1, &json!({"delta": "YQ=="}))?;
    writer.append(&raw, &event)?;
    drop(writer);
    let reference = store
        .list()?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("unsealed reference missing"))?;
    OpenOptions::new()
        .append(true)
        .open(artifact_path(store, reference))?
        .write_all(b"{\"record\":\"frame\"")?;

    let decoded = store.read(reference)?;
    assert_eq!(decoded.audio, b"a");
    assert_eq!(decoded.state, ResponseAudioArtifactState::Unsealed);
    Ok(())
}

#[test]
fn terminal_integrity_commits_unknown_retained_envelope_fields() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame("response.audio.delta", 1, &json!({"delta": "YQ=="}))?;
    writer.append(&raw, &event)?;
    let (raw, event) = frame("response.audio.done", 2, &json!({}))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(None)?;
    let path = artifact_path(store, reference);
    let original = std::fs::read_to_string(&path)?;
    let tampered = original.replace(
        "\"delta\":\"YQ==\"",
        "\"delta\":\"YQ==\",\"future\":\"tampered\"",
    );
    if tampered == original {
        return Err(std::io::Error::other("test fixture did not contain audio delta").into());
    }
    std::fs::write(path, tampered)?;

    assert!(matches!(
        store.read(reference),
        Err(crate::session::SessionPersistError::InvalidResponseAudioArtifact { .. })
    ));
    Ok(())
}

#[test]
fn artifact_reference_requires_canonical_uuid_v4_text() -> TestResult {
    let canonical = "550e8400-e29b-41d4-a716-446655440000";
    let reference = serde_json::from_value::<super::ResponseAudioArtifactRef>(json!(canonical))?;
    assert_eq!(reference.to_string(), canonical);

    for invalid in [
        "550e8400e29b41d4a716446655440000",
        "550E8400-E29B-41D4-A716-446655440000",
        "550e8400-e29b-11d4-a716-446655440000",
    ] {
        assert!(
            serde_json::from_value::<super::ResponseAudioArtifactRef>(json!(invalid)).is_err(),
            "non-canonical or non-v4 reference was accepted: {invalid}",
        );
    }
    Ok(())
}

#[test]
fn terminal_integrity_commits_terminal_and_header_provenance() -> TestResult {
    let directory = tempdir()?;
    let manager = SessionManager::new(directory.path());
    let opened = manager.create_with_id("audio-root", options("model"), DurabilityPolicy::Flush)?;
    let store = opened
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("audio store was not attached"))?;
    let mut writer = store.begin(1)?;
    let (raw, event) = frame("response.audio.done", 1, &json!({}))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(Some("resp_original"))?;
    let path = artifact_path(store, reference);
    let original = std::fs::read_to_string(&path)?;

    for (before, after) in [
        (
            "\"response_id\":\"resp_original\"",
            "\"response_id\":\"resp_tampered\"",
        ),
        ("\"audio_complete\":true", "\"audio_complete\":false"),
        (
            "\"owner_session_id\":\"audio-root\"",
            "\"owner_session_id\":\"other-root\"",
        ),
    ] {
        let tampered = original.replace(before, after);
        if tampered == original {
            return Err(std::io::Error::other("tamper fixture did not match").into());
        }
        std::fs::write(&path, tampered)?;
        assert!(matches!(
            store.read(reference),
            Err(crate::session::SessionPersistError::InvalidResponseAudioArtifact { .. })
        ));
    }
    Ok(())
}
