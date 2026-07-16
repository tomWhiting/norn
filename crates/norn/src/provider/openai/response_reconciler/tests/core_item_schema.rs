use serde_json::{Map, Value, json};

use super::*;
use crate::provider::openai::output_item_test_fixtures::public_output_item_inventory;

#[test]
fn canonical_typed_core_nested_data_passes_done_and_terminal_authority() -> TestResult {
    let items = ["message", "reasoning", "web_search_call"]
        .into_iter()
        .map(item_fixture)
        .collect::<Result<Vec<_>, _>>()?;
    let mut done_reconciler = ResponseReconciler::new();
    for (index, item) in items.iter().cloned().enumerate() {
        let index = u64::try_from(index)?;
        assert!(matches!(
            done_reconciler.ingest(&done(index + 1, index, item))?,
            ReconcileUpdate::CompletedItem { .. }
        ));
    }
    let terminal_sequence = u64::try_from(items.len())? + 1;
    assert!(matches!(
        done_reconciler.ingest(&event(
            "response.completed",
            terminal_sequence,
            json!({"response": {"output": items}}),
        ))?,
        ReconcileUpdate::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn every_documented_annotation_variant_requires_its_schema_fields() -> TestResult {
    assert_message_missing(
        |message| remove(annotation(message, 0)?, "filename"),
        "content[].annotations[].filename",
    )?;
    assert_message_invalid(
        |message| replace(annotation(message, 1)?, "start_index", json!(0.5)),
        "content[].annotations[].start_index",
    )?;
    assert_message_missing(
        |message| remove(annotation(message, 2)?, "container_id"),
        "content[].annotations[].container_id",
    )?;
    assert_message_invalid(
        |message| replace(annotation(message, 3)?, "index", json!("one")),
        "content[].annotations[].index",
    )?;
    assert_message_invalid(
        |message| replace(annotation(message, 0)?, "type", json!("future_citation")),
        "content[].annotations[].type",
    )?;
    Ok(())
}

#[test]
fn logprob_members_and_byte_arrays_are_required_and_typed() -> TestResult {
    assert_message_missing(
        |message| remove(logprob(message)?, "bytes"),
        "content[].logprobs[].bytes",
    )?;
    let mut fractional = item_fixture("message")?;
    replace(logprob(&mut fractional)?, "bytes", json!([65, 1.5]))?;
    replace(top_logprob(&mut fractional)?, "bytes", json!([66, 2.5]))?;
    let mut reconciler = ResponseReconciler::new();
    assert!(matches!(
        reconciler.ingest(&done(1, 0, fractional))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    assert_message_invalid(
        |message| replace(logprob(message)?, "bytes", json!([{"not": "a number"}])),
        "content[].logprobs[].bytes[]",
    )?;
    assert_message_invalid(
        |message| replace(logprob(message)?, "logprob", json!("unlikely")),
        "content[].logprobs[].logprob",
    )?;
    assert_message_missing(
        |message| remove(top_logprob(message)?, "token"),
        "content[].logprobs[].top_logprobs[].token",
    )?;
    assert_message_invalid(
        |message| replace(top_logprob(message)?, "bytes", json!(["not-a-byte"])),
        "content[].logprobs[].top_logprobs[].bytes[]",
    )?;
    assert_message_missing(
        |message| remove(logprob(message)?, "top_logprobs"),
        "content[].logprobs[].top_logprobs",
    )?;
    Ok(())
}

#[test]
fn reasoning_summary_and_content_parts_are_closed_to_the_documented_tags() -> TestResult {
    assert_reasoning_invalid(
        |reasoning| {
            replace(
                part(reasoning, "/summary/0")?,
                "type",
                json!("reasoning_text"),
            )
        },
        "summary[].type",
    )?;
    assert_reasoning_missing(
        |reasoning| remove(part(reasoning, "/summary/0")?, "text"),
        "summary[].text",
    )?;
    assert_reasoning_invalid(
        |reasoning| {
            replace(
                part(reasoning, "/content/0")?,
                "type",
                json!("summary_text"),
            )
        },
        "content[].type",
    )?;
    assert_reasoning_missing(
        |reasoning| remove(part(reasoning, "/content/0")?, "text"),
        "content[].text",
    )?;
    Ok(())
}

#[test]
fn function_and_custom_callers_accept_direct_or_identified_programs_only() -> TestResult {
    for item_type in ["function_call", "custom_tool_call"] {
        for caller in [
            json!({"type": "direct"}),
            json!({"type": "program", "caller_id": "program_1"}),
            Value::Null,
        ] {
            let mut item = item_fixture(item_type)?;
            item["caller"] = caller;
            assert!(matches!(
                ResponseReconciler::new().ingest(&done(1, 0, item))?,
                ReconcileUpdate::CompletedItem { .. }
            ));
        }

        let mut missing_id = item_fixture(item_type)?;
        missing_id["caller"] = json!({"type": "program"});
        assert_error(missing_id, item_type, "caller.caller_id", false);

        let mut unknown = item_fixture(item_type)?;
        unknown["caller"] = json!({"type": "delegate", "caller_id": "private"});
        assert_error(unknown, item_type, "caller.type", true);
    }
    Ok(())
}

#[test]
fn terminal_function_and_custom_program_callers_are_retained() -> TestResult {
    let mut function = item_fixture("function_call")?;
    function["caller"] = json!({"type": "program", "caller_id": "program_function"});
    let mut custom = item_fixture("custom_tool_call")?;
    custom["caller"] = json!({"type": "program", "caller_id": "program_custom"});
    assert!(matches!(
        ResponseReconciler::new().ingest(&event(
            "response.completed",
            1,
            json!({"response": {"output": [function, custom]}}),
        ))?,
        ReconcileUpdate::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn web_search_actions_accept_all_documented_shapes_without_cross_field_invention() -> TestResult {
    for action in [
        json!({"type": "search"}),
        json!({
            "type": "search",
            "query": "query",
            "queries": ["query", "fallback"],
            "sources": [{"type": "url", "url": "https://example.test"}]
        }),
        json!({"type": "open_page"}),
        json!({"type": "open_page", "url": null}),
        json!({"type": "find_in_page", "pattern": "needle", "url": "https://example.test"}),
    ] {
        let mut item = item_fixture("web_search_call")?;
        item["action"] = action;
        assert!(matches!(
            ResponseReconciler::new().ingest(&done(1, 0, item))?,
            ReconcileUpdate::CompletedItem { .. }
        ));
    }
    Ok(())
}

#[test]
fn malformed_web_search_actions_fail_with_pinned_diagnostics() -> TestResult {
    assert_web_invalid(
        |action| replace(action, "type", json!("browse")),
        "action.type",
    )?;
    assert_web_invalid(
        |action| replace(action, "queries", json!(["valid", 7])),
        "action.queries[]",
    )?;
    assert_web_invalid(
        |action| {
            replace(
                action,
                "sources",
                json!([{"type": "feed", "url": "private"}]),
            )
        },
        "action.sources[].type",
    )?;
    assert_web_missing(
        |action| {
            action.insert(
                "sources".to_owned(),
                json!([{"type": "url", "url": "private"}]),
            );
            let source = action
                .get_mut("sources")
                .and_then(Value::as_array_mut)
                .and_then(|sources| sources.first_mut())
                .and_then(Value::as_object_mut)
                .ok_or("web source fixture was not an object")?;
            source.remove("url");
            Ok(())
        },
        "action.sources[].url",
    )?;
    assert_web_invalid(
        |action| {
            *action = json!({"type": "open_page", "url": 7})
                .as_object()
                .cloned()
                .ok_or("open_page fixture was not an object")?;
            Ok(())
        },
        "action.url",
    )?;
    assert_web_missing(
        |action| {
            *action = json!({"type": "find_in_page", "url": "https://example.test"})
                .as_object()
                .cloned()
                .ok_or("find fixture was not an object")?;
            Ok(())
        },
        "action.pattern",
    )?;
    Ok(())
}

#[test]
fn terminal_core_schema_errors_do_not_render_provider_values() -> TestResult {
    const SENTINEL: &str = "core-schema-secret-sentinel";
    let mut item = item_fixture("web_search_call")?;
    item["action"] = json!({"type": SENTINEL});
    let terminal = event(
        "response.completed",
        1,
        json!({"response": {"output": [item]}}),
    );
    let error = ResponseReconciler::new()
        .ingest(&terminal)
        .err()
        .ok_or("expected terminal core schema rejection")?;
    assert_eq!(
        error,
        ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "web_search_call",
            field: "action.type",
        }
    );
    assert!(!error.to_string().contains(SENTINEL));
    Ok(())
}

fn item_fixture(item_type: &str) -> Result<Value, &'static str> {
    public_output_item_inventory("core_schema", "core schema validation")
        .into_iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .ok_or("missing typed-core fixture")
}

fn part<'a>(
    item: &'a mut Value,
    pointer: &str,
) -> Result<&'a mut Map<String, Value>, &'static str> {
    item.pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .ok_or("typed-core part fixture was not an object")
}

fn annotation(message: &mut Value, index: usize) -> Result<&mut Map<String, Value>, &'static str> {
    message
        .pointer_mut("/content/0/annotations")
        .and_then(Value::as_array_mut)
        .and_then(|annotations| annotations.get_mut(index))
        .and_then(Value::as_object_mut)
        .ok_or("annotation fixture was not an object")
}

fn logprob(message: &mut Value) -> Result<&mut Map<String, Value>, &'static str> {
    part(message, "/content/0/logprobs/0")
}

fn top_logprob(message: &mut Value) -> Result<&mut Map<String, Value>, &'static str> {
    part(message, "/content/0/logprobs/0/top_logprobs/0")
}

fn replace(object: &mut Map<String, Value>, field: &str, value: Value) -> Result<(), &'static str> {
    object
        .insert(field.to_owned(), value)
        .map(|_| ())
        .ok_or("typed-core fixture field was absent")
}

fn remove(object: &mut Map<String, Value>, field: &str) -> Result<(), &'static str> {
    object
        .remove(field)
        .map(|_| ())
        .ok_or("typed-core fixture field was absent")
}

