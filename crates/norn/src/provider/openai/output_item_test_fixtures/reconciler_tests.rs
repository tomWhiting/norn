use serde_json::{Value, json};

use super::nested::nested_output_item_matrix;
use super::{
    historical_shape_matrix_items, minimal_output_item_inventory, public_output_item_inventory,
    spawn_shape_matrix_items,
};
use crate::provider::openai::response_reconciler::{
    ReconcileUpdate, ResponseReconciler, ResponseReconciliationError,
};
use crate::provider::openai::sse::SseEvent;
use crate::provider::response_item::ResponseItem;

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

fn done(sequence_number: u64, output_index: u64, item: &Value) -> SseEvent {
    event(
        "response.output_item.done",
        sequence_number,
        json!({"output_index": output_index, "item": item}),
    )
}

fn terminal(sequence_number: u64, output: &[Value]) -> SseEvent {
    event(
        "response.completed",
        sequence_number,
        json!({"response": {"output": output}}),
    )
}

fn terminal_error(raw: &Value) -> Result<ResponseReconciliationError, Box<dyn std::error::Error>> {
    ResponseReconciler::new()
        .ingest(&terminal(1, std::slice::from_ref(raw)))
        .err()
        .ok_or_else(|| "expected terminal output rejection".into())
}

fn item_named(items: &[Value], item_type: &str) -> Result<Value, Box<dyn std::error::Error>> {
    items
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .cloned()
        .ok_or_else(|| format!("fixture missing {item_type}").into())
}

#[test]
fn every_hosted_or_inert_fixture_survives_done_and_terminal_authority() -> TestResult {
    let expected = spawn_shape_matrix_items("authority", "authority text");
    assert_eq!(expected.len(), 48);
    let mut reconciler = ResponseReconciler::new();
    for (index, raw) in expected.iter().cloned().enumerate() {
        let output_index = u64::try_from(index)?;
        let update = reconciler.ingest(&done(output_index + 1, output_index, &raw))?;
        let ReconcileUpdate::CompletedItem { item, .. } = update else {
            return Err(format!("item {output_index} did not complete").into());
        };
        assert_eq!(item.item.raw(), &raw);
    }

    let sequence_number = u64::try_from(expected.len())? + 1;
    let update = reconciler.ingest(&terminal(sequence_number, &expected))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal inventory update".into());
    };
    let actual = items
        .iter()
        .map(|item| item.item.raw().clone())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn supported_function_and_custom_pairs_are_valid_completed_history() -> TestResult {
    let expected = historical_shape_matrix_items("history", "historical text");
    assert_eq!(expected.len(), 52);
    let update = ResponseReconciler::new().ingest(&terminal(1, &expected))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal historical inventory".into());
    };
    let actual = items
        .iter()
        .map(|item| item.item.raw().clone())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn unsupported_global_and_conditional_calls_fail_with_exact_raw_retention() -> TestResult {
    let inventory = public_output_item_inventory("unsupported", "unsupported text");
    let mut unsupported = [
        "computer_call",
        "local_shell_call",
        "apply_patch_call",
        "mcp_approval_request",
    ]
    .iter()
    .map(|item_type| item_named(&inventory, item_type))
    .collect::<Result<Vec<_>, _>>()?;

    let mut client_search = item_named(&inventory, "tool_search_call")?;
    let Some(client_search) = client_search.as_object_mut() else {
        return Err("tool search fixture was not an object".into());
    };
    client_search.insert("execution".to_owned(), json!("client"));
    unsupported.push(Value::Object(client_search.clone()));

    let mut local_shell = item_named(&inventory, "shell_call")?;
    let Some(local_shell) = local_shell.as_object_mut() else {
        return Err("shell fixture was not an object".into());
    };
    local_shell.insert("environment".to_owned(), json!({"type": "local"}));
    unsupported.push(Value::Object(local_shell.clone()));

    unsupported.push(item_named(
        &minimal_output_item_inventory("unsupported_minimal"),
        "shell_call",
    )?);

    assert_eq!(unsupported.len(), 7);
    for raw in unsupported {
        let error = terminal_error(&raw)?;
        assert!(matches!(
            error,
            ResponseReconciliationError::UnsupportedExecutableItem { .. }
        ));
        assert_eq!(error.retained_items().len(), 1);
        assert_eq!(error.retained_items()[0].item.raw(), &raw);
    }
    Ok(())
}

#[test]
fn every_nested_union_variant_reaches_parser_and_reconciler() -> TestResult {
    let matrix = nested_output_item_matrix("reconcile");
    for raw in matrix {
        let parsed = ResponseItem::from_value(raw.clone())?;
        assert_eq!(parsed.raw(), &raw);

        match parsed.item_type() {
            "computer_call" | "apply_patch_call" => {
                let error = terminal_error(&raw)?;
                assert!(matches!(
                    error,
                    ResponseReconciliationError::UnsupportedExecutableItem { .. }
                ));
                assert_eq!(error.retained_items()[0].item.raw(), &raw);
            }
            "web_search_call" | "shell_call_output" => {
                let update =
                    ResponseReconciler::new().ingest(&terminal(1, std::slice::from_ref(&raw)))?;
                let ReconcileUpdate::Terminal { items, .. } = update else {
                    return Err("expected terminal nested-union update".into());
                };
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item.raw(), &raw);
            }
            other => return Err(format!("unexpected nested item type {other}").into()),
        }
    }
    Ok(())
}
