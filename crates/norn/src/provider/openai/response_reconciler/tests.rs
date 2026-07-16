use serde_json::{Value, json};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn event(event_type: &str, sequence_number: u64, mut data: Value) -> SseEvent {
    if let Some(object) = data.as_object_mut() {
        object.insert("sequence_number".to_owned(), json!(sequence_number));
    }
    SseEvent {
        event_type: event_type.to_owned(),
        data,
    }
}

fn added(sequence_number: u64, output_index: u64, item: Value) -> SseEvent {
    let mut data = json!({"output_index": output_index});
    if let Some(object) = data.as_object_mut() {
        object.insert("item".to_owned(), item);
    }
    event("response.output_item.added", sequence_number, data)
}

fn done(sequence_number: u64, output_index: u64, item: Value) -> SseEvent {
    let mut data = json!({"output_index": output_index});
    if let Some(object) = data.as_object_mut() {
        object.insert("item".to_owned(), item);
    }
    event("response.output_item.done", sequence_number, data)
}

fn delta(
    event_type: &str,
    sequence_number: u64,
    item_id: &str,
    output_index: u64,
    mut extra: Value,
) -> SseEvent {
    let mut data = json!({"item_id": item_id, "output_index": output_index});
    if let (Some(target), Some(fields)) = (data.as_object_mut(), extra.as_object_mut()) {
        target.append(fields);
    }
    event(event_type, sequence_number, data)
}

fn message(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [],
            "logprobs": []
        }]
    })
}

fn message_start(id: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": "in_progress",
        "content": []
    })
}

fn reasoning(id: &str) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": "summary"}],
        "content": [{"type": "reasoning_text", "text": "detail"}],
        "encrypted_content": null,
        "status": "completed"
    })
}

fn function_call(id: &str, arguments: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "call_id": format!("call_{id}"),
        "name": "lookup",
        "arguments": arguments,
        "status": status
    })
}

fn custom_call(id: &str, input: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "custom_tool_call",
        "call_id": format!("call_{id}"),
        "name": "patch",
        "input": input,
        "status": status
    })
}

#[test]
fn sequence_numbers_are_required_idempotent_and_monotonic() -> TestResult {
    let mut missing = ResponseReconciler::new();
    let missing_event = SseEvent {
        event_type: "response.created".to_owned(),
        data: json!({}),
    };
    assert_eq!(
        missing.ingest(&missing_event),
        Err(ResponseReconciliationError::MissingSequenceNumber)
    );

    let mut duplicate = ResponseReconciler::new();
    let first = event("response.created", 7, json!({"response": {"id": "r"}}));
    assert_eq!(duplicate.ingest(&first)?, ReconcileUpdate::Ignored);
    assert_eq!(
        duplicate.ingest(&first)?,
        ReconcileUpdate::DuplicateSequence { sequence_number: 7 }
    );

    let conflict = event("response.created", 7, json!({"response": {"id": "other"}}));
    assert_eq!(
        duplicate.ingest(&conflict),
        Err(ResponseReconciliationError::ConflictingDuplicateSequence { sequence_number: 7 })
    );

    let mut nonmonotonic = ResponseReconciler::new();
    assert_eq!(
        nonmonotonic.ingest(&event("response.created", 9, json!({})))?,
        ReconcileUpdate::Ignored
    );
    assert_eq!(
        nonmonotonic.ingest(&event("response.in_progress", 8, json!({}))),
        Err(ResponseReconciliationError::NonMonotonicSequence {
            sequence_number: 8,
            highest_sequence_number: 9
        })
    );
    Ok(())
}

