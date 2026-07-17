use crate::provider::response_audio::ResponseAudioEvent;

use super::*;

#[test]
fn all_four_audio_events_are_typed_without_item_identity() -> TestResult {
    for (wire, expected) in [
        (
            event("response.audio.delta", 1, json!({"delta": "YXVkaW8="})),
            ResponseAudioEvent::AudioDelta {
                sequence_number: 1,
                bytes: b"audio".to_vec(),
            },
        ),
        (
            event("response.audio.done", 2, json!({})),
            ResponseAudioEvent::AudioDone { sequence_number: 2 },
        ),
        (
            event(
                "response.audio.transcript.delta",
                3,
                json!({"delta": "partial transcript"}),
            ),
            ResponseAudioEvent::TranscriptDelta {
                sequence_number: 3,
                delta: "partial transcript".to_owned(),
            },
        ),
        (
            event("response.audio.transcript.done", 4, json!({})),
            ResponseAudioEvent::TranscriptDone { sequence_number: 4 },
        ),
    ] {
        let mut reconciler = ResponseReconciler::new();
        assert_eq!(
            reconciler.ingest(&wire)?,
            ReconcileUpdate::ResponseAudio { event: expected }
        );
    }
    Ok(())
}

#[test]
fn duplicate_sequence_is_idempotent_but_new_sequence_repeated_done_fails() -> TestResult {
    let audio_done = event("response.audio.done", 1, json!({}));
    let mut audio = ResponseReconciler::new();
    assert!(matches!(
        audio.ingest(&audio_done)?,
        ReconcileUpdate::ResponseAudio { .. }
    ));
    assert_eq!(
        audio.ingest(&audio_done)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 1 }
    );
    assert_eq!(
        audio.ingest(&event("response.audio.done", 2, json!({}))),
        Err(ResponseReconciliationError::RepeatedAudioDone)
    );

    let transcript_done = event("response.audio.transcript.done", 1, json!({}));
    let mut transcript = ResponseReconciler::new();
    assert!(matches!(
        transcript.ingest(&transcript_done)?,
        ReconcileUpdate::ResponseAudio { .. }
    ));
    assert_eq!(
        transcript.ingest(&transcript_done)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 1 }
    );
    assert_eq!(
        transcript.ingest(&event("response.audio.transcript.done", 2, json!({}))),
        Err(ResponseReconciliationError::RepeatedAudioTranscriptDone)
    );
    Ok(())
}

#[test]
fn audio_delta_exact_duplicate_is_idempotent_but_changed_payload_conflicts() -> TestResult {
    let first = event("response.audio.delta", 1, json!({"delta": "YXVkaW8="}));
    let mut duplicate = ResponseReconciler::new();
    assert!(matches!(
        duplicate.ingest(&first)?,
        ReconcileUpdate::ResponseAudio { .. }
    ));
    assert_eq!(
        duplicate.ingest(&first)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 1 }
    );
    assert_eq!(
        duplicate.ingest(&event(
            "response.audio.delta",
            1,
            json!({"delta": "Y2hhbmdlZA=="}),
        )),
        Err(ResponseReconciliationError::ConflictingDuplicateSequence { sequence_number: 1 })
    );
    Ok(())
}

#[test]
fn audio_and_transcript_done_state_are_independent() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&event("response.audio.done", 1, json!({})))?;
    assert!(matches!(
        reconciler.ingest(&event(
            "response.audio.transcript.delta",
            2,
            json!({"delta": "still accepted"}),
        ))?,
        ReconcileUpdate::ResponseAudio { .. }
    ));
    assert!(matches!(
        reconciler.ingest(&event("response.audio.transcript.done", 3, json!({})))?,
        ReconcileUpdate::ResponseAudio { .. }
    ));
    Ok(())
}

#[test]
fn deltas_after_their_own_done_marker_fail_typed() -> TestResult {
    let mut audio = ResponseReconciler::new();
    audio.ingest(&event("response.audio.done", 1, json!({})))?;
    assert_eq!(
        audio.ingest(&event("response.audio.delta", 2, json!({"delta": "YQ=="}),)),
        Err(ResponseReconciliationError::AudioDeltaAfterDone)
    );

    let mut transcript = ResponseReconciler::new();
    transcript.ingest(&event("response.audio.transcript.done", 1, json!({})))?;
    assert_eq!(
        transcript.ingest(&event(
            "response.audio.transcript.delta",
            2,
            json!({"delta": "late"}),
        )),
        Err(ResponseReconciliationError::AudioTranscriptDeltaAfterDone)
    );
    Ok(())
}

#[test]
fn malformed_audio_and_transcript_delta_payloads_fail_typed() {
    for event_type in ["response.audio.delta", "response.audio.transcript.delta"] {
        let mut missing = ResponseReconciler::new();
        assert_eq!(
            missing.ingest(&event(event_type, 1, json!({}))),
            Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type,
                field: "delta",
            })
        );

        let mut non_string = ResponseReconciler::new();
        assert_eq!(
            non_string.ingest(&event(event_type, 1, json!({"delta": 42}))),
            Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type,
                field: "delta",
            })
        );
    }

    let mut invalid = ResponseReconciler::new();
    assert_eq!(
        invalid.ingest(&event("response.audio.delta", 1, json!({"delta": "***"}),)),
        Err(ResponseReconciliationError::InvalidAudioDeltaBase64)
    );
}

#[test]
fn terminal_does_not_invent_required_audio_channel_completion() -> TestResult {
    for prior in [
        Vec::new(),
        vec![event("response.audio.delta", 1, json!({"delta": "YQ=="}))],
        vec![event(
            "response.audio.transcript.delta",
            1,
            json!({"delta": "partial"}),
        )],
        vec![
            event("response.audio.delta", 1, json!({"delta": "YQ=="})),
            event(
                "response.audio.transcript.delta",
                2,
                json!({"delta": "partial"}),
            ),
        ],
    ] {
        let mut reconciler = ResponseReconciler::new();
        let terminal_sequence = u64::try_from(prior.len())? + 1;
        for frame in prior {
            reconciler.ingest(&frame)?;
        }
        assert!(matches!(
            reconciler.ingest(&event(
                "response.completed",
                terminal_sequence,
                json!({"response": {"output": []}}),
            ))?,
            ReconcileUpdate::Terminal { .. }
        ));
    }
    Ok(())
}
