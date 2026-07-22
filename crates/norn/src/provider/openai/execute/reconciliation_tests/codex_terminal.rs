use super::*;
use crate::provider::openai::request::{
    CATALOG_BACKEND_CODEX_SUBSCRIPTION, CATALOG_BACKEND_RESPONSES_API,
};

fn codex_completed(sequence: u64, output: Option<&[Value]>) -> SseEvent {
    let response = output.map_or_else(
        || {
            json!({
                "id": "resp_codex",
                "status": "completed",
                "end_turn": true,
            })
        },
        |output| {
            json!({
                "id": "resp_codex",
                "status": "completed",
                "end_turn": true,
                "output": output,
            })
        },
    );
    SseEvent {
        event_type: "response.completed".to_owned(),
        data: json!({
            "type": "response.completed",
            "sequence_number": sequence,
            "response": response,
        }),
    }
}

fn codex_incomplete(sequence: u64) -> SseEvent {
    SseEvent {
        event_type: "response.incomplete".to_owned(),
        data: json!({
            "type": "response.incomplete",
            "sequence_number": sequence,
            "response": {
                "id": "resp_incomplete",
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
            },
        }),
    }
}

fn codex_failed(sequence: u64) -> SseEvent {
    SseEvent {
        event_type: "response.failed".to_owned(),
        data: json!({
            "type": "response.failed",
            "sequence_number": sequence,
            "response": {
                "id": "resp_failed",
                "status": "failed",
                "error": {
                    "code": "server_is_overloaded",
                    "message": "provider-controlled detail",
                },
            },
        }),
    }
}

fn added_item(sequence: u64, output_index: u64, item: &Value) -> SseEvent {
    SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": sequence,
            "output_index": output_index,
            "item": item,
        }),
    }
}

fn item_event(
    event_type: &str,
    sequence: u64,
    item_id: &str,
    output_index: u64,
    fields: &Value,
) -> Result<SseEvent, &'static str> {
    let fields = fields
        .as_object()
        .ok_or("item event fields must be an object")?;
    let mut data = serde_json::Map::new();
    data.insert("type".to_owned(), json!(event_type));
    data.insert("sequence_number".to_owned(), json!(sequence));
    data.insert("item_id".to_owned(), json!(item_id));
    data.insert("output_index".to_owned(), json!(output_index));
    data.extend(fields.clone());
    Ok(SseEvent {
        event_type: event_type.to_owned(),
        data: Value::Object(data),
    })
}

fn message_start(id: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": "in_progress",
        "content": [],
    })
}

fn completed_message(id: &str, text: &str) -> Value {
    message(
        id,
        &json!([{
            "type": "output_text",
            "text": text,
            "annotations": [],
            "logprobs": [],
        }]),
    )
}

fn function_call_start(id: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": format!("call_{id}"),
        "name": "lookup",
        "arguments": "",
        "status": "in_progress",
    })
}

fn completed_function_call(id: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": format!("call_{id}"),
        "name": "lookup",
        "arguments": "{}",
        "status": "completed",
    })
}

fn completed_custom_call(id: &str) -> Value {
    json!({
        "type": "custom_tool_call",
        "id": id,
        "call_id": format!("call_{id}"),
        "name": "patch",
        "input": "change",
        "status": "completed",
    })
}

fn assert_protocol_error(
    events: &[Result<ProviderEvent, ProviderError>],
    expected: &ResponseReconciliationError,
) {
    assert!(matches!(
        events.last(),
        Some(Err(ProviderError::ResponseProtocolViolation { source })) if source == expected
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(ProviderEvent::Done { .. })))
    );
}