fn assert_message_missing(
    mutate: impl FnOnce(&mut Value) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_core_error("message", mutate, field, false)
}

fn assert_message_invalid(
    mutate: impl FnOnce(&mut Value) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_core_error("message", mutate, field, true)
}

fn assert_reasoning_missing(
    mutate: impl FnOnce(&mut Value) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_core_error("reasoning", mutate, field, false)
}

fn assert_reasoning_invalid(
    mutate: impl FnOnce(&mut Value) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_core_error("reasoning", mutate, field, true)
}

fn assert_web_missing(
    mutate: impl FnOnce(&mut Map<String, Value>) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_web_error(mutate, field, false)
}

fn assert_web_invalid(
    mutate: impl FnOnce(&mut Map<String, Value>) -> Result<(), &'static str>,
    field: &'static str,
) -> TestResult {
    assert_web_error(mutate, field, true)
}

fn assert_web_error(
    mutate: impl FnOnce(&mut Map<String, Value>) -> Result<(), &'static str>,
    field: &'static str,
    invalid: bool,
) -> TestResult {
    let mut item = item_fixture("web_search_call")?;
    let action = item
        .get_mut("action")
        .and_then(Value::as_object_mut)
        .ok_or("web action fixture was not an object")?;
    mutate(action)?;
    assert_error(item, "web_search_call", field, invalid);
    Ok(())
}

fn assert_core_error(
    item_type: &'static str,
    mutate: impl FnOnce(&mut Value) -> Result<(), &'static str>,
    field: &'static str,
    invalid: bool,
) -> TestResult {
    let mut item = item_fixture(item_type)?;
    mutate(&mut item)?;
    assert_error(item, item_type, field, invalid);
    Ok(())
}

fn assert_error(item: Value, item_type: &'static str, field: &'static str, invalid: bool) {
    let error = ResponseReconciler::new().ingest(&done(1, 0, item));
    let expected = if invalid {
        ResponseReconciliationError::InvalidAuthoritativeItemField { item_type, field }
    } else {
        ResponseReconciliationError::MissingAuthoritativeItemField { item_type, field }
    };
    assert_eq!(error, Err(expected));
}