#[test]
fn output_item_identity_cannot_be_rebound() -> TestResult {
    let base = message("msg_a", "a");
    let mut moved_id = ResponseReconciler::new();
    moved_id.ingest(&added(1, 0, base.clone()))?;
    assert_eq!(
        moved_id.ingest(&added(2, 1, base)),
        Err(ResponseReconciliationError::ItemIdRebound {
            item_id: "msg_a".to_owned(),
            prior_index: 0,
            new_index: 1
        })
    );

    let mut replaced_index = ResponseReconciler::new();
    replaced_index.ingest(&added(1, 0, message("msg_a", "a")))?;
    assert_eq!(
        replaced_index.ingest(&added(2, 0, message("msg_b", "b"))),
        Err(ResponseReconciliationError::OutputIndexRebound { output_index: 0 })
    );

    let mut changed_announcement = ResponseReconciler::new();
    changed_announcement.ingest(&added(1, 0, message("msg_a", "a")))?;
    assert_eq!(
        changed_announcement.ingest(&added(2, 0, message("msg_a", "changed"))),
        Err(ResponseReconciliationError::ConflictingAddedItem)
    );

    let mut changed_family = ResponseReconciler::new();
    changed_family.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    assert_eq!(
        changed_family.ingest(&done(2, 0, custom_call("fc_a", "complete", "completed"))),
        Err(ResponseReconciliationError::AddedItemKindConflict)
    );
    Ok(())
}

#[test]
fn interleaved_deltas_accumulate_only_under_exact_identity_and_index() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message_start("msg_a")))?;
    reconciler.ingest(&added(2, 1, message_start("msg_b")))?;
    reconciler.ingest(&added(3, 2, reasoning("rs_a")))?;
    reconciler.ingest(&added(4, 3, function_call("fc_a", "", "in_progress")))?;
    reconciler.ingest(&added(5, 4, custom_call("ct_a", "", "in_progress")))?;

    reconciler.ingest(&delta(
        "response.output_text.delta",
        6,
        "msg_a",
        0,
        json!({"content_index": 0, "delta": "hel", "logprobs": []}),
    ))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        7,
        "msg_b",
        1,
        json!({"content_index": 0, "delta": "other", "logprobs": []}),
    ))?;
    reconciler.ingest(&delta(
        "response.output_text.delta",
        8,
        "msg_a",
        0,
        json!({"content_index": 0, "delta": "lo", "logprobs": []}),
    ))?;
    reconciler.ingest(&delta(
        "response.refusal.delta",
        9,
        "msg_a",
        0,
        json!({"content_index": 1, "delta": "no"}),
    ))?;
    reconciler.ingest(&delta(
        "response.reasoning_summary_text.delta",
        10,
        "rs_a",
        2,
        json!({"summary_index": 0, "delta": "sum"}),
    ))?;
    reconciler.ingest(&delta(
        "response.reasoning_text.delta",
        11,
        "rs_a",
        2,
        json!({"content_index": 0, "delta": "detail"}),
    ))?;
    reconciler.ingest(&delta(
        "response.function_call_arguments.delta",
        12,
        "fc_a",
        3,
        json!({"delta": "{\"a\":"}),
    ))?;
    reconciler.ingest(&delta(
        "response.function_call_arguments.delta",
        13,
        "fc_a",
        3,
        json!({"delta": "1}"}),
    ))?;
    reconciler.ingest(&delta(
        "response.custom_tool_call_input.delta",
        14,
        "ct_a",
        4,
        json!({"delta": "patch"}),
    ))?;

    assert_eq!(
        reconciler.accumulated_delta("msg_a", 0, ResponseDeltaChannel::OutputText(0)),
        Some("hello")
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_b", 1, ResponseDeltaChannel::OutputText(0)),
        Some("other")
    );
    assert_eq!(
        reconciler.accumulated_delta("msg_a", 0, ResponseDeltaChannel::Refusal(1)),
        Some("no")
    );
    assert_eq!(
        reconciler.accumulated_delta("rs_a", 2, ResponseDeltaChannel::ReasoningSummaryText(0)),
        Some("sum")
    );
    assert_eq!(
        reconciler.accumulated_delta("rs_a", 2, ResponseDeltaChannel::ReasoningText(0)),
        Some("detail")
    );
    assert_eq!(
        reconciler.accumulated_delta("fc_a", 3, ResponseDeltaChannel::FunctionCallArguments),
        Some("{\"a\":1}")
    );
    assert_eq!(
        reconciler.accumulated_delta("ct_a", 4, ResponseDeltaChannel::CustomToolCallInput),
        Some("patch")
    );
    Ok(())
}

