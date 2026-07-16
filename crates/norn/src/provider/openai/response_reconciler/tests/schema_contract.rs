use super::*;

fn part_event(item_id: &str, part: &Value) -> SseEvent {
    delta(
        "response.content_part.added",
        2,
        item_id,
        0,
        json!({"content_index": 0, "part": part}),
    )
}

fn hosted_event(event_type: &str, item_id: &str) -> SseEvent {
    delta(event_type, 2, item_id, 0, json!({}))
}

#[test]
fn content_part_events_require_the_normative_payload_shape() -> TestResult {
    for (item_id, announcement, part, field) in [
        (
            "msg_a",
            message_start("msg_a"),
            json!({"type": "output_text", "text": "", "annotations": []}),
            "part.logprobs",
        ),
        (
            "msg_b",
            message_start("msg_b"),
            json!({"type": "refusal"}),
            "part.refusal",
        ),
        (
            "rs_a",
            json!({
                "id": "rs_a",
                "type": "reasoning",
                "summary": [],
                "content": [],
                "status": "in_progress"
            }),
            json!({"type": "reasoning_text"}),
            "part.text",
        ),
    ] {
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, announcement))?;
        assert_eq!(
            reconciler.ingest(&part_event(item_id, &part)),
            Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.content_part.added",
                field,
            })
        );
    }

    let mut summary = ResponseReconciler::new();
    summary.ingest(&added(
        1,
        0,
        json!({
            "id": "rs_b",
            "type": "reasoning",
            "summary": [],
            "content": [],
            "status": "in_progress"
        }),
    ))?;
    assert_eq!(
        summary.ingest(&delta(
            "response.reasoning_summary_part.added",
            2,
            "rs_b",
            0,
            json!({"summary_index": 0, "part": {"type": "summary_text"}}),
        )),
        Err(ResponseReconciliationError::InvalidEnvelopeField {
            event_type: "response.reasoning_summary_part.added",
            field: "part.text",
        })
    );
    Ok(())
}

#[test]
fn content_part_logprob_bytes_accept_numbers_and_reject_other_shapes() -> TestResult {
    let valid = json!({
        "type": "output_text",
        "text": "answer",
        "annotations": [],
        "logprobs": [{
            "token": "answer",
            "bytes": [65, 1.5],
            "logprob": -0.1,
            "top_logprobs": [{
                "token": "Answer",
                "bytes": [66, 2.5],
                "logprob": -0.2
            }]
        }]
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_numeric_bytes")))?;
    assert_eq!(
        reconciler.ingest(&part_event("msg_numeric_bytes", &valid))?,
        ReconcileUpdate::Accepted
    );

    for (pointer, replacement, field) in [
        (
            "/logprobs/0/bytes/0",
            json!("not-a-number"),
            "part.logprobs[].bytes[]",
        ),
        (
            "/logprobs/0/top_logprobs/0/bytes/0",
            json!({"not": "a number"}),
            "part.logprobs[].top_logprobs[].bytes[]",
        ),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).ok_or("missing byte fixture")? = replacement;
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, message_start("msg_invalid_bytes")))?;
        assert_eq!(
            reconciler.ingest(&part_event("msg_invalid_bytes", &invalid)),
            Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.content_part.added",
                field,
            })
        );
    }
    Ok(())
}

