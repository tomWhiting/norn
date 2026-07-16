use std::io;

use serde_json::json;

use super::assembly::assemble_response;
use super::tool_dispatch::{ToolResultRecord, append_tool_result};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::ToolCallCaller;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::provider::usage::Usage;
use crate::session::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn terminal_caller_reaches_live_tool_result_message() -> TestResult {
    let caller = json!({
        "type": "program",
        "caller_id": "program_live",
        "provider_extension": {"kept": true}
    });
    let item = ResponseItem::from_value(json!({
        "type": "function_call",
        "id": "fc_live",
        "call_id": "call_live",
        "name": "lookup",
        "arguments": "{}",
        "caller": caller
    }))?;
    let events = [
        ProviderEvent::ResponseItemDone {
            item: ResponseTranscriptItem {
                item,
                provenance: ResponseStreamProvenance::default(),
            },
        },
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: Some("resp_live".to_owned()),
        },
    ];
    let Some(response) = assemble_response(&events) else {
        return Err(io::Error::other("terminal response did not assemble").into());
    };
    let Some(call) = response.tool_calls.first() else {
        return Err(io::Error::other("assembled response had no tool call").into());
    };
    assert_eq!(call.caller, ToolCallCaller::Present(caller.clone()));

    let store = EventStore::new();
    let mut messages = Vec::new();
    append_tool_result(
        &store,
        &mut messages,
        ToolResultRecord {
            tool_call_id: &call.call_id,
            tool_name: &call.name,
            kind: call.kind,
            caller: call.caller.clone(),
            output: &json!({"ok": true}),
            duration_ms: 1,
            inline_char_limit: 1_024,
        },
        None,
        None,
    )
    .await?;
    let Some(message) = messages.first() else {
        return Err(io::Error::other("tool result message was not appended").into());
    };
    assert_eq!(message.tool_call_caller.value(), Some(&caller));
    Ok(())
}
