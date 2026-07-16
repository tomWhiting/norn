use serde_json::{Value, json};

use super::*;
use crate::provider::openai::output_item_test_fixtures::{
    public_output_item_inventory, public_tool_definitions,
};

mod client_calls;

const ACCEPTED_KNOWN_ITEMS: &[&str] = &[
    "file_search_call",
    "function_call_output",
    "computer_call_output",
    "program",
    "program_output",
    "tool_search_call",
    "tool_search_output",
    "additional_tools",
    "image_generation_call",
    "code_interpreter_call",
    "local_shell_call_output",
    "shell_call",
    "shell_call_output",
    "apply_patch_call_output",
    "mcp_call",
    "mcp_list_tools",
    "mcp_approval_response",
    "custom_tool_call_output",
];

#[test]
fn every_public_output_item_has_an_explicit_authoritative_validator() -> TestResult {
    let items = public_output_item_inventory("validator", "validator coverage");
    assert_eq!(items.len(), 28);
    for item in items {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or("public output fixture was missing its discriminator")?;
        if !ResponseReconciler::has_authoritative_item_validator(item_type) {
            return Err(format!("missing authoritative validator for {item_type}").into());
        }
    }
    assert!(!ResponseReconciler::has_authoritative_item_validator(
        "future_output_item"
    ));
    Ok(())
}

#[test]
fn every_accepted_known_item_passes_authoritative_schema_validation() -> TestResult {
    let items = public_output_item_inventory("schema", "schema validation");
    let accepted = items
        .into_iter()
        .filter(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .is_some_and(|item_type| ACCEPTED_KNOWN_ITEMS.contains(&item_type))
        })
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), ACCEPTED_KNOWN_ITEMS.len());
    for (output_index, item) in accepted.into_iter().enumerate() {
        let output_index = u64::try_from(output_index)?;
        let mut reconciler = ResponseReconciler::new();
        let update = reconciler.ingest(&done(1, output_index, item))?;
        if !matches!(update, ReconcileUpdate::CompletedItem { .. }) {
            return Err("expected completed known output item".into());
        }
    }
    Ok(())
}

#[test]
fn missing_required_fields_fail_before_canonical_retention() -> TestResult {
    for (item_type, field) in [
        ("computer_call", "pending_safety_checks"),
        ("function_call_output", "call_id"),
        ("computer_call_output", "output"),
        ("program", "fingerprint"),
        ("program_output", "result"),
        ("tool_search_call", "arguments"),
        ("tool_search_output", "tools"),
        ("additional_tools", "role"),
        ("local_shell_call_output", "output"),
        ("local_shell_call", "action"),
        ("shell_call", "action"),
        ("shell_call_output", "max_output_length"),
        ("apply_patch_call_output", "status"),
        ("apply_patch_call", "operation"),
        ("mcp_approval_request", "arguments"),
        ("mcp_approval_response", "approve"),
        ("custom_tool_call_output", "output"),
    ] {
        assert_removed_field(item_type, field)?;
    }
    Ok(())
}

#[test]
fn top_level_enums_nullability_and_types_are_exact() -> TestResult {
    for (item_type, field, replacement) in [
        ("computer_call", "status", json!("failed")),
        ("function_call_output", "status", json!("failed")),
        ("computer_call_output", "call_id", Value::Null),
        ("program", "code", json!({})),
        ("program_output", "status", json!("failed")),
        ("tool_search_call", "call_id", json!(7)),
        ("tool_search_output", "execution", json!("remote")),
        ("additional_tools", "role", json!("owner")),
        ("local_shell_call_output", "status", json!("failed")),
        ("local_shell_call", "call_id", Value::Null),
        ("shell_call", "status", json!("failed")),
        ("shell_call_output", "max_output_length", json!(1.5)),
        ("apply_patch_call_output", "output", json!({})),
        ("apply_patch_call", "status", json!("incomplete")),
        ("mcp_approval_request", "server_label", json!(7)),
        ("mcp_approval_response", "reason", json!(false)),
        ("custom_tool_call_output", "status", Value::Null),
    ] {
        assert_replaced_field(item_type, field, replacement)?;
    }
    Ok(())
}

