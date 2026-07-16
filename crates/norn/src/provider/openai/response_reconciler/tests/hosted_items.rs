use super::*;

fn image_item(id: &str, result: Option<&str>) -> Value {
    let mut item = json!({
        "id": id,
        "type": "image_generation_call",
        "status": "completed"
    });
    if let (Some(object), Some(result)) = (item.as_object_mut(), result) {
        object.insert("result".to_owned(), json!(result));
    }
    item
}

fn mcp_item(id: &str, arguments: &str, error: &Value) -> Value {
    json!({
        "id": id,
        "type": "mcp_call",
        "arguments": arguments,
        "name": "lookup",
        "server_label": "server",
        "output": "result",
        "error": error
    })
}

fn code_item(id: &str, code: Option<&str>) -> Value {
    let mut item = json!({
        "id": id,
        "type": "code_interpreter_call",
        "container_id": "cntr_a",
        "status": "completed",
        "outputs": []
    });
    if let (Some(object), Some(code)) = (item.as_object_mut(), code) {
        object.insert("code".to_owned(), json!(code));
    }
    item
}

fn mcp_list_item(id: &str, tools: Option<Value>) -> Value {
    let mut item = json!({
        "id": id,
        "type": "mcp_list_tools",
        "server_label": "server"
    });
    if let (Some(object), Some(tools)) = (item.as_object_mut(), tools) {
        object.insert("tools".to_owned(), tools);
    }
    item
}

fn file_search_item(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "file_search_call",
        "status": status,
        "queries": ["query"]
    })
}

fn web_search_item(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "web_search_call",
        "status": status,
        "action": {"type": "search", "query": "query"}
    })
}

fn hosted_event(event_type: &str, sequence: u64, id: &str) -> SseEvent {
    delta(event_type, sequence, id, 0, json!({}))
}

