use super::*;

#[test]
fn terminal_output_orders_items_and_synthesizes_missing_done_frames() -> TestResult {
    let reasoning = reasoning("rs_a");
    let message = message("msg_a", "answer");
    let call = function_call("fc_a", "{}", "completed");
    let hosted = json!({
        "id": "search_a",
        "type": "file_search_call",
        "queries": ["query"],
        "status": "completed"
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, reasoning.clone()))?;
    reconciler.ingest(&added(2, 1, message.clone()))?;
    reconciler.ingest(&added(3, 2, call.clone()))?;
    reconciler.ingest(&added(4, 3, hosted.clone()))?;
    reconciler.ingest(&done(5, 2, call.clone()))?;

    let update = reconciler.ingest(&event(
        "response.completed",
        6,
        json!({"response": {"output": [reasoning, message, call, hosted]}}),
    ))?;
    let ReconcileUpdate::Terminal {
        items,
        delta_reconciliations,
    } = update
    else {
        return Err("expected terminal update".into());
    };
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].item.raw(), &reasoning);
    assert_eq!(items[1].item.raw(), &message);
    assert_eq!(items[2].item.raw(), &call);
    assert_eq!(items[3].item.raw(), &hosted);
    assert_eq!(items[0].provenance.sequence_number, Some(6));
    assert_eq!(items[2].provenance.sequence_number, Some(5));
    assert_eq!(items[3].provenance.output_index, Some(3));
    assert!(delta_reconciliations.iter().any(|reconciliation| {
        reconciliation.channel == ResponseDeltaChannel::OutputText(0)
            && reconciliation.disposition == DeltaReconciliationDisposition::Synthesized
    }));
    assert_eq!(
        reconciler.accumulated_delta("msg_a", 1, ResponseDeltaChannel::OutputText(0)),
        Some("answer")
    );
    assert_eq!(
        reconciler.ingest(&event(
            "response.completed",
            6,
            json!({"response": {"output": [reasoning, message, call, hosted]}}),
        ))?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 6 }
    );
    Ok(())
}

#[test]
fn failed_response_still_synthesizes_its_authoritative_output() -> TestResult {
    let partial = message("msg_partial", "partial answer");
    let mut reconciler = ResponseReconciler::new();
    let update = reconciler.ingest(&event(
        "response.failed",
        1,
        json!({
            "response": {
                "status": "failed",
                "output": [partial],
                "error": {"code": "server_error", "message": "not rendered here"}
            }
        }),
    ))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal reconciliation for response.failed".into());
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.raw(), &partial);
    assert_eq!(items[0].provenance.output_index, Some(0));
    Ok(())
}

#[test]
fn terminal_output_rejects_conflicting_completion() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message("msg_a", "initial")))?;
    reconciler.ingest(&done(2, 0, message("msg_a", "complete")))?;
    assert_eq!(
        reconciler.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": [message("msg_a", "conflict")]}}),
        )),
        Err(ResponseReconciliationError::TerminalCompletionConflict)
    );
    Ok(())
}

#[test]
fn rejected_terminal_frame_preserves_every_accumulated_preview() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_first")))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        2,
        "msg_first",
        0,
        json!({"content_index": 0, "delta": "first", "logprobs": []}),
    ))?;
    reconciler.ingest(&added(3, 1, message_start("msg_second")))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        4,
        "msg_second",
        1,
        json!({"content_index": 0, "delta": "second", "logprobs": []}),
    ))?;

    assert_eq!(
        reconciler.ingest(&event(
            "response.completed",
            5,
            json!({"response": {"output": [
                message("msg_first", "first complete"),
                message("msg_second", "different")
            ]}}),
        )),
        Err(ResponseReconciliationError::AuthoritativeDeltaConflict)
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_first", 0, ResponseDeltaChannel::OutputText(0)),
        Some("first")
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_second", 1, ResponseDeltaChannel::OutputText(0)),
        Some("second")
    );
    Ok(())
}

#[test]
fn terminal_output_cannot_omit_or_reorder_completed_items() -> TestResult {
    let first = message("msg_first", "first");
    let second = message("msg_second", "second");
    let mut omitted = ResponseReconciler::new();
    omitted.ingest(&done(1, 0, first.clone()))?;
    assert_eq!(
        omitted.ingest(&event(
            "response.completed",
            2,
            json!({"response": {"output": []}}),
        )),
        Err(ResponseReconciliationError::CompletionAbsentFromTerminal)
    );

    let mut reordered = ResponseReconciler::new();
    reordered.ingest(&done(1, 0, first.clone()))?;
    reordered.ingest(&done(2, 1, second.clone()))?;
    assert_eq!(
        reordered.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": [second, first]}}),
        )),
        Err(ResponseReconciliationError::ItemIdRebound {
            item_id: "msg_second".to_owned(),
            prior_index: 1,
            new_index: 0,
        })
    );
    Ok(())
}

