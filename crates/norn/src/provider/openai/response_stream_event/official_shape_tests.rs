use std::error::Error;

use serde_json::{Value, json};

use super::{ResponseStreamEvent, ResponseStreamEventManifest, ResponseStreamSequencePolicy};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn reasoning_summary_part_done_retains_optional_status_shapes() -> TestResult {
    for (raw, expected_status) in [
        (
            json!({
                "type": "response.reasoning_summary_part.done",
                "sequence_number": 7,
                "item_id": "rs_1",
                "output_index": 0,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": "summary"},
            }),
            None,
        ),
        (
            json!({
                "type": "response.reasoning_summary_part.done",
                "sequence_number": 7,
                "item_id": "rs_1",
                "output_index": 0,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": "summary"},
                "status": "incomplete",
            }),
            Some("incomplete"),
        ),
    ] {
        assert_eq!(raw.get("status").and_then(Value::as_str), expected_status);
        assert_eq!(raw.get("status").is_some(), expected_status.is_some());

        let event =
            ResponseStreamEvent::from_sse("response.reasoning_summary_part.done", raw.clone())?;
        assert!(matches!(
            event.manifest(),
            ResponseStreamEventManifest::Public(_)
        ));
        assert_eq!(event.sequence_number(), Some(7));
        assert_eq!(event.raw(), &raw);
        assert_eq!(serde_json::to_value(&event)?, raw);
    }
    Ok(())
}

#[test]
fn public_error_event_retains_required_nullable_code_and_param() -> TestResult {
    for (raw, expected_code, expected_param) in [
        (
            json!({
                "type": "error",
                "sequence_number": 11,
                "code": null,
                "message": "request failed",
                "param": null,
            }),
            None,
            None,
        ),
        (
            json!({
                "type": "error",
                "sequence_number": 12,
                "code": "invalid_request_error",
                "message": "request failed",
                "param": "input",
            }),
            Some("invalid_request_error"),
            Some("input"),
        ),
    ] {
        assert!(raw.get("code").is_some());
        assert!(raw.get("param").is_some());
        assert_eq!(raw.get("code").and_then(Value::as_str), expected_code);
        assert_eq!(raw.get("param").and_then(Value::as_str), expected_param);
        assert_eq!(raw["code"].is_null(), expected_code.is_none());
        assert_eq!(raw["param"].is_null(), expected_param.is_none());

        let event = ResponseStreamEvent::from_sse("error", raw.clone())?;
        assert!(matches!(
            event.manifest(),
            ResponseStreamEventManifest::Public(_)
        ));
        assert_eq!(
            event.manifest().sequence_policy(),
            ResponseStreamSequencePolicy::Required
        );
        assert_eq!(event.raw(), &raw);
        assert_eq!(serde_json::to_value(&event)?, raw);
    }
    Ok(())
}
