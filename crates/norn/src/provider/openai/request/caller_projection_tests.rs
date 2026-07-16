use std::io;

use super::*;
use crate::provider::request::{AssistantToolCall, ToolCallCaller};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn threaded_result_keeps_caller_without_originating_message() -> TestResult {
    let caller = serde_json::json!({"type": "program", "caller_id": "program_threaded"});
    let payload = build_payload(
        &request(
            vec![tool_result(
                "call_threaded",
                ToolCallKind::Function,
                ToolCallCaller::Present(caller.clone()),
            )],
            Some("resp_previous"),
        ),
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
    )?;
    let input = request_input(&payload)?;
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["caller"], caller);
    assert_eq!(payload["previous_response_id"], "resp_previous");
    Ok(())
}

#[test]
fn canonical_output_consumes_earlier_reused_call_caller() -> TestResult {
    let assistant = canonical_assistant(vec![
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "old",
            "arguments": "{}",
            "caller": {"type": "program", "caller_id": "program_old"}
        }),
        serde_json::json!({
            "type": "function_call_output",
            "id": "fco_old",
            "call_id": "call_reused",
            "output": "already resolved",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_reused",
            "name": "new",
            "arguments": "{}",
            "caller": {"type": "program", "caller_id": "program_new"}
        }),
    ])?;
    let payload = build_payload(
        &request(
            vec![
                assistant,
                tool_result(
                    "call_reused",
                    ToolCallKind::Function,
                    ToolCallCaller::Absent,
                ),
            ],
            None,
        ),
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
    )?;
    let input = request_input(&payload)?;
    let Some(output) = input.last() else {
        return Err(io::Error::other("request input was empty").into());
    };
    assert_eq!(output["caller"]["caller_id"], "program_new");
    Ok(())
}

#[test]
fn provider_neutral_fallback_call_and_result_share_caller() -> TestResult {
    let caller = ToolCallCaller::Present(
        serde_json::json!({"type": "program", "caller_id": "program_fallback"}),
    );
    let assistant = Message {
        response_items: Vec::new(),
        role: MessageRole::Assistant,
        content: None,
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![AssistantToolCall {
            call_id: "call_fallback".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
            kind: ToolCallKind::Function,
            caller,
        }],
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: ToolCallCaller::Absent,
    };
    let payload = build_payload(
        &request(
            vec![
                assistant,
                tool_result(
                    "call_fallback",
                    ToolCallKind::Function,
                    ToolCallCaller::Absent,
                ),
            ],
            None,
        ),
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
    )?;
    let input = request_input(&payload)?;
    assert_eq!(input[0]["caller"], input[1]["caller"]);
    assert_eq!(input[1]["caller"]["caller_id"], "program_fallback");
    Ok(())
}

#[test]
fn malformed_or_conflicting_caller_fails_without_disclosure() -> TestResult {
    let malformed = "sentinel-malformed-caller";
    let malformed_error = build_payload(
        &request(
            vec![tool_result(
                "call_bad",
                ToolCallKind::Function,
                ToolCallCaller::Present(serde_json::Value::String(malformed.to_owned())),
            )],
            Some("resp_previous"),
        ),
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
    );
    let Err(malformed_error) = malformed_error else {
        return Err(io::Error::other("malformed caller was accepted").into());
    };
    assert!(!malformed_error.to_string().contains(malformed));

    let assistant = canonical_assistant(vec![serde_json::json!({
        "type": "function_call",
        "call_id": "call_conflict",
        "name": "lookup",
        "arguments": "{}",
        "caller": {"type": "program", "caller_id": "program_authority"}
    })])?;
    let conflict = build_payload(
        &request(
            vec![
                assistant,
                tool_result(
                    "call_conflict",
                    ToolCallKind::Function,
                    ToolCallCaller::Present(serde_json::json!({
                        "type": "program",
                        "caller_id": "sentinel-conflicting-program"
                    })),
                ),
            ],
            None,
        ),
        CATALOG_BACKEND_CODEX_SUBSCRIPTION,
    );
    let Err(conflict) = conflict else {
        return Err(io::Error::other("conflicting caller was accepted").into());
    };
    assert!(
        !conflict
            .to_string()
            .contains("sentinel-conflicting-program")
    );
    Ok(())
}

fn canonical_assistant(raw_items: Vec<serde_json::Value>) -> Result<Message, ProviderError> {
    let response_items = raw_items
        .into_iter()
        .map(|raw| {
            ResponseItem::from_value(raw)
                .map(|item| ResponseTranscriptItem {
                    item,
                    provenance: ResponseStreamProvenance::default(),
                })
                .map_err(|error| ProviderError::ResponseParseError {
                    reason: error.to_string(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Message {
        response_items,
        role: MessageRole::Assistant,
        content: None,
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: ToolCallCaller::Absent,
    })
}

fn tool_result(call_id: &str, kind: ToolCallKind, caller: ToolCallCaller) -> Message {
    Message {
        response_items: Vec::new(),
        role: MessageRole::ToolResult,
        content: Some("result".to_owned()),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(call_id.to_owned()),
        tool_name: Some("fixture".to_owned()),
        tool_call_kind: Some(kind),
        tool_call_caller: caller,
    }
}

fn request(messages: Vec<Message>, previous_response_id: Option<&str>) -> ProviderRequest {
    ProviderRequest {
        messages,
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: previous_response_id.map(str::to_owned),
        store: false,
        context_management: None,
    }
}

fn request_input(payload: &serde_json::Value) -> Result<&[serde_json::Value], io::Error> {
    payload
        .get("input")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| io::Error::other("request input was not an array"))
}
