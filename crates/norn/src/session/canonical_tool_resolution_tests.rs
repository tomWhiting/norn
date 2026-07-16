use serde_json::{Value, json};

use crate::provider::openai::output_item_test_fixtures::response_items_named;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;
use crate::session::{repair_dangling_tool_calls, unresolved_local_tool_calls};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn canonical_assistant(
    suffix: &str,
    names: &[&str],
) -> Result<SessionEvent, Box<dyn std::error::Error>> {
    let response_items = response_items_named(suffix, names)?;
    if response_items.len() != names.len() {
        return Err("canonical fixture selection was incomplete".into());
    }
    Ok(assistant_with_items(response_items))
}

fn assistant_from_raw(raws: Vec<Value>) -> Result<SessionEvent, Box<dyn std::error::Error>> {
    let response_items = raws
        .into_iter()
        .map(|raw| {
            Ok(ResponseTranscriptItem {
                item: ResponseItem::from_value(raw)?,
                provenance: ResponseStreamProvenance::default(),
            })
        })
        .collect::<Result<Vec<_>, crate::provider::ResponseItemError>>()?;
    Ok(assistant_with_items(response_items))
}

fn assistant_with_items(response_items: Vec<ResponseTranscriptItem>) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    }
}

fn function_call(call_id: &str) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": "function",
        "arguments": "{}"
    })
}

fn function_output(call_id: &str) -> Value {
    json!({
        "type": "function_call_output",
        "id": "fco",
        "call_id": call_id,
        "output": "ok",
        "status": "completed"
    })
}

fn custom_call(call_id: &str) -> Value {
    json!({
        "type": "custom_tool_call",
        "call_id": call_id,
        "name": "custom",
        "input": "input"
    })
}

fn custom_output(call_id: &str) -> Value {
    json!({
        "type": "custom_tool_call_output",
        "id": "ctco",
        "call_id": call_id,
        "output": "ok",
        "status": "completed"
    })
}

fn legacy_result(call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: call_id.to_owned(),
        tool_name: "legacy".to_owned(),
        output: json!({"ok": true}),
        spool_ref: None,
        duration_ms: 0,
    }
}

#[test]
fn canonical_function_and_custom_outputs_leave_no_unresolved_local_calls() -> TestResult {
    let event = canonical_assistant(
        "projection",
        &[
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
        ],
    )?;
    assert!(unresolved_local_tool_calls(&[event]).is_empty());
    Ok(())
}

#[test]
fn ordered_projection_rejects_mismatch_precedence_and_id_reuse_shortcuts() -> TestResult {
    let mismatched = assistant_from_raw(vec![
        function_call("shared"),
        custom_output("shared"),
        custom_call("other"),
        function_output("other"),
    ])?;
    let pending = unresolved_local_tool_calls(&[mismatched]);
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].kind, ToolCallKind::Function);
    assert_eq!(pending[1].kind, ToolCallKind::Custom);

    let output_before_call =
        assistant_from_raw(vec![function_output("before"), function_call("before")])?;
    let pending = unresolved_local_tool_calls(&[output_before_call]);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, "before");

    let reused = assistant_from_raw(vec![
        function_call("reused"),
        function_call("reused"),
        function_output("reused"),
    ])?;
    let pending = unresolved_local_tool_calls(&[reused]);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, "reused");
    Ok(())
}

#[test]
fn legacy_results_resolve_only_calls_that_precede_them() -> TestResult {
    let call_after_result = assistant_from_raw(vec![function_call("ordered")])?;
    let pending = unresolved_local_tool_calls(&[legacy_result("ordered"), call_after_result]);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, "ordered");

    let call_before_result = assistant_from_raw(vec![custom_call("resolved")])?;
    assert!(
        unresolved_local_tool_calls(&[call_before_result, legacy_result("resolved")]).is_empty()
    );
    Ok(())
}

#[test]
fn canonical_projection_preserves_caller_presence_and_exact_value() -> TestResult {
    let program_caller = json!({
        "type": "program",
        "caller_id": "program_projection",
        "provider_extension": {"kept": true}
    });
    let event = assistant_from_raw(vec![
        json!({
            "type": "function_call",
            "call_id": "program",
            "name": "function",
            "arguments": "{}",
            "caller": program_caller
        }),
        json!({
            "type": "custom_tool_call",
            "call_id": "null",
            "name": "custom",
            "input": "input",
            "caller": null
        }),
        function_call("absent"),
    ])?;
    let pending = unresolved_local_tool_calls(&[event]);
    assert_eq!(pending.len(), 3);
    assert_eq!(pending[0].caller.value(), Some(&program_caller));
    assert_eq!(pending[1].caller, ToolCallCaller::Present(Value::Null));
    assert_eq!(pending[2].caller, ToolCallCaller::Absent);
    Ok(())
}

#[test]
fn resume_repair_does_not_duplicate_canonical_function_or_custom_outputs() -> TestResult {
    for (suffix, names) in [
        ("repair_function", ["function_call", "function_call_output"]),
        (
            "repair_custom",
            ["custom_tool_call", "custom_tool_call_output"],
        ),
    ] {
        let store = EventStore::new();
        store.append(canonical_assistant(suffix, &names)?)?;
        let repaired = repair_dangling_tool_calls(&store)?;
        assert!(repaired.is_empty());
        assert_eq!(store.events().len(), 1);
    }
    Ok(())
}
