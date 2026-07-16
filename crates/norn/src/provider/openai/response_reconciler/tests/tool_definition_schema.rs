use serde_json::{Value, json};

use super::*;
use crate::provider::openai::output_item_test_fixtures::public_output_item_inventory;

const TOOL_OUTPUT_ITEMS: [&str; 2] = ["tool_search_output", "additional_tools"];

#[test]
fn file_search_filters_accept_recursive_documented_union() -> TestResult {
    let filter = json!({
        "type": "and",
        "filters": [
            {"type": "eq", "key": "category", "value": "guide"},
            {"type": "ne", "key": "archived", "value": false},
            {"type": "gt", "key": "score", "value": 0.1},
            {"type": "lt", "key": "score", "value": 1.0},
            {
                "type": "or",
                "filters": [
                    {"type": "gte", "key": "score", "value": 0.5},
                    {"type": "lte", "key": "year", "value": 2026},
                    {"type": "in", "key": "year", "value": [2025, "current"]},
                    {"type": "nin", "key": "category", "value": ["draft", 0]}
                ]
            }
        ]
    });
    for item_type in TOOL_OUTPUT_ITEMS {
        assert_tool_accepted(item_type, file_search_tool(Some(filter.clone())))?;
        assert_tool_accepted(item_type, file_search_tool(None))?;
        assert_tool_accepted(item_type, file_search_tool(Some(Value::Null)))?;
    }
    Ok(())
}

#[test]
fn file_search_filters_reject_invalid_tags_keys_values_and_children() -> TestResult {
    for (filter, field) in [
        (
            json!({"type": "future", "key": "category", "value": "guide"}),
            "tools[].filters.type",
        ),
        (
            json!({"type": "eq", "key": false, "value": "guide"}),
            "tools[].filters.key",
        ),
        (
            json!({"type": "eq", "key": "category", "value": {"secret": true}}),
            "tools[].filters.value",
        ),
        (
            json!({"type": "in", "key": "category", "value": ["guide", false]}),
            "tools[].filters.value",
        ),
        (
            json!({"type": "and", "filters": ["not-a-filter"]}),
            "tools[].filters.filters[]",
        ),
        (
            json!({"type": "or", "filters": [{"type": "eq", "key": 7, "value": "guide"}]}),
            "tools[].filters.filters[].key",
        ),
    ] {
        for item_type in TOOL_OUTPUT_ITEMS {
            assert_tool_invalid(item_type, file_search_tool(Some(filter.clone())), field)?;
        }
    }
    Ok(())
}

#[test]
fn mcp_headers_accept_absent_null_and_string_maps_only() -> TestResult {
    for item_type in TOOL_OUTPUT_ITEMS {
        assert_tool_accepted(item_type, mcp_tool(None))?;
        assert_tool_accepted(item_type, mcp_tool(Some(Value::Null)))?;
        assert_tool_accepted(
            item_type,
            mcp_tool(Some(
                json!({"Authorization": "Bearer sentinel", "X-Mode": "test"}),
            )),
        )?;

        let error = tool_result(
            item_type,
            mcp_tool(Some(
                json!({"Authorization": {"secret": "header-sentinel"}}),
            )),
        )
        .err()
        .ok_or("expected non-string MCP header rejection")?;
        assert_eq!(
            error,
            ResponseReconciliationError::InvalidAuthoritativeItemField {
                item_type: static_item_type(item_type)?,
                field: "tools[].headers.*",
            }
        );
        assert!(!error.to_string().contains("header-sentinel"));
        assert!(!error.to_string().contains("Authorization"));
    }
    Ok(())
}

fn file_search_tool(filters: Option<Value>) -> Value {
    let mut tool = json!({"type": "file_search", "vector_store_ids": ["vs_schema"]});
    if let Some(filters) = filters {
        tool["filters"] = filters;
    }
    tool
}

fn mcp_tool(headers: Option<Value>) -> Value {
    let mut tool = json!({"type": "mcp", "server_label": "schema"});
    if let Some(headers) = headers {
        tool["headers"] = headers;
    }
    tool
}

fn assert_tool_accepted(item_type: &str, tool: Value) -> TestResult {
    assert!(matches!(
        tool_result(item_type, tool)?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

fn assert_tool_invalid(item_type: &str, tool: Value, field: &'static str) -> TestResult {
    assert_eq!(
        tool_result(item_type, tool),
        Err(ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: static_item_type(item_type)?,
            field,
        })
    );
    Ok(())
}

fn tool_result(
    item_type: &str,
    tool: Value,
) -> Result<ReconcileUpdate, ResponseReconciliationError> {
    let mut item = public_output_item_inventory("tool_schema", "tool schema")
        .into_iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .ok_or(ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "response output item",
            field: "fixture",
        })?;
    item["tools"] = Value::Array(vec![tool]);
    ResponseReconciler::new().ingest(&done(1, 0, item))
}

fn static_item_type(item_type: &str) -> Result<&'static str, &'static str> {
    match item_type {
        "tool_search_output" => Ok("tool_search_output"),
        "additional_tools" => Ok("additional_tools"),
        _ => Err("unexpected tool-bearing output item"),
    }
}