#[test]
fn output_text_delta_requires_the_normative_logprob_shape() -> TestResult {
    for (fields, field) in [
        (json!({"content_index": 0, "delta": "answer"}), "logprobs"),
        (
            json!({"content_index": 0, "delta": "answer", "logprobs": null}),
            "logprobs",
        ),
        (
            json!({"content_index": 0, "delta": "answer", "logprobs": [{}]}),
            "logprobs[].token",
        ),
        (
            json!({
                "content_index": 0,
                "delta": "answer",
                "logprobs": [{"token": "answer", "logprob": "invalid"}]
            }),
            "logprobs[].logprob",
        ),
        (
            json!({
                "content_index": 0,
                "delta": "answer",
                "logprobs": [{"token": "answer", "logprob": -0.1, "top_logprobs": {}}]
            }),
            "logprobs[].top_logprobs",
        ),
        (
            json!({
                "content_index": 0,
                "delta": "answer",
                "logprobs": [{"token": "answer", "logprob": -0.1, "top_logprobs": [null]}]
            }),
            "logprobs[].top_logprobs[]",
        ),
        (
            json!({
                "content_index": 0,
                "delta": "answer",
                "logprobs": [{
                    "token": "answer",
                    "logprob": -0.1,
                    "top_logprobs": [{"token": 1, "logprob": -0.2}]
                }]
            }),
            "logprobs[].top_logprobs[].token",
        ),
        (
            json!({
                "content_index": 0,
                "delta": "answer",
                "logprobs": [{
                    "token": "answer",
                    "logprob": -0.1,
                    "top_logprobs": [{"token": "Answer", "logprob": "invalid"}]
                }]
            }),
            "logprobs[].top_logprobs[].logprob",
        ),
    ] {
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, message_start("msg_logprobs")))?;
        assert_eq!(
            reconciler.ingest(&delta(
                "response.output_text.delta",
                2,
                "msg_logprobs",
                0,
                fields,
            )),
            Err(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.output_text.delta",
                field,
            })
        );
    }

    for fields in [
        json!({
            "content_index": 0,
            "delta": "answer",
            "logprobs": [{
                "token": "answer",
                "logprob": -0.1,
                "top_logprobs": [{"token": "Answer", "logprob": -0.2}]
            }]
        }),
        json!({
            "content_index": 0,
            "delta": "answer",
            "logprobs": [{
                "token": "answer",
                "logprob": -0.1,
                "top_logprobs": [{"token": "Answer"}, {"logprob": -0.2}, {}]
            }]
        }),
        json!({
            "content_index": 0,
            "delta": "answer",
            "logprobs": [{"token": "answer", "logprob": -0.1}]
        }),
    ] {
        let mut valid = ResponseReconciler::new();
        valid.ingest(&added(1, 0, message_start("msg_logprobs")))?;
        assert_eq!(
            valid.ingest(&delta(
                "response.output_text.delta",
                2,
                "msg_logprobs",
                0,
                fields,
            ))?,
            ReconcileUpdate::Accepted
        );
    }
    Ok(())
}

#[test]
fn completed_hosted_lifecycle_requires_completed_final_status() -> TestResult {
    for (item, lifecycle) in [
        (
            json!({
                "id": "fs_a",
                "type": "file_search_call",
                "queries": ["query"],
                "status": "searching"
            }),
            "response.file_search_call.completed",
        ),
        (
            json!({
                "id": "ws_a",
                "type": "web_search_call",
                "action": {"type": "search", "query": "query"},
                "status": "searching"
            }),
            "response.web_search_call.completed",
        ),
        (
            json!({
                "id": "ig_a",
                "type": "image_generation_call",
                "result": null,
                "status": "generating"
            }),
            "response.image_generation_call.completed",
        ),
        (
            json!({
                "id": "ci_a",
                "type": "code_interpreter_call",
                "code": null,
                "container_id": "cntr_a",
                "outputs": null,
                "status": "interpreting"
            }),
            "response.code_interpreter_call.completed",
        ),
    ] {
        let item_id = item["id"].as_str().ok_or("missing test item id")?;
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, item.clone()))?;
        reconciler.ingest(&hosted_event(lifecycle, item_id))?;
        assert_eq!(
            reconciler.ingest(&done(3, 0, item)),
            Err(ResponseReconciliationError::ItemScopedCompletionConflict)
        );
    }
    Ok(())
}

#[test]
fn mcp_list_tools_lifecycle_does_not_invent_error_cross_field_rules() -> TestResult {
    let failed_item = json!({
        "id": "list_a",
        "type": "mcp_list_tools",
        "server_label": "server",
        "tools": []
    });
    let mut failed = ResponseReconciler::new();
    failed.ingest(&added(1, 0, failed_item.clone()))?;
    failed.ingest(&hosted_event("response.mcp_list_tools.failed", "list_a"))?;
    assert!(matches!(
        failed.ingest(&done(3, 0, failed_item))?,
        ReconcileUpdate::CompletedItem { .. }
    ));

    let completed_item = json!({
        "id": "list_b",
        "type": "mcp_list_tools",
        "server_label": "server",
        "tools": [],
        "error": "failed"
    });
    let mut completed = ResponseReconciler::new();
    completed.ingest(&added(1, 0, completed_item.clone()))?;
    completed.ingest(&hosted_event("response.mcp_list_tools.completed", "list_b"))?;
    assert!(matches!(
        completed.ingest(&done(3, 0, completed_item))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}
