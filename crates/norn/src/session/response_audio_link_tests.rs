use serde_json::json;

use super::{
    ResponseAudioArtifactLink, ResponseAudioArtifactRef, ResponseAudioReferenceError,
    referenced_response_audio_artifacts, response_audio_artifact_links,
};
use crate::session::events::{EventBase, EventUsage, SessionEvent};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn reference() -> Result<ResponseAudioArtifactRef, serde_json::Error> {
    serde_json::from_value(json!("123e4567-e89b-42d3-a456-426614174000"))
}

fn linked_turn(
    response_id: Option<String>,
) -> Result<(SessionEvent, SessionEvent, ResponseAudioArtifactRef), serde_json::Error> {
    let reference = reference()?;
    let link_base = EventBase::new(None);
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let link =
        ResponseAudioArtifactLink::new(assistant_base.id.clone(), reference, response_id.clone())
            .into_custom_event(link_base)?;
    let assistant = SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: "answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id,
    };
    Ok((link, assistant, reference))
}

#[test]
fn assistant_message_serialization_keeps_exact_format_two_shape() -> TestResult {
    let base = EventBase::new(None);
    let event = SessionEvent::AssistantMessage {
        base: base.clone(),
        response_items: Vec::new(),
        content: "answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    };
    assert_eq!(
        serde_json::to_value(event)?,
        json!({
            "type": "AssistantMessage",
            "base": base,
            "content": "answer",
            "thinking": "",
            "tool_calls": [],
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0
            },
            "stop_reason": "end_turn"
        })
    );
    Ok(())
}

#[test]
fn typed_artifact_link_round_trips_and_enumerates() -> TestResult {
    let (link, assistant, reference) = linked_turn(Some("resp_audio".to_owned()))?;
    let encoded = serde_json::to_vec(&link)?;
    let decoded: SessionEvent = serde_json::from_slice(&encoded)?;
    let parsed = ResponseAudioArtifactLink::from_event(&decoded)?
        .ok_or_else(|| std::io::Error::other("typed link was not recognized"))?;
    assert_eq!(parsed.reference(), reference);
    assert_eq!(parsed.response_id(), Some("resp_audio"));
    assert_eq!(
        response_audio_artifact_links(&[decoded.clone(), assistant.clone()])?,
        vec![parsed]
    );
    assert_eq!(
        referenced_response_audio_artifacts(&[decoded, assistant])?,
        vec![reference]
    );
    Ok(())
}

#[test]
fn unknown_link_field_is_rejected() -> TestResult {
    let (link, _assistant, _reference) = linked_turn(None)?;
    let SessionEvent::Custom { mut data, .. } = link else {
        return Err(std::io::Error::other("link changed event variant").into());
    };
    let object = data
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("link data changed shape"))?;
    object.insert("future".to_owned(), json!(true));
    let malformed = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: super::RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE.to_owned(),
        data,
    };
    assert!(matches!(
        ResponseAudioArtifactLink::from_event(&malformed),
        Err(ResponseAudioReferenceError::InvalidArtifactLink { .. })
    ));
    Ok(())
}

#[test]
fn orphan_precursor_remains_enumerable() -> TestResult {
    let (link, _assistant, reference) = linked_turn(Some("resp_audio".to_owned()))?;
    assert_eq!(
        referenced_response_audio_artifacts(&[link])?,
        vec![reference]
    );
    Ok(())
}

#[test]
fn assistant_response_id_mismatch_is_rejected() -> TestResult {
    let (link, mut assistant, _reference) = linked_turn(Some("resp_link".to_owned()))?;
    if let SessionEvent::AssistantMessage { response_id, .. } = &mut assistant {
        *response_id = Some("resp_assistant".to_owned());
    }
    assert!(matches!(
        response_audio_artifact_links(&[link, assistant]),
        Err(ResponseAudioReferenceError::ResponseIdMismatch { .. })
    ));
    Ok(())
}

#[test]
fn one_artifact_cannot_be_linked_to_two_assistant_events() -> TestResult {
    let (first_link, first_assistant, reference) = linked_turn(Some("resp_audio".to_owned()))?;
    let (second_link, second_assistant, second_reference) =
        linked_turn(Some("resp_audio".to_owned()))?;
    assert_eq!(second_reference, reference);
    assert!(matches!(
        response_audio_artifact_links(&[
            first_link,
            first_assistant,
            second_link,
            second_assistant,
        ]),
        Err(ResponseAudioReferenceError::DuplicateArtifactLink { .. })
    ));
    Ok(())
}

#[test]
fn noncanonical_event_id_and_unsupported_version_are_rejected() -> TestResult {
    let reference = reference()?;
    let event_id = "018f47a1-b2c3-7d4e-8f90-123456789abc";
    for data in [
        json!({
            "version": 1,
            "assistant_event_id": event_id.to_uppercase(),
            "reference": reference,
        }),
        json!({
            "version": 2,
            "assistant_event_id": event_id,
            "reference": reference,
        }),
    ] {
        let event = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: super::RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE.to_owned(),
            data,
        };
        assert!(matches!(
            ResponseAudioArtifactLink::from_event(&event),
            Err(ResponseAudioReferenceError::InvalidArtifactLink { .. })
        ));
    }
    Ok(())
}