#[test]
fn codex_reference_sequence_uses_done_items_in_output_index_order() -> TestResult {
    let first = completed_message("msg_first", "Hello");
    let second = completed_message("msg_second", "World");
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);

    only_ok(mapper.map_event(&done_item(0, 1, &second)))?;
    only_ok(mapper.map_event(&done_item(1, 0, &first)))?;
    let terminal = mapper.map_event(&codex_completed(2, None));

    let ids = terminal
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderEvent::ResponseItemDone { item }) => item.item.id(),
            Ok(_) | Err(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["msg_first", "msg_second"]);
    assert!(matches!(
        terminal.last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
    only_ok(terminal)?;
    Ok(())
}

#[test]
fn codex_empty_terminal_output_uses_done_item_authority() -> TestResult {
    let item = completed_message("msg_empty_output", "answer");
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);

    only_ok(mapper.map_event(&done_item(0, 0, &item)))?;
    let terminal = mapper.map_event(&codex_completed(1, Some(&[])));

    assert_eq!(
        terminal
            .iter()
            .filter(|event| matches!(event, Ok(ProviderEvent::ResponseItemDone { .. })))
            .count(),
        1
    );
    assert!(matches!(
        terminal.last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
    only_ok(terminal)?;
    Ok(())
}

#[test]
fn public_responses_still_requires_terminal_output_authority() -> TestResult {
    let item = completed_message("msg_public", "answer");

    let mut missing = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_RESPONSES_API);
    only_ok(missing.map_event(&done_item(0, 0, &item)))?;
    assert_protocol_error(
        &missing.map_event(&codex_completed(1, None)),
        &ResponseReconciliationError::MissingTerminalOutput,
    );

    let mut empty = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_RESPONSES_API);
    only_ok(empty.map_event(&done_item(0, 0, &item)))?;
    assert_protocol_error(
        &empty.map_event(&codex_completed(1, Some(&[]))),
        &ResponseReconciliationError::CompletionAbsentFromTerminal,
    );
    Ok(())
}

#[test]
fn codex_nonempty_terminal_output_must_match_done_items() -> TestResult {
    let completed = completed_message("msg_conflict", "authoritative done");
    let conflict = completed_message("msg_conflict", "conflicting terminal");
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);

    only_ok(mapper.map_event(&done_item(0, 0, &completed)))?;
    assert_protocol_error(
        &mapper.map_event(&codex_completed(1, Some(&[conflict]))),
        &ResponseReconciliationError::TerminalCompletionConflict,
    );
    Ok(())
}

#[test]
fn codex_done_tool_calls_reach_canonical_output_and_tool_use() -> TestResult {
    let function = completed_function_call("fc_complete");
    let custom = completed_custom_call("ct_complete");
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);

    only_ok(mapper.map_event(&done_item(0, 1, &custom)))?;
    only_ok(mapper.map_event(&done_item(1, 0, &function)))?;
    let terminal = mapper.map_event(&codex_completed(2, None));

    let item_types = terminal
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderEvent::ResponseItemDone { item }) => Some(item.item.item_type()),
            Ok(_) | Err(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(item_types, ["function_call", "custom_tool_call"]);
    assert!(matches!(
        terminal.last(),
        Some(Ok(ProviderEvent::Done {
            stop_reason: crate::provider::events::StopReason::ToolUse,
            ..
        }))
    ));
    only_ok(terminal)?;
    Ok(())
}

#[test]
fn codex_incomplete_uses_done_authority_but_rejects_unfinished_state() -> TestResult {
    let item = completed_message("msg_truncated", "partial answer");
    let mut complete = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    only_ok(complete.map_event(&done_item(0, 0, &item)))?;
    let terminal = complete.map_event(&codex_incomplete(1));
    assert!(
        terminal
            .iter()
            .any(|event| matches!(event, Ok(ProviderEvent::ResponseItemDone { .. })))
    );
    assert!(matches!(
        terminal.last(),
        Some(Ok(ProviderEvent::Done {
            stop_reason: crate::provider::events::StopReason::MaxTokens,
            ..
        }))
    ));
    only_ok(terminal)?;

    let mut unfinished = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let unfinished_announcement = message_start("msg_unfinished");
    only_ok(unfinished.map_event(&added_item(0, 0, &unfinished_announcement)))?;
    only_ok(unfinished.map_event(&item_event(
        "response.output_text.delta",
        1,
        "msg_unfinished",
        0,
        &json!({"content_index": 0, "delta": "partial", "logprobs": []}),
    )?))?;
    assert_protocol_error(
        &unfinished.map_event(&codex_incomplete(2)),
        &ResponseReconciliationError::CoreDeltaAbsentFromTerminal,
    );
    Ok(())
}

