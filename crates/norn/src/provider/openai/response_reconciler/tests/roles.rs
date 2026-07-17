use super::super::roles::{HostedFamily, HostedPhase, ResponseEventRole, response_event_role};
use crate::provider::openai::response_contract::PUBLIC_STREAM_EVENTS;

#[test]
fn every_pinned_public_event_has_an_explicit_reconciliation_role() {
    let classified = PUBLIC_STREAM_EVENTS
        .iter()
        .filter(|event| response_event_role(event.name()).is_some())
        .count();
    assert_eq!(classified, PUBLIC_STREAM_EVENTS.len());
}

#[test]
fn coarse_completed_stage_does_not_determine_reconciliation_semantics() {
    assert_eq!(
        response_event_role("response.output_text.done"),
        Some(ResponseEventRole::CoreStringDone)
    );
    assert_eq!(
        response_event_role("response.image_generation_call.completed"),
        Some(ResponseEventRole::HostedLifecycle(
            HostedFamily::ImageGeneration,
            HostedPhase::Completed,
        ))
    );
    assert_eq!(
        response_event_role("response.audio.done"),
        Some(ResponseEventRole::ResponseAudio)
    );
}

#[test]
fn unknown_event_has_no_accidental_role() {
    assert_eq!(response_event_role("response.future"), None);
}