#[test]
fn image_previews_are_indexed_snapshots_not_final_content() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, image_item("ig_a", None)))?;
    reconciler.ingest(&hosted_event(
        "response.image_generation_call.in_progress",
        2,
        "ig_a",
    ))?;
    reconciler.ingest(&hosted_event(
        "response.image_generation_call.generating",
        3,
        "ig_a",
    ))?;
    for (sequence, index, image) in [(4, 0, "preview-a"), (5, 1, "preview-b")] {
        reconciler.ingest(&delta(
            "response.image_generation_call.partial_image",
            sequence,
            "ig_a",
            0,
            json!({"partial_image_index": index, "partial_image_b64": image}),
        ))?;
    }
    reconciler.ingest(&hosted_event(
        "response.image_generation_call.completed",
        6,
        "ig_a",
    ))?;

    let final_item = image_item("ig_a", Some("final-image-differs-from-previews"));
    assert!(matches!(
        reconciler.ingest(&done(7, 0, final_item.clone()))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    assert!(matches!(
        reconciler.ingest(&event(
            "response.completed",
            8,
            json!({"response": {"output": [final_item]}}),
        ))?,
        ReconcileUpdate::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn image_preview_duplicates_are_idempotent_but_conflicts_fail() -> TestResult {
    let preview = delta(
        "response.image_generation_call.partial_image",
        2,
        "ig_a",
        0,
        json!({"partial_image_index": 0, "partial_image_b64": "same"}),
    );
    let mut duplicate = ResponseReconciler::new();
    duplicate.ingest(&added(1, 0, image_item("ig_a", None)))?;
    duplicate.ingest(&preview)?;
    assert_eq!(
        duplicate.ingest(&delta(
            "response.image_generation_call.partial_image",
            3,
            "ig_a",
            0,
            json!({"partial_image_index": 0, "partial_image_b64": "same"}),
        ))?,
        ReconcileUpdate::Accepted
    );

    let mut conflict = ResponseReconciler::new();
    conflict.ingest(&added(1, 0, image_item("ig_a", None)))?;
    conflict.ingest(&preview)?;
    assert_eq!(
        conflict.ingest(&delta(
            "response.image_generation_call.partial_image",
            3,
            "ig_a",
            0,
            json!({"partial_image_index": 0, "partial_image_b64": "different"}),
        )),
        Err(ResponseReconciliationError::ConflictingItemScopedPreview)
    );
    Ok(())
}

#[test]
fn image_final_result_is_required_and_completion_closes_previews() -> TestResult {
    let mut missing = ResponseReconciler::new();
    missing.ingest(&added(1, 0, image_item("ig_a", None)))?;
    assert_eq!(
        missing.ingest(&done(2, 0, image_item("ig_a", None))),
        Err(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "image_generation_call",
            field: "result",
        })
    );

    let mut late = ResponseReconciler::new();
    late.ingest(&added(1, 0, image_item("ig_a", None)))?;
    late.ingest(&hosted_event(
        "response.image_generation_call.completed",
        2,
        "ig_a",
    ))?;
    assert_eq!(
        late.ingest(&delta(
            "response.image_generation_call.partial_image",
            3,
            "ig_a",
            0,
            json!({"partial_image_index": 0, "partial_image_b64": "late"}),
        )),
        Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
    );
    Ok(())
}

#[test]
fn mcp_arguments_repair_preview_and_done_binds_final_item() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, mcp_item("mcp_a", "", &Value::Null)))?;
    reconciler.ingest(&hosted_event("response.mcp_call.in_progress", 2, "mcp_a"))?;
    reconciler.ingest(&delta(
        "response.mcp_call_arguments.delta",
        3,
        "mcp_a",
        0,
        json!({"delta": "{\"x\":"}),
    ))?;
    let completion = delta(
        "response.mcp_call_arguments.done",
        4,
        "mcp_a",
        0,
        json!({"arguments": "{\"x\":1}"}),
    );
    reconciler.ingest(&completion)?;
    assert_eq!(
        reconciler.ingest(&delta(
            "response.mcp_call_arguments.done",
            5,
            "mcp_a",
            0,
            json!({"arguments": "{\"x\":1}"}),
        ))?,
        ReconcileUpdate::DuplicateChannelCompletion
    );
    reconciler.ingest(&hosted_event("response.mcp_call.completed", 6, "mcp_a"))?;
    assert!(matches!(
        reconciler.ingest(&done(7, 0, mcp_item("mcp_a", "{\"x\":1}", &Value::Null),))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

#[test]
fn mcp_done_conflict_and_late_delta_fail_but_nullable_failed_error_is_valid() -> TestResult {
    let mut mismatch = ResponseReconciler::new();
    mismatch.ingest(&added(1, 0, mcp_item("mcp_a", "", &Value::Null)))?;
    mismatch.ingest(&delta(
        "response.mcp_call_arguments.done",
        2,
        "mcp_a",
        0,
        json!({"arguments": "{}"}),
    ))?;
    assert_eq!(
        mismatch.ingest(&done(3, 0, mcp_item("mcp_a", "different", &Value::Null),)),
        Err(ResponseReconciliationError::ItemScopedCompletionConflict)
    );

    let mut late = ResponseReconciler::new();
    late.ingest(&added(1, 0, mcp_item("mcp_a", "", &Value::Null)))?;
    late.ingest(&delta(
        "response.mcp_call_arguments.done",
        2,
        "mcp_a",
        0,
        json!({"arguments": "{}"}),
    ))?;
    assert_eq!(
        late.ingest(&delta(
            "response.mcp_call_arguments.delta",
            3,
            "mcp_a",
            0,
            json!({"delta": "late"}),
        )),
        Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
    );

    let mut failed = ResponseReconciler::new();
    failed.ingest(&added(1, 0, mcp_item("mcp_a", "", &Value::Null)))?;
    failed.ingest(&hosted_event("response.mcp_call.failed", 2, "mcp_a"))?;
    assert!(matches!(
        failed.ingest(&done(3, 0, mcp_item("mcp_a", "{}", &Value::Null)))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    Ok(())
}

#[test]
fn code_delta_done_and_item_authority_are_exact() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, code_item("ci_a", Some(""))))?;
    reconciler.ingest(&hosted_event(
        "response.code_interpreter_call.in_progress",
        2,
        "ci_a",
    ))?;
    reconciler.ingest(&delta(
        "response.code_interpreter_call_code.delta",
        3,
        "ci_a",
        0,
        json!({"delta": "print"}),
    ))?;
    reconciler.ingest(&delta(
        "response.code_interpreter_call_code.done",
        4,
        "ci_a",
        0,
        json!({"code": "print('ok')"}),
    ))?;
    reconciler.ingest(&hosted_event(
        "response.code_interpreter_call.interpreting",
        5,
        "ci_a",
    ))?;
    reconciler.ingest(&hosted_event(
        "response.code_interpreter_call.completed",
        6,
        "ci_a",
    ))?;
    assert!(matches!(
        reconciler.ingest(&done(7, 0, code_item("ci_a", Some("print('ok')")),))?,
        ReconcileUpdate::CompletedItem { .. }
    ));

    let mut missing = ResponseReconciler::new();
    missing.ingest(&added(1, 0, code_item("ci_a", Some(""))))?;
    assert_eq!(
        missing.ingest(&done(2, 0, code_item("ci_a", None))),
        Err(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "code_interpreter_call",
            field: "code",
        })
    );
    Ok(())
}

#[test]
fn hosted_lifecycle_checks_family_transitions_and_terminal_presence() -> TestResult {
    let mut family = ResponseReconciler::new();
    family.ingest(&added(1, 0, image_item("ig_a", None)))?;
    assert_eq!(
        family.ingest(&hosted_event("response.mcp_call.in_progress", 2, "ig_a",)),
        Err(ResponseReconciliationError::ItemScopedFamilyConflict)
    );

    let mut transition = ResponseReconciler::new();
    transition.ingest(&added(1, 0, image_item("ig_a", None)))?;
    transition.ingest(&hosted_event(
        "response.image_generation_call.completed",
        2,
        "ig_a",
    ))?;
    assert_eq!(
        transition.ingest(&hosted_event(
            "response.image_generation_call.generating",
            3,
            "ig_a",
        )),
        Err(ResponseReconciliationError::ConflictingHostedLifecycle)
    );

    let mut absent = ResponseReconciler::new();
    absent.ingest(&added(1, 0, image_item("ig_a", None)))?;
    absent.ingest(&hosted_event(
        "response.image_generation_call.in_progress",
        2,
        "ig_a",
    ))?;
    assert_eq!(
        absent.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": []}}),
        )),
        Err(ResponseReconciliationError::ItemScopedStateAbsentFromTerminal)
    );
    Ok(())
}

