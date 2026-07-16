use super::*;

struct ChannelCase {
    item_id: &'static str,
    announced: Value,
    delta_type: &'static str,
    delta_fields: Option<Value>,
    done_type: &'static str,
    done_fields: Value,
    channel: ResponseDeltaChannel,
    authoritative: &'static str,
    disposition: DeltaReconciliationDisposition,
    completed_item: Value,
}

fn refusal_message(id: &str, refusal: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{"type": "refusal", "refusal": refusal}]
    })
}

fn reasoning_start(id: &str) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "summary": [],
        "content": [],
        "status": "in_progress"
    })
}

fn channel_cases() -> Vec<ChannelCase> {
    vec![
        ChannelCase {
            item_id: "msg_text",
            announced: message_start("msg_text"),
            delta_type: "response.output_text.delta",
            delta_fields: Some(json!({"content_index": 0, "delta": "answer"})),
            done_type: "response.output_text.done",
            done_fields: json!({"content_index": 0, "text": "answer"}),
            channel: ResponseDeltaChannel::OutputText(0),
            authoritative: "answer",
            disposition: DeltaReconciliationDisposition::Matched,
            completed_item: message("msg_text", "answer"),
        },
        ChannelCase {
            item_id: "msg_refusal",
            announced: message_start("msg_refusal"),
            delta_type: "response.refusal.delta",
            delta_fields: Some(json!({"content_index": 0, "delta": "partial"})),
            done_type: "response.refusal.done",
            done_fields: json!({"content_index": 0, "refusal": "cannot"}),
            channel: ResponseDeltaChannel::Refusal(0),
            authoritative: "cannot",
            disposition: DeltaReconciliationDisposition::Repaired,
            completed_item: refusal_message("msg_refusal", "cannot"),
        },
        ChannelCase {
            item_id: "rs_detail",
            announced: reasoning_start("rs_detail"),
            delta_type: "response.reasoning_text.delta",
            delta_fields: None,
            done_type: "response.reasoning_text.done",
            done_fields: json!({"content_index": 0, "text": "detail"}),
            channel: ResponseDeltaChannel::ReasoningText(0),
            authoritative: "detail",
            disposition: DeltaReconciliationDisposition::Synthesized,
            completed_item: reasoning("rs_detail"),
        },
        ChannelCase {
            item_id: "rs_summary",
            announced: reasoning_start("rs_summary"),
            delta_type: "response.reasoning_summary_text.delta",
            delta_fields: Some(json!({"summary_index": 0, "delta": "summary"})),
            done_type: "response.reasoning_summary_text.done",
            done_fields: json!({"summary_index": 0, "text": "summary"}),
            channel: ResponseDeltaChannel::ReasoningSummaryText(0),
            authoritative: "summary",
            disposition: DeltaReconciliationDisposition::Matched,
            completed_item: reasoning("rs_summary"),
        },
        ChannelCase {
            item_id: "fc_done",
            announced: function_call("fc_done", "", "in_progress"),
            delta_type: "response.function_call_arguments.delta",
            delta_fields: Some(json!({"delta": "{"})),
            done_type: "response.function_call_arguments.done",
            done_fields: json!({"arguments": "{}", "name": "lookup"}),
            channel: ResponseDeltaChannel::FunctionCallArguments,
            authoritative: "{}",
            disposition: DeltaReconciliationDisposition::Repaired,
            completed_item: function_call("fc_done", "{}", "completed"),
        },
        ChannelCase {
            item_id: "ct_done",
            announced: custom_call("ct_done", "", "in_progress"),
            delta_type: "response.custom_tool_call_input.delta",
            delta_fields: None,
            done_type: "response.custom_tool_call_input.done",
            done_fields: json!({"input": "patch"}),
            channel: ResponseDeltaChannel::CustomToolCallInput,
            authoritative: "patch",
            disposition: DeltaReconciliationDisposition::Synthesized,
            completed_item: custom_call("ct_done", "patch", "completed"),
        },
    ]
}