#[test]
fn terminal_output_cannot_omit_any_core_preview_delta_identity() -> TestResult {
    let cases = [
        (
            message_start("msg_text"),
            delta(
                "response.output_text.delta",
                2,
                "msg_text",
                0,
                json!({"content_index": 0, "delta": "text", "logprobs": []}),
            ),
        ),
        (
            message_start("msg_refusal"),
            delta(
                "response.refusal.delta",
                2,
                "msg_refusal",
                0,
                json!({"content_index": 0, "delta": "refusal"}),
            ),
        ),
        (
            json!({
                "id": "rs_summary",
                "type": "reasoning",
                "summary": [],
                "content": [],
                "encrypted_content": null,
                "status": "in_progress"
            }),
            delta(
                "response.reasoning_summary_text.delta",
                2,
                "rs_summary",
                0,
                json!({"summary_index": 0, "delta": "summary"}),
            ),
        ),
        (
            json!({
                "id": "rs_detail",
                "type": "reasoning",
                "summary": [],
                "content": [],
                "encrypted_content": null,
                "status": "in_progress"
            }),
            delta(
                "response.reasoning_text.delta",
                2,
                "rs_detail",
                0,
                json!({"content_index": 0, "delta": "detail"}),
            ),
        ),
    ];

    for terminal_type in ["response.completed", "response.incomplete"] {
        for (announced, preview) in &cases {
            let mut reconciler = ResponseReconciler::new();
            reconciler.ingest(&added(1, 0, announced.clone()))?;
            reconciler.ingest(preview)?;
            let error = reconciler
                .ingest(&event(
                    terminal_type,
                    3,
                    json!({"response": {"output": []}}),
                ))
                .err()
                .ok_or("expected a core-preview terminal omission error")?;
            assert_eq!(
                error,
                ResponseReconciliationError::CoreDeltaAbsentFromTerminal
            );
            assert!(!error.to_string().contains("msg_"));
            assert!(!error.to_string().contains("rs_"));
        }
    }
    Ok(())
}

#[test]
fn failed_terminal_preserves_failure_authority_over_orphan_preview() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_failed")))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        2,
        "msg_failed",
        0,
        json!({"content_index": 0, "delta": "partial", "logprobs": []}),
    ))?;
    assert!(matches!(
        reconciler.ingest(&event(
            "response.failed",
            3,
            json!({"response": {"output": []}}),
        ))?,
        ReconcileUpdate::Terminal { ref items, .. } if items.is_empty()
    ));
    Ok(())
}

#[test]
fn terminal_authority_may_supply_core_preview_without_intermediate_done() -> TestResult {
    let cases = [
        (
            message_start("msg_text"),
            delta(
                "response.output_text.delta",
                2,
                "msg_text",
                0,
                json!({"content_index": 0, "delta": "text", "logprobs": []}),
            ),
            message("msg_text", "text"),
        ),
        (
            message_start("msg_refusal"),
            delta(
                "response.refusal.delta",
                2,
                "msg_refusal",
                0,
                json!({"content_index": 0, "delta": "refusal"}),
            ),
            json!({
                "id": "msg_refusal",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "refusal", "refusal": "refusal"}]
            }),
        ),
        (
            json!({
                "id": "rs_summary",
                "type": "reasoning",
                "summary": [],
                "content": [],
                "encrypted_content": null,
                "status": "in_progress"
            }),
            delta(
                "response.reasoning_summary_text.delta",
                2,
                "rs_summary",
                0,
                json!({"summary_index": 0, "delta": "summary"}),
            ),
            json!({
                "id": "rs_summary",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "summary"}],
                "content": [],
                "encrypted_content": null,
                "status": "completed"
            }),
        ),
        (
            json!({
                "id": "rs_detail",
                "type": "reasoning",
                "summary": [],
                "content": [],
                "encrypted_content": null,
                "status": "in_progress"
            }),
            delta(
                "response.reasoning_text.delta",
                2,
                "rs_detail",
                0,
                json!({"content_index": 0, "delta": "detail"}),
            ),
            json!({
                "id": "rs_detail",
                "type": "reasoning",
                "summary": [],
                "content": [{"type": "reasoning_text", "text": "detail"}],
                "encrypted_content": null,
                "status": "completed"
            }),
        ),
    ];

    for (announced, preview, authoritative) in cases {
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, announced))?;
        reconciler.ingest(&preview)?;
        assert!(matches!(
            reconciler.ingest(&event(
                "response.completed",
                3,
                json!({"response": {"output": [authoritative]}}),
            ))?,
            ReconcileUpdate::Terminal { .. }
        ));
    }
    Ok(())
}

#[test]
fn delta_only_and_other_unresolved_executable_items_fail_closed() -> TestResult {
    let mut delta_only = ResponseReconciler::new();
    delta_only.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    delta_only.ingest(&delta(
        "response.function_call_arguments.delta",
        2,
        "fc_a",
        0,
        json!({"delta": "{}"}),
    ))?;
    assert_eq!(
        delta_only.ingest(&event(
            "response.incomplete",
            3,
            json!({"response": {"output": []}}),
        )),
        Err(ResponseReconciliationError::DeltaOnlyActionableCall)
    );

    let mut shell = ResponseReconciler::new();
    shell.ingest(&added(
        1,
        0,
        json!({
            "id": "shell_a",
            "type": "shell_call",
            "call_id": "shell_call_a",
            "status": "in_progress",
            "action": {
                "commands": ["printf a"],
                "timeout_ms": 1_000,
                "max_output_length": 4_096
            },
            "environment": {"type": "local"}
        }),
    ))?;
    let error = shell
        .ingest(&event(
            "response.incomplete",
            2,
            json!({"response": {"output": []}}),
        ))
        .err()
        .ok_or("expected an unsupported executable-item error")?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    Ok(())
}

#[test]
fn incomplete_authoritative_call_remains_unresolved() -> TestResult {
    let call = function_call("fc_a", "{}", "incomplete");
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, call.clone()))?;
    let error = reconciler
        .ingest(&event(
            "response.incomplete",
            2,
            json!({"response": {"output": [call]}}),
        ))
        .err()
        .ok_or("expected an unresolved executable-item error")?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnresolvedActionableItem { .. }
    ));
    Ok(())
}