#[test]
fn codex_failed_without_output_preserves_provider_failure_authority() -> TestResult {
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let failed_announcement = message_start("msg_failed");
    only_ok(mapper.map_event(&added_item(0, 0, &failed_announcement)))?;
    only_ok(mapper.map_event(&item_event(
        "response.output_text.delta",
        1,
        "msg_failed",
        0,
        &json!({"content_index": 0, "delta": "partial", "logprobs": []}),
    )?))?;

    let terminal = mapper.map_event(&codex_failed(2));
    assert!(matches!(
        terminal.last(),
        Some(Err(ProviderError::StreamError {
            transient: Some(crate::error::TransientKind::ServerError { status: 503 }),
            ..
        }))
    ));
    assert!(!terminal.iter().any(|event| matches!(
        event,
        Err(ProviderError::ResponseProtocolViolation {
            source: ResponseReconciliationError::MissingTerminalOutput,
        })
    )));
    Ok(())
}

#[test]
fn codex_malformed_non_array_terminal_output_fails_closed() {
    let terminal = SseEvent {
        event_type: "response.completed".to_owned(),
        data: json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_codex",
                "status": "completed",
                "end_turn": true,
                "output": {"not": "an array"},
            },
        }),
    };
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);

    assert_protocol_error(
        &mapper.map_event(&terminal),
        &ResponseReconciliationError::MissingTerminalOutput,
    );
}

#[test]
fn codex_fallback_requires_contiguous_zero_based_output_indices() -> TestResult {
    let first = completed_message("msg_first", "first");
    let third = completed_message("msg_third", "third");

    let mut starts_at_one =
        ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    only_ok(starts_at_one.map_event(&done_item(0, 1, &first)))?;
    assert_protocol_error(
        &starts_at_one.map_event(&codex_completed(1, None)),
        &ResponseReconciliationError::NonContiguousCompletedItemOutput,
    );

    let mut sparse = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    only_ok(sparse.map_event(&done_item(0, 0, &first)))?;
    only_ok(sparse.map_event(&done_item(1, 2, &third)))?;
    assert_protocol_error(
        &sparse.map_event(&codex_completed(2, None)),
        &ResponseReconciliationError::NonContiguousCompletedItemOutput,
    );
    Ok(())
}

#[test]
fn codex_fallback_rejects_an_unfinished_announcement() -> TestResult {
    let mut mapper = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let announcement = message_start("msg_announced");
    only_ok(mapper.map_event(&added_item(0, 0, &announcement)))?;

    assert_protocol_error(
        &mapper.map_event(&codex_completed(1, None)),
        &ResponseReconciliationError::AnnouncementAbsentFromTerminal,
    );
    Ok(())
}

#[test]
fn codex_fallback_rejects_unresolved_delta_channel_and_actionable_state() -> TestResult {
    let mut delta = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let delta_announcement = message_start("msg_delta");
    only_ok(delta.map_event(&added_item(0, 0, &delta_announcement)))?;
    only_ok(delta.map_event(&item_event(
        "response.output_text.delta",
        1,
        "msg_delta",
        0,
        &json!({"content_index": 0, "delta": "partial", "logprobs": []}),
    )?))?;
    assert_protocol_error(
        &delta.map_event(&codex_completed(2, None)),
        &ResponseReconciliationError::CoreDeltaAbsentFromTerminal,
    );

    let mut channel = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let channel_announcement = message_start("msg_channel");
    only_ok(channel.map_event(&added_item(0, 0, &channel_announcement)))?;
    only_ok(channel.map_event(&item_event(
        "response.output_text.done",
        1,
        "msg_channel",
        0,
        &json!({"content_index": 0, "text": "complete"}),
    )?))?;
    assert_protocol_error(
        &channel.map_event(&codex_completed(2, None)),
        &ResponseReconciliationError::ChannelCompletionAbsentFromTerminal,
    );

    let mut actionable = ResponsesMapper::for_catalog_backend(CATALOG_BACKEND_CODEX_SUBSCRIPTION);
    let actionable_announcement = function_call_start("fc_unresolved");
    only_ok(actionable.map_event(&added_item(0, 0, &actionable_announcement)))?;
    let terminal = actionable.map_event(&codex_completed(1, None));
    assert!(matches!(
        terminal.last(),
        Some(Err(ProviderError::ResponseProtocolViolation {
            source: ResponseReconciliationError::UnresolvedActionableItem { .. },
        }))
    ));
    assert!(
        !terminal
            .iter()
            .any(|event| matches!(event, Ok(ProviderEvent::Done { .. })))
    );
    Ok(())
}