#[test]
fn all_supported_channel_done_events_reconcile_and_remain_idempotent() -> TestResult {
    for case in channel_cases() {
        let mut reconciler = ResponseReconciler::new();
        reconciler.ingest(&added(1, 0, case.announced))?;
        if let Some(fields) = case.delta_fields {
            reconciler.ingest(&delta(case.delta_type, 2, case.item_id, 0, fields))?;
        }
        let completion = delta(case.done_type, 3, case.item_id, 0, case.done_fields.clone());
        assert_eq!(reconciler.ingest(&completion)?, ReconcileUpdate::Accepted);
        assert_eq!(
            reconciler.accumulated_delta(case.item_id, 0, case.channel),
            Some(case.authoritative)
        );
        assert_eq!(
            reconciler.completed_channel_reconciliation(case.item_id, 0, case.channel),
            Some(case.disposition)
        );
        let repeated = delta(case.done_type, 4, case.item_id, 0, case.done_fields);
        assert_eq!(
            reconciler.ingest(&repeated)?,
            ReconcileUpdate::DuplicateChannelCompletion
        );
        assert_eq!(
            reconciler.completed_channel_reconciliation(case.item_id, 0, case.channel),
            Some(case.disposition)
        );
        reconciler.ingest(&done(5, 0, case.completed_item.clone()))?;
        assert!(matches!(
            reconciler.ingest(&event(
                "response.completed",
                6,
                json!({"response": {"output": [case.completed_item]}}),
            ))?,
            ReconcileUpdate::Terminal { .. }
        ));
    }
    Ok(())
}

#[test]
fn channel_done_requires_announcement_identity_and_matching_family() -> TestResult {
    let unannounced = delta(
        "response.output_text.done",
        1,
        "msg_missing",
        0,
        json!({"content_index": 0, "text": "answer"}),
    );
    assert_eq!(
        ResponseReconciler::new().ingest(&unannounced),
        Err(ResponseReconciliationError::UnannouncedChannelCompletionIdentity)
    );

    let mut wrong_family = ResponseReconciler::new();
    wrong_family.ingest(&added(1, 0, reasoning_start("rs_wrong")))?;
    assert_eq!(
        wrong_family.ingest(&delta(
            "response.output_text.done",
            2,
            "rs_wrong",
            0,
            json!({"content_index": 0, "text": "answer"}),
        )),
        Err(ResponseReconciliationError::ChannelCompletionItemKindConflict)
    );
    Ok(())
}

#[test]
fn conflicting_channel_completion_and_late_delta_fail_closed() -> TestResult {
    let mut conflict = ResponseReconciler::new();
    conflict.ingest(&added(1, 0, message_start("msg_conflict")))?;
    conflict.ingest(&delta(
        "response.output_text.done",
        2,
        "msg_conflict",
        0,
        json!({"content_index": 0, "text": "first"}),
    ))?;
    assert_eq!(
        conflict.ingest(&delta(
            "response.output_text.done",
            3,
            "msg_conflict",
            0,
            json!({"content_index": 0, "text": "different"}),
        )),
        Err(ResponseReconciliationError::ConflictingChannelCompletion)
    );

    let mut late_delta = ResponseReconciler::new();
    late_delta.ingest(&added(1, 0, function_call("fc_late", "", "in_progress")))?;
    late_delta.ingest(&delta(
        "response.function_call_arguments.done",
        2,
        "fc_late",
        0,
        json!({"arguments": "{}", "name": "lookup"}),
    ))?;
    assert_eq!(
        late_delta.ingest(&delta(
            "response.function_call_arguments.delta",
            3,
            "fc_late",
            0,
            json!({"delta": "late"}),
        )),
        Err(ResponseReconciliationError::DeltaAfterChannelCompletion)
    );
    Ok(())
}

#[test]
fn item_and_terminal_authority_must_agree_with_channel_completion() -> TestResult {
    let mut item_conflict = ResponseReconciler::new();
    item_conflict.ingest(&added(1, 0, message_start("msg_item")))?;
    item_conflict.ingest(&delta(
        "response.output_text.done",
        2,
        "msg_item",
        0,
        json!({"content_index": 0, "text": "authoritative"}),
    ))?;
    assert_eq!(
        item_conflict.ingest(&done(3, 0, message("msg_item", "different"))),
        Err(ResponseReconciliationError::ChannelItemCompletionConflict)
    );

    let mut terminal_conflict = ResponseReconciler::new();
    terminal_conflict.ingest(&added(1, 0, message_start("msg_terminal")))?;
    terminal_conflict.ingest(&delta(
        "response.output_text.done",
        2,
        "msg_terminal",
        0,
        json!({"content_index": 0, "text": "authoritative"}),
    ))?;
    assert_eq!(
        terminal_conflict.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": [message("msg_terminal", "different")]}}),
        )),
        Err(ResponseReconciliationError::ChannelItemCompletionConflict)
    );

    let mut omitted = ResponseReconciler::new();
    omitted.ingest(&added(1, 0, message_start("msg_omitted")))?;
    omitted.ingest(&delta(
        "response.output_text.done",
        2,
        "msg_omitted",
        0,
        json!({"content_index": 0, "text": "answer"}),
    ))?;
    assert_eq!(
        omitted.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": []}}),
        )),
        Err(ResponseReconciliationError::ChannelCompletionAbsentFromTerminal)
    );
    Ok(())
}
