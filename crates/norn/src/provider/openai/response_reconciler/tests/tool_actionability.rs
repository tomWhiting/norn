use super::*;

fn tool_search_call(execution: Option<Value>) -> Value {
    let mut item = json!({
        "id": "tool_search_identity",
        "type": "tool_search_call",
        "call_id": "tool_search_call_id",
        "status": "completed",
        "arguments": {"goal": "private_search_goal"}
    });
    if let (Some(object), Some(execution)) = (item.as_object_mut(), execution) {
        object.insert("execution".to_owned(), execution);
    }
    item
}

fn shell_call(environment: Option<Value>) -> Value {
    let mut item = json!({
        "id": "shell_identity",
        "type": "shell_call",
        "call_id": "shell_call_id",
        "status": "completed",
        "action": {
            "commands": ["private_shell_command"],
            "timeout_ms": 1_000,
            "max_output_length": 4_096
        }
    });
    if let (Some(object), Some(environment)) = (item.as_object_mut(), environment) {
        object.insert("environment".to_owned(), environment);
    }
    item
}

fn terminal_error(raw: &Value) -> Result<ResponseReconciliationError, Box<dyn std::error::Error>> {
    ResponseReconciler::new()
        .ingest(&event(
            "response.completed",
            1,
            json!({"response": {"output": [raw]}}),
        ))
        .err()
        .ok_or_else(|| "expected terminal response rejection".into())
}

#[test]
fn terminal_server_tool_search_is_provider_hosted_and_retained() -> TestResult {
    let raw = tool_search_call(Some(json!("server")));
    let update = ResponseReconciler::new().ingest(&event(
        "response.completed",
        1,
        json!({"response": {"output": [&raw]}}),
    ))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal update".into());
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.raw(), &raw);
    Ok(())
}

#[test]
fn completed_server_tool_search_is_provider_hosted_and_retained() -> TestResult {
    let raw = tool_search_call(Some(json!("server")));
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, raw.clone()))?;
    let update = reconciler.ingest(&done(2, 0, raw.clone()))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item update".into());
    };
    assert_eq!(item.item.raw(), &raw);
    Ok(())
}

#[test]
fn terminal_client_tool_search_is_executable_and_retained() -> TestResult {
    let raw = tool_search_call(Some(json!("client")));
    let error = terminal_error(&raw)?;

    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &raw);
    let rendered = error.to_string();
    assert!(!rendered.contains("private_search_goal"));
    assert!(!rendered.contains("tool_search_call_id"));
    Ok(())
}

#[test]
fn completed_client_tool_search_is_executable_and_retained() -> TestResult {
    let raw = tool_search_call(Some(json!("client")));
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, raw.clone()))?;
    let error = reconciler
        .ingest(&done(2, 0, raw.clone()))
        .err()
        .ok_or("expected client tool-search rejection")?;

    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &raw);
    Ok(())
}

#[test]
fn tool_search_execution_is_required_and_closed_to_unknown_values() -> TestResult {
    for (raw, expected_field) in [
        (tool_search_call(None), "execution"),
        (tool_search_call(Some(Value::Null)), "execution"),
        (
            tool_search_call(Some(json!("private_execution_sentinel"))),
            "execution",
        ),
    ] {
        let error = terminal_error(&raw)?;
        assert!(matches!(
            error,
            ResponseReconciliationError::MissingAuthoritativeItemField { .. }
                | ResponseReconciliationError::InvalidAuthoritativeItemField { .. }
        ));
        assert!(error.retained_items().is_empty());
        let rendered = error.to_string();
        assert!(rendered.contains(expected_field));
        assert!(!rendered.contains("private_execution_sentinel"));
        assert!(!rendered.contains("private_search_goal"));
    }
    Ok(())
}

#[test]
fn tool_search_announcement_cannot_downgrade_client_actionability() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, tool_search_call(Some(json!("client")))))?;
    let error = reconciler
        .ingest(&done(2, 0, tool_search_call(Some(json!("server")))))
        .err()
        .ok_or("expected actionability conflict")?;

    assert_eq!(
        error,
        ResponseReconciliationError::AddedItemActionabilityConflict
    );
    assert!(!error.to_string().contains("tool_search_call_id"));
    Ok(())
}

#[test]
fn terminal_hosted_shell_is_provider_owned_and_retained() -> TestResult {
    let raw = shell_call(Some(json!({
        "type": "container_reference",
        "container_id": "container_identity"
    })));
    let update = ResponseReconciler::new().ingest(&event(
        "response.completed",
        1,
        json!({"response": {"output": [&raw]}}),
    ))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal update".into());
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.raw(), &raw);
    Ok(())
}

#[test]
fn completed_hosted_shell_is_provider_owned_and_retained() -> TestResult {
    let raw = shell_call(Some(json!({
        "type": "container_reference",
        "container_id": "container_identity"
    })));
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, raw.clone()))?;
    let update = reconciler.ingest(&done(2, 0, raw.clone()))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item update".into());
    };
    assert_eq!(item.item.raw(), &raw);
    Ok(())
}

#[test]
fn terminal_local_shell_is_executable_and_retained() -> TestResult {
    let raw = shell_call(Some(json!({"type": "local"})));
    let error = terminal_error(&raw)?;

    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &raw);
    assert!(!error.to_string().contains("private_shell_command"));
    Ok(())
}

#[test]
fn shell_announcement_cannot_downgrade_local_actionability() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, shell_call(Some(json!({"type": "local"})))))?;
    let error = reconciler
        .ingest(&done(
            2,
            0,
            shell_call(Some(json!({
                "type": "container_reference",
                "container_id": "container_identity"
            }))),
        ))
        .err()
        .ok_or("expected actionability conflict")?;

    assert_eq!(
        error,
        ResponseReconciliationError::AddedItemActionabilityConflict
    );
    assert!(!error.to_string().contains("private_shell_command"));
    Ok(())
}

#[test]
fn null_shell_environment_remains_fail_closed_and_retained() -> TestResult {
    let raw = shell_call(Some(Value::Null));
    let error = terminal_error(&raw)?;

    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &raw);
    Ok(())
}

#[test]
fn shell_environment_is_required_and_closed_to_unknown_values() -> TestResult {
    for raw in [
        shell_call(None),
        shell_call(Some(json!({"type": "private_environment_sentinel"}))),
    ] {
        let error = terminal_error(&raw)?;
        assert!(matches!(
            error,
            ResponseReconciliationError::MissingAuthoritativeItemField { .. }
                | ResponseReconciliationError::InvalidAuthoritativeItemField { .. }
        ));
        let rendered = error.to_string();
        assert!(!rendered.contains("private_environment_sentinel"));
        assert!(!rendered.contains("private_shell_command"));
    }
    Ok(())
}
