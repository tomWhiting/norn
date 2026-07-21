use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::error::SessionError;
use crate::session::events::{
    EventBase, EventId, EventUsage, ProviderEpochBoundaryReason, SessionEvent, ToolCallEvent,
};
use crate::session::persistence::io::read_session_events;
use crate::session::{
    DurabilityPolicy, EventStore, JsonlSink, ProviderStateProvenance, ProviderStateValidationError,
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponsePublicationCommitment,
    SessionPersistError, seal_response_publication_group,
    validate_new_response_publication_batches, validate_provider_state_provenance,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestResultWith<T> = Result<T, Box<dyn std::error::Error>>;

fn fixed_id(value: &str) -> Result<EventId, serde_json::Error> {
    serde_json::from_value(json!(value))
}

fn fixed_base(
    id: &str,
    parent_id: Option<&str>,
    timestamp: &str,
) -> Result<EventBase, serde_json::Error> {
    serde_json::from_value(json!({
        "id": id,
        "parent_id": parent_id,
        "timestamp": timestamp,
    }))
}

fn assistant(base: EventBase, content: &str, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base,
        response_items: Vec::new(),
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

fn legacy_direct_group() -> Result<Vec<SessionEvent>, serde_json::Error> {
    const BOUNDARY: &str = "11111111-1111-4111-8111-111111111111";
    const PROVENANCE: &str = "22222222-2222-4222-8222-222222222222";
    const ASSISTANT: &str = "33333333-3333-4333-8333-333333333333";

    let boundary = SessionEvent::ProviderEpochBoundary {
        base: fixed_base(BOUNDARY, None, "2026-01-02T03:04:05Z")?,
        reason: ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance = ProviderStateProvenance::new(fixed_id(ASSISTANT)?, true).into_custom_event(
        fixed_base(PROVENANCE, Some(BOUNDARY), "2026-01-02T03:04:06Z")?,
    )?;
    let assistant = assistant(
        fixed_base(ASSISTANT, Some(PROVENANCE), "2026-01-02T03:04:07Z")?,
        "fixed answer",
        "resp_fixed",
    );
    Ok(vec![boundary, provenance, assistant])
}

fn sealed_direct_group() -> TestResultWith<Vec<SessionEvent>> {
    let mut group = legacy_direct_group()?;
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn sealed_audio_group() -> TestResultWith<Vec<SessionEvent>> {
    const BOUNDARY: &str = "44444444-4444-4444-8444-444444444444";
    const PROVENANCE: &str = "55555555-5555-4555-8555-555555555555";
    const LINK: &str = "66666666-6666-4666-8666-666666666666";
    const ASSISTANT: &str = "77777777-7777-4777-8777-777777777777";

    let boundary = SessionEvent::ProviderEpochBoundary {
        base: fixed_base(BOUNDARY, None, "2026-02-03T04:05:06Z")?,
        reason: ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let provenance = ProviderStateProvenance::new(fixed_id(ASSISTANT)?, true).into_custom_event(
        fixed_base(PROVENANCE, Some(BOUNDARY), "2026-02-03T04:05:07Z")?,
    )?;
    let reference: ResponseAudioArtifactRef =
        serde_json::from_value(json!("123e4567-e89b-42d3-a456-426614174000"))?;
    let link = ResponseAudioArtifactLink::new(
        fixed_id(ASSISTANT)?,
        reference,
        Some("resp_audio_fixed".to_owned()),
    )
    .into_custom_event(fixed_base(LINK, Some(PROVENANCE), "2026-02-03T04:05:08Z")?)?;
    let assistant = assistant(
        fixed_base(ASSISTANT, Some(LINK), "2026-02-03T04:05:09Z")?,
        "fixed audio answer",
        "resp_audio_fixed",
    );
    let mut group = vec![boundary, provenance, link, assistant];
    seal_response_publication_group(&mut group)?;
    Ok(group)
}

fn commitment(group: &[SessionEvent]) -> Result<&ResponsePublicationCommitment, std::io::Error> {
    match group.first() {
        Some(SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ResponseStatePublicationV1(commitment),
            ..
        }) => Ok(commitment),
        _ => Err(std::io::Error::other(
            "response group has no V1 publication commitment",
        )),
    }
}

fn mutate_assistant_content(group: &mut [SessionEvent], content: &str) -> TestResult {
    let Some(SessionEvent::AssistantMessage {
        content: current, ..
    }) = group.last_mut()
    else {
        return Err(std::io::Error::other("response group has no assistant tail").into());
    };
    *current = content.to_owned();
    Ok(())
}

fn mutate_audio_reference(group: &mut [SessionEvent]) -> TestResult {
    let Some(SessionEvent::Custom { data, .. }) = group.get_mut(2) else {
        return Err(std::io::Error::other("audio group has no link row").into());
    };
    data["reference"] = json!("223e4567-e89b-42d3-a456-426614174001");
    Ok(())
}

fn timeline_bytes(events: &[SessionEvent]) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"{\"norn_session_format\":2}\n".to_vec();
    for event in events {
        serde_json::to_writer(&mut bytes, event)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn commitment_payload(value: &mut Value) -> Result<&mut Map<String, Value>, std::io::Error> {
    value
        .as_array_mut()
        .and_then(|events| events.first_mut())
        .and_then(Value::as_object_mut)
        .and_then(|boundary| boundary.get_mut("reason"))
        .and_then(Value::as_object_mut)
        .and_then(|reason| reason.get_mut("response_state_publication_v1"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other("encoded group omitted its V1 commitment payload"))
}

fn write_fsynced_prefix(path: &Path, events: &[SessionEvent], rows: usize) -> TestResult {
    let sink = JsonlSink::open_with(path, DurabilityPolicy::FsyncPerEvent)?;
    drop(sink);
    let prefix = events
        .get(..rows)
        .ok_or_else(|| std::io::Error::other("requested prefix exceeds response group"))?;
    let mut file = OpenOptions::new().append(true).open(path)?;
    for event in prefix {
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn assert_commitment_error(result: Result<(), ProviderStateValidationError>) {
    assert_eq!(
        result,
        Err(ProviderStateValidationError::PublicationCommitment)
    );
}

#[test]
fn fixed_direct_group_has_a_stable_canonical_commitment() -> TestResult {
    let group = sealed_direct_group()?;
    let observed = commitment(&group)?;
    assert_eq!(observed.event_count(), 3);
    assert_eq!(
        observed.group_sha256(),
        "b2aea16533082f8bb92f164850c51b5eeff6e859e83d5df4b4a05f20fffa4d54",
        "the domain-separated canonical preimage is a durable format contract",
    );
    assert!(
        observed
            .group_sha256()
            .bytes()
            .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) })
    );
    assert_eq!(observed.group_sha256().len(), 64);
    validate_provider_state_provenance(&group)?;

    let encoded = serde_json::to_value(&group)?;
    let decoded: Vec<SessionEvent> = serde_json::from_value(encoded.clone())?;
    assert_eq!(serde_json::to_value(decoded)?, encoded);
    Ok(())
}

#[test]
fn canonical_commitment_sorts_nested_objects_and_distinguishes_signed_zero() -> TestResult {
    let mut left = legacy_direct_group()?;
    let mut right = left.clone();
    let mut positive = left.clone();

    let mut left_arguments = serde_json::Map::new();
    left_arguments.insert("z".to_owned(), json!(-0.0));
    left_arguments.insert("a".to_owned(), json!({"second": 2, "first": 1}));
    let mut right_arguments = serde_json::Map::new();
    right_arguments.insert("a".to_owned(), json!({"first": 1, "second": 2}));
    right_arguments.insert("z".to_owned(), json!(-0.0));
    let mut positive_arguments = right_arguments.clone();
    positive_arguments.insert("z".to_owned(), json!(0.0));

    for (group, arguments) in [
        (&mut left, left_arguments),
        (&mut right, right_arguments),
        (&mut positive, positive_arguments),
    ] {
        let Some(SessionEvent::AssistantMessage { tool_calls, .. }) = group.last_mut() else {
            return Err(std::io::Error::other("response group has no assistant tail").into());
        };
        tool_calls.push(ToolCallEvent {
            call_id: "call_canonical".to_owned(),
            name: "canonical".to_owned(),
            arguments: serde_json::Value::Object(arguments),
            kind: crate::provider::request::ToolCallKind::Function,
            caller: crate::provider::request::ToolCallCaller::Absent,
        });
        seal_response_publication_group(group)?;
    }

    assert_eq!(
        commitment(&left)?.group_sha256(),
        commitment(&right)?.group_sha256(),
    );
    assert_ne!(
        commitment(&left)?.group_sha256(),
        commitment(&positive)?.group_sha256(),
    );

    let Some(SessionEvent::AssistantMessage { tool_calls, .. }) = left.last_mut() else {
        return Err(std::io::Error::other("response group has no assistant tail").into());
    };
    let Some(arguments) = tool_calls
        .first_mut()
        .and_then(|tool_call| tool_call.arguments.as_object_mut())
    else {
        return Err(std::io::Error::other("response tool call has no object arguments").into());
    };
    arguments.insert("z".to_owned(), json!(0.0));
    assert_commitment_error(validate_new_response_publication_batches(&left));
    assert_commitment_error(validate_provider_state_provenance(&left));
    Ok(())
}

#[test]
fn structurally_decodable_invalid_commitments_fail_validation() -> TestResult {
    let group = sealed_direct_group()?;
    for (field, replacement) in [
        ("event_count", json!(4)),
        ("group_sha256", json!("NOT-A-LOWERCASE-SHA256")),
    ] {
        let mut encoded = serde_json::to_value(&group)?;
        commitment_payload(&mut encoded)?.insert(field.to_owned(), replacement);
        let decoded: Vec<SessionEvent> = serde_json::from_value(encoded)?;

        assert_commitment_error(validate_new_response_publication_batches(&decoded));
        assert_commitment_error(validate_provider_state_provenance(&decoded));
    }
    Ok(())
}

#[test]
fn unknown_response_publication_v2_reason_fails_closed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let group = sealed_direct_group()?;
    let mut encoded = serde_json::to_value(&group)?;
    let boundary = encoded
        .as_array_mut()
        .and_then(|events| events.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other("encoded group omitted its boundary"))?;
    let reasons = boundary
        .get_mut("reason")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other("encoded boundary reason is not an object"))?;
    let payload = reasons
        .remove("response_state_publication_v1")
        .ok_or_else(|| std::io::Error::other("encoded boundary omitted its V1 reason"))?;
    reasons.insert("response_state_publication_v2".to_owned(), payload);

    let boundary_value = encoded
        .as_array()
        .and_then(|events| events.first())
        .ok_or_else(|| std::io::Error::other("encoded group lost its boundary"))?;
    assert!(serde_json::from_value::<SessionEvent>(boundary_value.clone()).is_err());

    fs::write(
        temp.path().join("unknown-v2.jsonl"),
        serde_json::to_vec(&json!({"norn_session_format": 2}))?
            .into_iter()
            .chain([b'\n'])
            .chain(serde_json::to_vec(boundary_value)?)
            .chain([b'\n'])
            .collect::<Vec<_>>(),
    )?;
    let error = read_session_events(temp.path(), "unknown-v2")
        .err()
        .ok_or_else(|| std::io::Error::other("unknown V2 publication reason replayed"))?;
    assert!(matches!(error, SessionPersistError::InvalidTimeline(_)));
    Ok(())
}

#[test]
fn direct_publication_commitment_rejects_assistant_tamper() -> TestResult {
    let mut group = sealed_direct_group()?;
    mutate_assistant_content(&mut group, "different unseen answer")?;

    assert_commitment_error(validate_new_response_publication_batches(&group));
    assert_commitment_error(validate_provider_state_provenance(&group));
    Ok(())
}

#[test]
fn audio_publication_commitment_rejects_link_tamper() -> TestResult {
    let mut group = sealed_audio_group()?;
    assert_eq!(commitment(&group)?.event_count(), 4);
    mutate_audio_reference(&mut group)?;

    assert_commitment_error(validate_new_response_publication_batches(&group));
    assert_commitment_error(validate_provider_state_provenance(&group));
    Ok(())
}

#[test]
fn complete_committed_group_rejects_tampered_suffix_on_replay() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut group = sealed_direct_group()?;
    mutate_assistant_content(&mut group, "tampered after commitment")?;
    fs::write(temp.path().join("tampered.jsonl"), timeline_bytes(&group)?)?;
    let error = read_session_events(temp.path(), "tampered")
        .err()
        .ok_or_else(|| std::io::Error::other("tampered committed group replayed"))?;
    assert!(matches!(
        error,
        SessionPersistError::InvalidProviderStateProvenance(
            ProviderStateValidationError::PublicationCommitment
        )
    ));
    Ok(())
}

#[test]
fn complete_legacy_uncommitted_group_remains_readable() -> TestResult {
    let temp = tempfile::tempdir()?;
    let group = legacy_direct_group()?;
    fs::write(temp.path().join("legacy.jsonl"), timeline_bytes(&group)?)?;
    let replay = read_session_events(temp.path(), "legacy")?;
    assert_eq!(
        serde_json::to_value(replay.events)?,
        serde_json::to_value(&group)?
    );
    validate_provider_state_provenance(&group)?;
    assert_commitment_error(validate_new_response_publication_batches(&group));
    Ok(())
}

#[test]
fn durable_committed_prefix_completes_on_exact_retry() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("committed-exact.jsonl");
    let group = sealed_direct_group()?;
    write_fsynced_prefix(&path, &group, 1)?;

    let store = EventStore::with_sink(Box::new(JsonlSink::open_with(
        &path,
        DurabilityPolicy::FsyncPerEvent,
    )?));
    store.append_batch(&group)?;
    assert_eq!(store.len(), group.len());
    let replay = read_session_events(temp.path(), "committed-exact")?;
    assert_eq!(
        serde_json::to_value(replay.events)?,
        serde_json::to_value(group)?
    );
    Ok(())
}

#[test]
fn durable_committed_prefix_rejects_divergent_unwritten_suffix() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("committed-divergent.jsonl");
    let group = sealed_direct_group()?;
    write_fsynced_prefix(&path, &group, 1)?;
    let before = fs::read(&path)?;
    let mut divergent = group.clone();
    mutate_assistant_content(&mut divergent, "divergent unseen suffix")?;

    let store = EventStore::with_sink(Box::new(JsonlSink::open_with(
        &path,
        DurabilityPolicy::FsyncPerEvent,
    )?));
    let error = store
        .append_batch(&divergent)
        .err()
        .ok_or_else(|| std::io::Error::other("divergent suffix completed durable prefix"))?;
    assert!(matches!(error, SessionError::EventAppendFailed { .. }));
    assert_eq!(fs::read(&path)?, before);
    assert!(store.is_empty());

    store.append_batch(&group)?;
    let replay = read_session_events(temp.path(), "committed-divergent")?;
    assert_eq!(
        serde_json::to_value(replay.events)?,
        serde_json::to_value(group)?
    );
    Ok(())
}

