use super::*;

fn assert_copy<T: Copy>() {}

#[test]
fn frame_signatures_are_fixed_width_copy_values() {
    assert_copy::<FrameSignature>();
    assert_eq!(
        std::mem::size_of::<FrameSignature>(),
        std::mem::size_of::<[u8; 32]>()
    );
    assert!(!std::mem::needs_drop::<FrameSignature>());
}

#[test]
fn frame_signatures_preserve_value_equality_and_cover_all_content() -> TestResult {
    let ordered: Value = serde_json::from_str(
        r#"{"sequence_number":7,"response":{"id":"r","status":"in_progress"}}"#,
    )?;
    let reordered: Value = serde_json::from_str(
        r#"{"response":{"status":"in_progress","id":"r"},"sequence_number":7}"#,
    )?;
    assert_eq!(ordered, reordered);

    let signature = FrameSignature::new("response.created", &ordered);
    assert_eq!(
        signature,
        FrameSignature::new("response.created", &reordered)
    );
    assert_ne!(
        signature,
        FrameSignature::new("response.in_progress", &reordered)
    );

    let changed: Value = serde_json::from_str(
        r#"{"response":{"status":"completed","id":"r"},"sequence_number":7}"#,
    )?;
    assert_ne!(signature, FrameSignature::new("response.created", &changed));

    for (left, right) in [
        (json!(4_607_182_418_800_017_408_u64), json!(1.0_f64)),
        (json!(u64::MAX), json!(-1_i64)),
    ] {
        assert_ne!(left, right);
        assert_ne!(
            FrameSignature::new("numeric", &left),
            FrameSignature::new("numeric", &right)
        );
    }

    for (left, right) in [
        (json!(0.0_f64), json!(-0.0_f64)),
        (
            serde_json::from_str::<Value>("1.0")?,
            serde_json::from_str::<Value>("1.00")?,
        ),
    ] {
        assert_eq!(
            left == right,
            FrameSignature::new("numeric", &left) == FrameSignature::new("numeric", &right)
        );
    }
    Ok(())
}

#[test]
fn reordered_object_keys_remain_an_identical_duplicate() -> TestResult {
    let first = SseEvent {
        event_type: "response.created".to_owned(),
        data: serde_json::from_str(
            r#"{"sequence_number":7,"response":{"id":"r","status":"in_progress"}}"#,
        )?,
    };
    let reordered = SseEvent {
        event_type: "response.created".to_owned(),
        data: serde_json::from_str(
            r#"{"response":{"status":"in_progress","id":"r"},"sequence_number":7}"#,
        )?,
    };
    let mut reconciler = ResponseReconciler::new();
    assert_eq!(reconciler.ingest(&first)?, ReconcileUpdate::Ignored);
    assert_eq!(
        reconciler.ingest(&reordered)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 7 }
    );
    Ok(())
}

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