#[test]
fn file_and_web_search_lifecycles_validate_identity_and_family() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, file_search_item("fs_a", "in_progress")))?;
    reconciler.ingest(&added(2, 1, web_search_item("ws_a", "in_progress")))?;
    reconciler.ingest(&delta(
        "response.file_search_call.in_progress",
        3,
        "fs_a",
        0,
        json!({}),
    ))?;
    reconciler.ingest(&delta(
        "response.web_search_call.in_progress",
        4,
        "ws_a",
        1,
        json!({}),
    ))?;
    reconciler.ingest(&delta(
        "response.file_search_call.searching",
        5,
        "fs_a",
        0,
        json!({}),
    ))?;
    reconciler.ingest(&delta(
        "response.web_search_call.searching",
        6,
        "ws_a",
        1,
        json!({}),
    ))?;
    reconciler.ingest(&delta(
        "response.file_search_call.completed",
        7,
        "fs_a",
        0,
        json!({}),
    ))?;
    reconciler.ingest(&delta(
        "response.web_search_call.completed",
        8,
        "ws_a",
        1,
        json!({}),
    ))?;
    reconciler.ingest(&done(9, 0, file_search_item("fs_a", "completed")))?;
    reconciler.ingest(&done(10, 1, web_search_item("ws_a", "completed")))?;

    let mut wrong = ResponseReconciler::new();
    wrong.ingest(&added(1, 0, file_search_item("fs_a", "in_progress")))?;
    assert_eq!(
        wrong.ingest(&delta(
            "response.web_search_call.searching",
            2,
            "fs_a",
            0,
            json!({}),
        )),
        Err(ResponseReconciliationError::ItemScopedFamilyConflict)
    );
    Ok(())
}

#[test]
fn mcp_list_tools_requires_authoritative_tools_array() -> TestResult {
    let mut valid = ResponseReconciler::new();
    valid.ingest(&added(1, 0, mcp_list_item("mcpl_a", Some(json!([])))))?;
    valid.ingest(&hosted_event(
        "response.mcp_list_tools.in_progress",
        2,
        "mcpl_a",
    ))?;
    valid.ingest(&hosted_event(
        "response.mcp_list_tools.completed",
        3,
        "mcpl_a",
    ))?;
    assert!(matches!(
        valid.ingest(&done(
            4,
            0,
            mcp_list_item(
                "mcpl_a",
                Some(json!([{"name": "lookup", "input_schema": {}}])),
            ),
        ))?,
        ReconcileUpdate::CompletedItem { .. }
    ));

    let mut missing = ResponseReconciler::new();
    missing.ingest(&added(1, 0, mcp_list_item("mcpl_a", Some(json!([])))))?;
    assert_eq!(
        missing.ingest(&done(2, 0, mcp_list_item("mcpl_a", None))),
        Err(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "mcp_list_tools",
            field: "tools",
        })
    );
    Ok(())
}
