use super::*;

fn output_part(text: &str, annotation: Option<Value>) -> Value {
    json!({
        "type": "output_text",
        "text": text,
        "annotations": annotation.into_iter().collect::<Vec<_>>(),
        "logprobs": []
    })
}

fn message_with_part(id: &str, part: &Value) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [part]
    })
}

fn part_event(event_type: &str, sequence: u64, item_id: &str, part: &Value) -> SseEvent {
    delta(
        event_type,
        sequence,
        item_id,
        0,
        json!({"content_index": 0, "part": part}),
    )
}

#[test]
fn content_part_completion_repairs_preview_and_binds_item_authority() -> TestResult {
    let annotation = json!({
        "type": "text_annotation",
        "text": "official open annotation example",
        "start": 0,
        "end": 10
    });
    let initial = output_part("", None);
    let complete = output_part("answer", Some(annotation.clone()));
    let item = message_with_part("msg_a", &complete);
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_a")))?;
    reconciler.ingest(&part_event(
        "response.content_part.added",
        2,
        "msg_a",
        &initial,
    ))?;
    reconciler.ingest(&delta(
        "response.output_text.annotation.added",
        3,
        "msg_a",
        0,
        json!({
            "content_index": 0,
            "annotation_index": 0,
            "annotation": annotation,
        }),
    ))?;
    reconciler.ingest(&part_event(
        "response.content_part.done",
        4,
        "msg_a",
        &complete,
    ))?;

    assert!(matches!(
        reconciler.ingest(&done(5, 0, item))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

#[test]
fn repeated_part_completion_is_distinct_and_conflicts_fail() -> TestResult {
    let complete = output_part("answer", None);
    let completion = part_event("response.content_part.done", 2, "msg_a", &complete);
    let mut duplicate = ResponseReconciler::new();
    duplicate.ingest(&added(1, 0, message_start("msg_a")))?;
    duplicate.ingest(&completion)?;
    assert_eq!(
        duplicate.ingest(&part_event(
            "response.content_part.done",
            3,
            "msg_a",
            &complete,
        ))?,
        ReconcileUpdate::DuplicateChannelCompletion
    );

    let mut conflict = ResponseReconciler::new();
    conflict.ingest(&added(1, 0, message_start("msg_a")))?;
    conflict.ingest(&completion)?;
    assert_eq!(
        conflict.ingest(&part_event(
            "response.content_part.done",
            3,
            "msg_a",
            &output_part("different", None),
        )),
        Err(ResponseReconciliationError::ConflictingItemScopedCompletion)
    );
    Ok(())
}

#[test]
fn conflicting_part_or_annotation_preview_fails_closed() -> TestResult {
    let seed = output_part("", None);
    let mut part_conflict = ResponseReconciler::new();
    part_conflict.ingest(&added(1, 0, message_start("msg_a")))?;
    part_conflict.ingest(&part_event(
        "response.content_part.added",
        2,
        "msg_a",
        &seed,
    ))?;
    assert_eq!(
        part_conflict.ingest(&part_event(
            "response.content_part.added",
            3,
            "msg_a",
            &output_part("changed", None),
        )),
        Err(ResponseReconciliationError::ConflictingItemScopedPreview)
    );

    let mut annotation_conflict = ResponseReconciler::new();
    annotation_conflict.ingest(&added(1, 0, message_start("msg_a")))?;
    annotation_conflict.ingest(&part_event(
        "response.content_part.added",
        2,
        "msg_a",
        &seed,
    ))?;
    for (sequence, url, expected) in [
        (3, "https://a.example", Ok(ReconcileUpdate::Accepted)),
        (
            4,
            "https://b.example",
            Err(ResponseReconciliationError::ConflictingItemScopedPreview),
        ),
    ] {
        let result = annotation_conflict.ingest(&delta(
            "response.output_text.annotation.added",
            sequence,
            "msg_a",
            0,
            json!({
                "content_index": 0,
                "annotation_index": 0,
                "annotation": {
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": 1,
                    "title": "Example",
                    "url": url
                },
            }),
        ));
        assert_eq!(result, expected);
    }
    Ok(())
}

#[test]
fn part_done_blocks_late_text_and_annotations() -> TestResult {
    let complete = output_part("answer", None);
    let mut late_text = ResponseReconciler::new();
    late_text.ingest(&added(1, 0, message_start("msg_a")))?;
    late_text.ingest(&part_event(
        "response.content_part.done",
        2,
        "msg_a",
        &complete,
    ))?;
    assert_eq!(
        late_text.ingest(&delta(
            "response.output_text.delta",
            3,
            "msg_a",
            0,
            json!({"content_index": 0, "delta": "late"}),
        )),
        Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
    );

    let mut late_annotation = ResponseReconciler::new();
    late_annotation.ingest(&added(1, 0, message_start("msg_a")))?;
    late_annotation.ingest(&part_event(
        "response.content_part.added",
        2,
        "msg_a",
        &output_part("", None),
    ))?;
    late_annotation.ingest(&part_event(
        "response.content_part.done",
        3,
        "msg_a",
        &complete,
    ))?;
    assert_eq!(
        late_annotation.ingest(&delta(
            "response.output_text.annotation.added",
            4,
            "msg_a",
            0,
            json!({
                "content_index": 0,
                "annotation_index": 0,
                "annotation": {"type": "file_citation"},
            }),
        )),
        Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
    );
    Ok(())
}

#[test]
fn part_and_annotation_authority_must_match_completed_item() -> TestResult {
    let annotation = json!({
        "type": "file_citation",
        "file_id": "file_a",
        "filename": "answer.txt",
        "index": 0
    });
    let complete = output_part("answer", Some(annotation.clone()));
    let mut part_mismatch = ResponseReconciler::new();
    part_mismatch.ingest(&added(1, 0, message_start("msg_a")))?;
    part_mismatch.ingest(&part_event(
        "response.content_part.done",
        2,
        "msg_a",
        &complete,
    ))?;
    assert_eq!(
        part_mismatch.ingest(&done(
            3,
            0,
            message_with_part("msg_a", &output_part("different", Some(annotation.clone())),),
        )),
        Err(ResponseReconciliationError::ItemScopedCompletionConflict)
    );

    let mut annotation_mismatch = ResponseReconciler::new();
    annotation_mismatch.ingest(&added(1, 0, message_start("msg_a")))?;
    annotation_mismatch.ingest(&part_event(
        "response.content_part.added",
        2,
        "msg_a",
        &output_part("", None),
    ))?;
    annotation_mismatch.ingest(&delta(
        "response.output_text.annotation.added",
        3,
        "msg_a",
        0,
        json!({
            "content_index": 0,
            "annotation_index": 0,
            "annotation": annotation,
        }),
    ))?;
    assert_eq!(
        annotation_mismatch.ingest(&done(
            4,
            0,
            message_with_part("msg_a", &output_part("answer", None)),
        )),
        Err(ResponseReconciliationError::ItemScopedCompletionConflict)
    );
    Ok(())
}

#[test]
fn reasoning_summary_parts_reconcile_with_final_opaque_summary_json() -> TestResult {
    let announced = json!({
        "id": "rs_a",
        "type": "reasoning",
        "summary": [],
        "content": [],
        "status": "in_progress"
    });
    let seed = json!({"type": "summary_text", "text": ""});
    let complete = json!({"type": "summary_text", "text": "summary"});
    let final_item = json!({
        "id": "rs_a",
        "type": "reasoning",
        "summary": [complete],
        "content": [],
        "encrypted_content": null,
        "status": "completed"
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, announced))?;
    reconciler.ingest(&delta(
        "response.reasoning_summary_part.added",
        2,
        "rs_a",
        0,
        json!({"summary_index": 0, "part": seed}),
    ))?;
    reconciler.ingest(&delta(
        "response.reasoning_summary_text.delta",
        3,
        "rs_a",
        0,
        json!({"summary_index": 0, "delta": "summary"}),
    ))?;
    reconciler.ingest(&delta(
        "response.reasoning_summary_part.done",
        4,
        "rs_a",
        0,
        json!({"summary_index": 0, "part": complete}),
    ))?;

    assert!(matches!(
        reconciler.ingest(&done(5, 0, final_item))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

#[test]
fn reasoning_summary_done_rejects_late_delta_and_final_conflict() -> TestResult {
    let announced = json!({
        "id": "rs_a",
        "type": "reasoning",
        "summary": [],
        "content": [],
        "status": "in_progress"
    });
    let complete = json!({"type": "summary_text", "text": "summary"});
    let completion = delta(
        "response.reasoning_summary_part.done",
        2,
        "rs_a",
        0,
        json!({"summary_index": 0, "part": complete}),
    );
    let mut late = ResponseReconciler::new();
    late.ingest(&added(1, 0, announced.clone()))?;
    late.ingest(&completion)?;
    assert_eq!(
        late.ingest(&delta(
            "response.reasoning_summary_text.delta",
            3,
            "rs_a",
            0,
            json!({"summary_index": 0, "delta": "late"}),
        )),
        Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
    );

    let mut conflict = ResponseReconciler::new();
    conflict.ingest(&added(1, 0, announced))?;
    conflict.ingest(&completion)?;
    assert_eq!(
        conflict.ingest(&done(
            3,
            0,
            json!({
                "id": "rs_a",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "different"}],
                "content": [],
                "encrypted_content": null,
                "status": "completed"
            }),
        )),
        Err(ResponseReconciliationError::ItemScopedCompletionConflict)
    );
    Ok(())
}
