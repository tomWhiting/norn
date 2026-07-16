use super::*;

#[test]
fn client_owned_nested_shapes_reject_unknown_or_incomplete_members() -> TestResult {
    assert_nested_error(
        "computer_call",
        |item| item["action"]["type"] = json!("future_action"),
        "action.type",
    )?;
    assert_nested_missing_error(
        "computer_call",
        |item| {
            item["actions"] = json!([{"type": "drag", "path": [{"x": 1}]}]);
        },
        "actions[].path[].y",
    )?;
    assert_nested_missing_error(
        "local_shell_call",
        |item| item["action"] = json!({"type": "exec", "command": ["pwd"]}),
        "action.env",
    )?;
    assert_nested_error(
        "local_shell_call",
        |item| item["action"]["env"] = json!({"TOKEN": ["secret-sentinel"]}),
        "action.env.*",
    )?;
    assert_nested_missing_error(
        "apply_patch_call",
        |item| item["operation"] = json!({"type": "update_file", "path": "file.txt"}),
        "operation.diff",
    )?;
    assert_nested_missing_error(
        "apply_patch_call",
        |item| item["caller"] = json!({"type": "program"}),
        "caller.caller_id",
    )?;
    Ok(())
}

#[test]
fn computer_call_accepts_the_documented_batched_actions_shape() -> TestResult {
    let mut item = item_fixture("computer_call")?;
    item["action"] = Value::Null;
    item["actions"] = json!([
        {"type": "screenshot"},
        {"type": "click", "button": "left", "x": 10, "y": 20, "keys": null},
        {"type": "keypress", "keys": ["CTRL", "A"]}
    ]);
    let error = ResponseReconciler::new()
        .ingest(&done(1, 0, item))
        .err()
        .ok_or("expected valid client-owned computer call to remain unsupported")?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    Ok(())
}

#[test]
fn unsupported_calls_are_schema_checked_before_capability_classification() -> TestResult {
    let mut done_item = item_fixture("mcp_approval_request")?;
    done_item["arguments"] = json!({"private": "done-sentinel"});
    assert_invalid_schema_error(done_item, "mcp_approval_request", "arguments")?;

    let mut terminal_item = item_fixture("computer_call")?;
    let object = terminal_item
        .as_object_mut()
        .ok_or("computer fixture was not an object")?;
    object.remove("call_id");
    let terminal = event(
        "response.completed",
        1,
        json!({"response": {"output": [terminal_item]}}),
    );
    assert_eq!(
        ResponseReconciler::new().ingest(&terminal),
        Err(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "computer_call",
            field: "call_id",
        })
    );
    Ok(())
}