#[test]
fn nested_tagged_unions_reject_unknown_or_incomplete_members() -> TestResult {
    assert_nested_error(
        "function_call_output",
        |item| item["output"] = json!([{"type": "input_audio"}]),
        "output[].type",
    )?;
    assert_nested_error(
        "computer_call_output",
        |item| item["output"]["type"] = json!("image"),
        "output.type",
    )?;
    assert_nested_error(
        "tool_search_output",
        |item| item["tools"] = json!([{"type": "future_tool"}]),
        "tools[].type",
    )?;
    assert_nested_missing_error(
        "additional_tools",
        |item| {
            item["tools"] =
                json!([{"type": "namespace", "name": "missing-description", "tools": []}]);
        },
        "tools[].description",
    )?;
    assert_nested_missing_error(
        "shell_call",
        |item| item["environment"] = json!({"type": "container_reference"}),
        "environment.container_id",
    )?;
    assert_nested_missing_error(
        "shell_call_output",
        |item| item["output"][0]["outcome"] = json!({"type": "exit"}),
        "output[].outcome.exit_code",
    )?;
    assert_nested_missing_error(
        "custom_tool_call_output",
        |item| item["caller"] = json!({"type": "program"}),
        "caller.caller_id",
    )?;
    Ok(())
}

#[test]
fn hosted_nested_outputs_and_mcp_tools_are_structurally_validated() -> TestResult {
    assert_nested_missing_error(
        "code_interpreter_call",
        |item| item["outputs"] = json!([{"type": "image"}]),
        "outputs[].url",
    )?;
    assert_nested_missing_error(
        "mcp_list_tools",
        |item| item["tools"] = json!([{"name": "missing-schema"}]),
        "tools[].input_schema",
    )?;
    assert_nested_error(
        "file_search_call",
        |item| item["queries"] = json!(["ok", 7]),
        "queries[]",
    )?;
    Ok(())
}

#[test]
fn file_search_result_attributes_reject_non_scalar_values_without_disclosure() -> TestResult {
    const SENTINEL: &str = "private-file-search-attribute";
    for invalid in [Value::Null, json!([SENTINEL]), json!({"secret": SENTINEL})] {
        let mut item = item_fixture("file_search_call")?;
        item["results"][0]["attributes"]["provider_key"] = invalid;
        let error = ResponseReconciler::new()
            .ingest(&done(1, 0, item))
            .err()
            .ok_or("expected invalid file-search attribute value")?;
        assert_eq!(
            error,
            ResponseReconciliationError::InvalidAuthoritativeItemField {
                item_type: "file_search_call",
                field: "results[].attributes.*",
            }
        );
        assert!(!error.to_string().contains(SENTINEL));
        assert!(!error.to_string().contains("provider_key"));
    }
    Ok(())
}