#[test]
fn deltas_require_an_announced_identity_of_the_matching_family() -> TestResult {
    let unknown = delta(
        "response.output_text.delta",
        1,
        "msg_missing",
        0,
        json!({"content_index": 0, "delta": "text", "logprobs": []}),
    );
    assert_eq!(
        ResponseReconciler::new().ingest(&unknown),
        Err(ResponseReconciliationError::UnannouncedDeltaIdentity)
    );

    let mut wrong_family = ResponseReconciler::new();
    wrong_family.ingest(&added(1, 0, reasoning("rs_a")))?;
    assert_eq!(
        wrong_family.ingest(&delta(
            "response.output_text.delta",
            2,
            "rs_a",
            0,
            json!({"content_index": 0, "delta": "not a message", "logprobs": []}),
        )),
        Err(ResponseReconciliationError::DeltaItemKindConflict)
    );

    let mut wrong_index = ResponseReconciler::new();
    wrong_index.ingest(&added(1, 0, message_start("msg_a")))?;
    wrong_index.ingest(&delta(
        "response.output_text.delta",
        2,
        "msg_a",
        0,
        json!({"content_index": 1, "delta": "wrong index", "logprobs": []}),
    ))?;
    assert_eq!(
        wrong_index.ingest(&done(3, 0, message("msg_a", "complete"))),
        Err(ResponseReconciliationError::DeltaItemKindConflict)
    );
    Ok(())
}

#[test]
fn completed_items_are_authoritative_and_exact_duplicates_are_idempotent() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    reconciler.ingest(&delta(
        "response.function_call_arguments.delta",
        2,
        "fc_a",
        0,
        json!({"delta": "author"}),
    ))?;
    let complete = function_call("fc_a", "authoritative", "completed");
    let completion = reconciler.ingest(&done(3, 0, complete.clone()))?;
    let ReconcileUpdate::CompletedItem {
        item,
        delta_reconciliations,
    } = completion
    else {
        return Err("expected completed item update".into());
    };
    assert_eq!(item.item.raw(), &complete);
    assert_eq!(delta_reconciliations.len(), 1);
    assert_eq!(
        delta_reconciliations[0].disposition,
        DeltaReconciliationDisposition::Repaired
    );
    assert_eq!(delta_reconciliations[0].repair.as_deref(), Some("itative"));
    assert_eq!(
        reconciler.accumulated_delta("fc_a", 0, ResponseDeltaChannel::FunctionCallArguments),
        Some("authoritative")
    );
    let repeated = reconciler.ingest(&done(4, 0, complete))?;
    let ReconcileUpdate::DuplicateCompletion { identity } = repeated else {
        return Err("expected duplicate completion".into());
    };
    assert_eq!(identity.item_id(), Some("fc_a"));
    assert_eq!(identity.output_index(), 0);

    assert_eq!(
        reconciler.ingest(&done(5, 0, function_call("fc_a", "different", "completed"))),
        Err(ResponseReconciliationError::ConflictingCompletion)
    );
    Ok(())
}

#[test]
fn deltas_after_authoritative_completion_fail_closed() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    reconciler.ingest(&done(2, 0, function_call("fc_a", "{}", "completed")))?;
    assert_eq!(
        reconciler.ingest(&delta(
            "response.function_call_arguments.delta",
            3,
            "fc_a",
            0,
            json!({"delta": "late"}),
        )),
        Err(ResponseReconciliationError::DeltaAfterCompletion)
    );
    Ok(())
}

mod audio;
mod call_identity;
mod channels;
mod content_parts;
mod hosted_items;
mod roles;
mod schema_contract;
mod sequence;
mod support;
mod terminal;
