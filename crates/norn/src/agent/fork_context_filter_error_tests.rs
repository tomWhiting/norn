use crate::agent::{ContextFilter, ContextFilterError};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::{
    ProviderFilteredForkBoundary, RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE, ResponseAudioReferenceError,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn malformed_response_audio_custom_event(event_type: &str) -> SessionEvent {
    SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: event_type.to_owned(),
        data: serde_json::json!({
            "version": 2,
            "assistant_event_id": EventBase::new(None).id,
            "reference": "123e4567-e89b-42d3-a456-426614174000",
        }),
    }
}

fn filtered_payload(
    events: &[SessionEvent],
) -> Result<&[SessionEvent], Box<dyn std::error::Error>> {
    let Some((boundary, payload)) = events.split_last() else {
        return Err("a non-identity filter omitted its provider epoch boundary".into());
    };
    if !ProviderFilteredForkBoundary::is_family(boundary) {
        return Err("a non-identity filter ended without a filtered-fork boundary".into());
    }
    Ok(payload)
}

#[test]
fn malformed_reserved_audio_link_fails_typed_only_for_nonidentity_filter() -> TestResult {
    let malformed = malformed_response_audio_custom_event(RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE);
    let source = vec![malformed];
    let copied = ContextFilter::default().apply(&source)?;
    assert_eq!(
        serde_json::to_vec(&copied)?,
        serde_json::to_vec(&source)?,
        "identity filtering must preserve malformed reserved audit data exactly",
    );

    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(16),
        exclude_tool_calls: false,
    }
    .apply(&source);
    let Err(error) = filtered else {
        return Err("a malformed reserved response-audio row was silently filtered".into());
    };
    assert!(matches!(
        error,
        ContextFilterError::ResponseAudio(ResponseAudioReferenceError::InvalidArtifactLink { .. })
    ));
    Ok(())
}

#[test]
fn unrelated_custom_event_with_audio_shaped_data_remains_opaque() -> TestResult {
    let unrelated = malformed_response_audio_custom_event("application.audio.note");
    let unrelated_id = unrelated.base().id.clone();
    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(16),
        exclude_tool_calls: false,
    }
    .apply(&[unrelated])?;
    let payload = filtered_payload(&filtered)?;

    assert_eq!(payload.len(), 1);
    assert_eq!(payload[0].base().id, unrelated_id);
    assert!(matches!(
        &payload[0],
        SessionEvent::Custom { event_type, .. } if event_type == "application.audio.note"
    ));
    Ok(())
}