#[test]
fn tool_definition_union_accepts_all_documented_discriminators() -> TestResult {
    let definitions = public_tool_definitions();
    assert_eq!(definitions.len(), 18);
    let item = json!({
        "id": "at_all",
        "type": "additional_tools",
        "role": "developer",
        "tools": definitions
    });
    let mut reconciler = ResponseReconciler::new();
    assert!(matches!(
        reconciler.ingest(&done(1, 0, item))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

#[test]
fn code_interpreter_auto_container_network_policy_is_fully_validated() -> TestResult {
    let valid_tool = json!({
        "type": "code_interpreter",
        "container": {
            "type": "auto",
            "file_ids": ["file_schema"],
            "memory_limit": null,
            "network_policy": {
                "type": "allowlist",
                "allowed_domains": ["example.test"],
                "domain_secrets": [{
                    "domain": "example.test",
                    "name": "TOKEN",
                    "value": "provider-secret-sentinel"
                }]
            }
        }
    });
    let mut valid = item_fixture("additional_tools")?;
    valid["tools"] = json!([valid_tool]);
    assert!(matches!(
        ResponseReconciler::new().ingest(&done(1, 0, valid))?,
        ReconcileUpdate::CompletedItem { .. }
    ));

    assert_nested_error(
        "additional_tools",
        |item| {
            item["tools"] = json!([{
                "type": "code_interpreter",
                "container": {
                    "type": "auto",
                    "network_policy": {"type": "future_policy"}
                }
            }]);
        },
        "tools[].container.network_policy.type",
    )?;
    assert_nested_missing_error(
        "additional_tools",
        |item| {
            item["tools"] = json!([{
                "type": "code_interpreter",
                "container": {
                    "type": "auto",
                    "network_policy": {"type": "allowlist"}
                }
            }]);
        },
        "tools[].container.network_policy.allowed_domains",
    )?;
    assert_nested_error(
        "additional_tools",
        |item| {
            item["tools"] = json!([{
                "type": "code_interpreter",
                "container": {
                    "type": "auto",
                    "network_policy": {
                        "type": "allowlist",
                        "allowed_domains": ["example.test"],
                        "domain_secrets": [{
                            "domain": "example.test",
                            "name": "TOKEN",
                            "value": false
                        }]
                    }
                }
            }]);
        },
        "tools[].container.network_policy.domain_secrets[].value",
    )?;
    Ok(())
}

#[test]
fn schema_errors_do_not_render_provider_controlled_values() -> TestResult {
    const SENTINEL: &str = "schema-secret-sentinel";
    let mut item = item_fixture("program_output")?;
    item["status"] = json!(SENTINEL);
    item["result"] = json!(SENTINEL);

    let error = ResponseReconciler::new()
        .ingest(&done(1, 0, item))
        .err()
        .ok_or("expected invalid authoritative field")?;
    assert_eq!(
        error,
        ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "program_output",
            field: "status",
        }
    );
    assert!(!error.to_string().contains(SENTINEL));
    Ok(())
}

#[test]
fn terminal_only_items_receive_the_same_schema_validation() -> TestResult {
    let mut item = item_fixture("program")?;
    item["fingerprint"] = json!(false);
    let terminal = event(
        "response.completed",
        1,
        json!({"response": {"output": [item]}}),
    );
    assert_eq!(
        ResponseReconciler::new().ingest(&terminal),
        Err(ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: "program",
            field: "fingerprint",
        })
    );
    Ok(())
}

fn assert_removed_field(item_type: &str, field: &'static str) -> TestResult {
    let mut item = item_fixture(item_type)?;
    let object = item
        .as_object_mut()
        .ok_or("known item fixture was not an object")?;
    object.remove(field);
    assert_missing_schema_error(item, item_type, field)
}

fn assert_replaced_field(item_type: &str, field: &'static str, replacement: Value) -> TestResult {
    let mut item = item_fixture(item_type)?;
    let object = item
        .as_object_mut()
        .ok_or("known item fixture was not an object")?;
    object.insert(field.to_owned(), replacement);
    assert_invalid_schema_error(item, item_type, field)
}

fn assert_nested_error(
    item_type: &str,
    mutate: impl FnOnce(&mut Value),
    field: &'static str,
) -> TestResult {
    let mut item = item_fixture(item_type)?;
    mutate(&mut item);
    assert_invalid_schema_error(item, item_type, field)
}

fn assert_nested_missing_error(
    item_type: &str,
    mutate: impl FnOnce(&mut Value),
    field: &'static str,
) -> TestResult {
    let mut item = item_fixture(item_type)?;
    mutate(&mut item);
    assert_missing_schema_error(item, item_type, field)
}

fn assert_missing_schema_error(item: Value, item_type: &str, field: &'static str) -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    let error = reconciler.ingest(&done(1, 0, item));
    assert_eq!(
        error,
        Err(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: static_item_type(item_type)?,
            field,
        })
    );
    Ok(())
}

fn assert_invalid_schema_error(item: Value, item_type: &str, field: &'static str) -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    let error = reconciler.ingest(&done(1, 0, item));
    assert_eq!(
        error,
        Err(ResponseReconciliationError::InvalidAuthoritativeItemField {
            item_type: static_item_type(item_type)?,
            field,
        })
    );
    Ok(())
}

fn item_fixture(item_type: &str) -> Result<Value, &'static str> {
    public_output_item_inventory("malformed", "malformed validation")
        .into_iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .ok_or("missing known item fixture")
}

fn static_item_type(item_type: &str) -> Result<&'static str, &'static str> {
    match item_type {
        "file_search_call" => Ok("file_search_call"),
        "computer_call" => Ok("computer_call"),
        "function_call_output" => Ok("function_call_output"),
        "computer_call_output" => Ok("computer_call_output"),
        "program" => Ok("program"),
        "program_output" => Ok("program_output"),
        "tool_search_call" => Ok("tool_search_call"),
        "tool_search_output" => Ok("tool_search_output"),
        "additional_tools" => Ok("additional_tools"),
        "code_interpreter_call" => Ok("code_interpreter_call"),
        "local_shell_call" => Ok("local_shell_call"),
        "local_shell_call_output" => Ok("local_shell_call_output"),
        "shell_call" => Ok("shell_call"),
        "shell_call_output" => Ok("shell_call_output"),
        "apply_patch_call_output" => Ok("apply_patch_call_output"),
        "apply_patch_call" => Ok("apply_patch_call"),
        "mcp_approval_request" => Ok("mcp_approval_request"),
        "mcp_list_tools" => Ok("mcp_list_tools"),
        "mcp_approval_response" => Ok("mcp_approval_response"),
        "custom_tool_call_output" => Ok("custom_tool_call_output"),
        _ => Err("unmapped known item type"),
    }
}
