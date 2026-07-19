use super::*;

#[test]
fn bare_preview_absent_from_terminal_fails_before_done() -> TestResult {
    let mut mapper = ResponsesMapper::default();
    only_ok(mapper.map_event(&SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_orphan",
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }
        }),
    }))?;
    only_ok(mapper.map_event(&SseEvent {
        event_type: "response.output_text.delta".to_owned(),
        data: json!({
            "type": "response.output_text.delta",
            "sequence_number": 1,
            "item_id": "msg_orphan",
            "output_index": 0,
            "content_index": 0,
            "delta": "preview only",
            "logprobs": []
        }),
    }))?;

    let terminal = mapper.map_event(&completed(2, &[]));
    assert!(matches!(
        terminal.as_slice(),
        [
            Ok(ProviderEvent::ResponseStreamEvent { .. }),
            Err(ProviderError::ResponseProtocolViolation {
                source: ResponseReconciliationError::CoreDeltaAbsentFromTerminal,
            })
        ]
    ));
    assert!(!terminal.iter().any(|event| matches!(
        event,
        Ok(ProviderEvent::ResponseItemDone { .. } | ProviderEvent::Done { .. })
    )));
    Ok(())
}

#[test]
fn mapper_rejects_even_an_exact_terminal_retransmit_after_delivery() {
    let terminal = completed(0, &[]);
    let mut mapper = ResponsesMapper::default();
    assert!(matches!(
        mapper.map_event(&terminal).last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
    assert!(matches!(
        mapper.map_event(&terminal).as_slice(),
        [Err(ProviderError::ResponseProtocolViolation {
            source: ResponseReconciliationError::PostTerminalFrame,
        })]
    ));
}

#[test]
fn failed_response_remains_authoritative_over_orphan_preview() -> TestResult {
    let mut mapper = ResponsesMapper::default();
    only_ok(mapper.map_event(&SseEvent {
        event_type: "response.output_item.added".to_owned(),
        data: json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_failed",
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }
        }),
    }))?;
    only_ok(mapper.map_event(&SseEvent {
        event_type: "response.output_text.delta".to_owned(),
        data: json!({
            "type": "response.output_text.delta",
            "sequence_number": 1,
            "item_id": "msg_failed",
            "output_index": 0,
            "content_index": 0,
            "delta": "partial",
            "logprobs": []
        }),
    }))?;

    let failed = mapper.map_event(&failed(2, &[], "server_is_overloaded"));
    assert!(matches!(
        failed.as_slice(),
        [
            Ok(ProviderEvent::ResponseStreamEvent { .. }),
            Err(ProviderError::StreamError {
                transient: Some(crate::error::TransientKind::ServerError { status: 503 }),
                ..
            })
        ]
    ));
    assert!(!failed.iter().any(|event| matches!(
        event,
        Ok(ProviderEvent::ResponseItemDone { .. } | ProviderEvent::Done { .. })
    )));
    Ok(())
}
