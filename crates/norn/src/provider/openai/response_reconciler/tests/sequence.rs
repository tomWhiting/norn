use super::*;

#[test]
fn duplicate_delta_sequence_is_not_applied_twice() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_a")))?;
    let fragment = delta(
        "response.output_text.delta",
        2,
        "msg_a",
        0,
        json!({"content_index": 0, "delta": "once", "logprobs": []}),
    );

    assert_eq!(reconciler.ingest(&fragment)?, ReconcileUpdate::Accepted);
    assert_eq!(
        reconciler.ingest(&fragment)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 2 }
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_a", 0, ResponseDeltaChannel::OutputText(0)),
        Some("once")
    );
    Ok(())
}

#[test]
fn conflicting_duplicate_delta_poisoning_preserves_first_preview() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_a")))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        2,
        "msg_a",
        0,
        json!({"content_index": 0, "delta": "first", "logprobs": []}),
    ))?;
    let conflict = delta(
        "response.output_text.delta",
        2,
        "msg_a",
        0,
        json!({"content_index": 0, "delta": "second", "logprobs": []}),
    );

    assert_eq!(
        reconciler.ingest(&conflict),
        Err(ResponseReconciliationError::ConflictingDuplicateSequence { sequence_number: 2 })
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_a", 0, ResponseDeltaChannel::OutputText(0)),
        Some("first")
    );
    assert_eq!(
        reconciler.ingest(&event("response.in_progress", 3, json!({}))),
        Err(ResponseReconciliationError::AlreadyFailed)
    );
    Ok(())
}

#[test]
fn post_terminal_duplicate_is_idempotent_but_new_frame_is_rejected() -> TestResult {
    let terminal = event("response.completed", 1, json!({"response": {"output": []}}));
    let mut reconciler = ResponseReconciler::new();
    assert!(matches!(
        reconciler.ingest(&terminal)?,
        ReconcileUpdate::Terminal { .. }
    ));
    assert_eq!(
        reconciler.ingest(&terminal)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 1 }
    );
    assert_eq!(
        reconciler.ingest(&event("response.in_progress", 2, json!({}))),
        Err(ResponseReconciliationError::PostTerminalFrame)
    );
    Ok(())
}