#[test]
fn durable_boundary_rejects_a_recomputed_divergent_commitment() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("committed-resealed.jsonl");
    let group = sealed_direct_group()?;
    write_fsynced_prefix(&path, &group, 1)?;
    let before = fs::read(&path)?;
    let mut divergent = group.clone();
    mutate_assistant_content(&mut divergent, "resealed divergent suffix")?;
    seal_response_publication_group(&mut divergent)?;
    assert_ne!(
        commitment(&group)?.group_sha256(),
        commitment(&divergent)?.group_sha256()
    );
    validate_new_response_publication_batches(&divergent)?;

    let store = EventStore::with_sink(Box::new(JsonlSink::open_with(
        &path,
        DurabilityPolicy::FsyncPerEvent,
    )?));
    let error = store
        .append_batch(&divergent)
        .err()
        .ok_or_else(|| std::io::Error::other("resealed suffix replaced durable commitment"))?;
    assert!(matches!(error, SessionError::StorageError { .. }));
    assert_eq!(fs::read(&path)?, before);
    assert!(store.is_empty());

    store.append_batch(&group)?;
    Ok(())
}

#[test]
fn durable_legacy_prefix_cannot_be_completed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("legacy-prefix.jsonl");
    let group = legacy_direct_group()?;
    write_fsynced_prefix(&path, &group, 1)?;
    let before = fs::read(&path)?;

    let store = EventStore::with_sink(Box::new(JsonlSink::open_with(
        &path,
        DurabilityPolicy::FsyncPerEvent,
    )?));
    let error = store
        .append_batch(&group)
        .err()
        .ok_or_else(|| std::io::Error::other("legacy prefix accepted an uncommitted suffix"))?;
    assert!(matches!(error, SessionError::EventAppendFailed { .. }));
    assert_eq!(fs::read(path)?, before);
    assert!(store.is_empty());
    Ok(())
}
