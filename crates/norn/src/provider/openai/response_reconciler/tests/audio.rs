use super::*;

#[test]
fn every_public_audio_event_fails_closed_as_known_unsupported_media() {
    for (event_type, payload) in [
        ("response.audio.delta", json!({"delta": "YXVkaW8="})),
        ("response.audio.done", json!({})),
        (
            "response.audio.transcript.delta",
            json!({"delta": "partial transcript"}),
        ),
        ("response.audio.transcript.done", json!({})),
    ] {
        let mut reconciler = ResponseReconciler::new();
        assert_eq!(
            reconciler.ingest(&event(event_type, 1, payload)),
            Err(ResponseReconciliationError::UnsupportedResponseMedia)
        );
    }
}
