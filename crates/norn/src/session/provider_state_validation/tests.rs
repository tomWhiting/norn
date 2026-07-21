use super::*;
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::{
    ProviderFilteredForkBoundary, ResponseAudioArtifactLink, ResponseAudioArtifactRef,
    response_publication_fixture,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn assistant(base: EventBase, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base,
        content: "answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

fn assert_invalid_frame_remains_visible(events: &[SessionEvent]) {
    assert!(response_publication_group_len(events, 0).is_err());
    assert!(validate_provider_state_provenance(events).is_err());

    let provenance_id = events[1].base().id.clone();
    let mut visible = Vec::new();
    crate::r#loop::context::for_each_visible_event(events, &ContextEdits::new(), |event, _tag| {
        visible.push(event.base().id.clone());
    });
    assert!(
        visible.contains(&provenance_id),
        "an invalid local frame must not hide its custom provenance row",
    );
}

#[test]
fn filtered_fork_closes_legacy_eligibility_across_a_later_cut() -> TestResult {
    let filtered = ProviderFilteredForkBoundary::into_event(EventBase::new(None));
    let later_cut = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(Some(filtered.base().id.clone())),
        reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
    };
    let assistant_base = EventBase::new(Some(later_cut.base().id.clone()));
    let assistant_id = assistant_base.id.clone();
    let events = vec![
        filtered,
        later_cut,
        assistant(assistant_base, "resp_unframed"),
    ];

    let provenance = discover_active_response_provenance(&events)?;
    assert_eq!(
        provenance.disposition(&assistant_id),
        Some(ResponseStateDisposition::UnmarkedAfterProvenance),
        "a later cut must not reopen legacy eligibility closed by a filtered fork",
    );
    Ok(())
}

#[test]
fn direct_publication_rejects_a_positional_assistant_with_the_wrong_id() -> TestResult {
    let fixture = response_publication_fixture(None, true)?;
    let mut mismatched_base = fixture.assistant_base;
    mismatched_base.id = EventId::new();
    let events = vec![
        fixture.boundary,
        fixture.provenance,
        assistant(mismatched_base, "resp_direct"),
    ];

    assert_invalid_frame_remains_visible(&events);
    Ok(())
}

#[test]
fn audio_publication_rejects_a_positional_assistant_with_the_wrong_id() -> TestResult {
    let fixture = response_publication_fixture(None, true)?;
    let expected_assistant_id = fixture.assistant_base.id.clone();
    let provenance_id = fixture.provenance.base().id.clone();
    let link_base = EventBase::new(Some(provenance_id));
    let reference: ResponseAudioArtifactRef =
        serde_json::from_value(serde_json::json!("123e4567-e89b-42d3-a456-426614174000"))?;
    let link = ResponseAudioArtifactLink::new(
        expected_assistant_id,
        reference,
        Some("resp_audio".to_owned()),
    )
    .into_custom_event(link_base)?;
    let mut mismatched_base = EventBase::new(Some(link.base().id.clone()));
    mismatched_base.id = EventId::new();
    let events = vec![
        fixture.boundary,
        fixture.provenance,
        link,
        assistant(mismatched_base, "resp_audio"),
    ];

    assert_invalid_frame_remains_visible(&events);
    Ok(())
}
