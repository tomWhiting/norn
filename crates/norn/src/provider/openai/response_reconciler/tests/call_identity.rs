use super::*;

fn replace_string_field(
    value: &mut Value,
    field: &str,
    replacement: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let object = value
        .as_object_mut()
        .ok_or("call fixture must be an object")?;
    object.insert(field.to_owned(), json!(replacement));
    Ok(())
}

#[test]
fn function_call_id_and_name_must_match_the_announcement() -> TestResult {
    let announced = function_call("fc_a", "", "in_progress");

    let mut changed_id = function_call("fc_a", "{}", "completed");
    replace_string_field(&mut changed_id, "call_id", "call_changed")?;
    let mut id_reconciler = ResponseReconciler::new();
    id_reconciler.ingest(&added(1, 0, announced.clone()))?;
    assert_eq!(
        id_reconciler.ingest(&done(2, 0, changed_id)),
        Err(ResponseReconciliationError::AnnouncedCallIdConflict)
    );

    let mut changed_name = function_call("fc_a", "{}", "completed");
    replace_string_field(&mut changed_name, "name", "different")?;
    let mut name_reconciler = ResponseReconciler::new();
    name_reconciler.ingest(&added(1, 0, announced))?;
    assert_eq!(
        name_reconciler.ingest(&done(2, 0, changed_name)),
        Err(ResponseReconciliationError::AnnouncedCallNameConflict)
    );
    Ok(())
}

#[test]
fn custom_call_id_and_name_must_match_terminal_authority() -> TestResult {
    let announced = custom_call("ct_a", "", "in_progress");
    let mut changed = custom_call("ct_a", "patch", "completed");
    replace_string_field(&mut changed, "call_id", "call_other")?;
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, announced))?;

    assert_eq!(
        reconciler.ingest(&event(
            "response.completed",
            2,
            json!({"response": {"output": [changed]}}),
        )),
        Err(ResponseReconciliationError::AnnouncedCallIdConflict)
    );
    Ok(())
}

#[test]
fn one_call_id_cannot_identify_two_output_items() -> TestResult {
    let first = function_call("fc_a", "", "in_progress");
    let mut second = custom_call("ct_b", "", "in_progress");
    replace_string_field(&mut second, "call_id", "call_fc_a")?;
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, first))?;

    assert_eq!(
        reconciler.ingest(&added(2, 1, second)),
        Err(ResponseReconciliationError::CallIdReused)
    );
    Ok(())
}

#[test]
fn stable_distinct_call_ids_survive_item_and_terminal_authority() -> TestResult {
    let function = function_call("fc_a", "{}", "completed");
    let custom = custom_call("ct_b", "patch", "completed");
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    reconciler.ingest(&added(2, 1, custom_call("ct_b", "", "in_progress")))?;
    reconciler.ingest(&done(3, 0, function.clone()))?;
    reconciler.ingest(&done(4, 1, custom.clone()))?;

    assert!(matches!(
        reconciler.ingest(&event(
            "response.completed",
            5,
            json!({"response": {"output": [function, custom]}}),
        ))?,
        ReconcileUpdate::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn idless_function_and_custom_calls_reconcile_without_fabricating_item_ids() -> TestResult {
    let function = json!({
        "type": "function_call",
        "call_id": "call_function",
        "name": "lookup",
        "arguments": "{}",
        "status": "completed"
    });
    let custom = json!({
        "type": "custom_tool_call",
        "call_id": "call_custom",
        "name": "patch",
        "input": "change",
        "status": "completed"
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, function.clone()))?;
    reconciler.ingest(&added(2, 1, custom.clone()))?;
    reconciler.ingest(&done(3, 0, function.clone()))?;
    reconciler.ingest(&done(4, 1, custom.clone()))?;

    let update = reconciler.ingest(&event(
        "response.completed",
        5,
        json!({"response": {"output": [function, custom]}}),
    ))?;
    let ReconcileUpdate::Terminal { items, .. } = update else {
        return Err("expected terminal items".into());
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].provenance.item_id, None);
    assert_eq!(items[1].provenance.item_id, None);
    assert_eq!(
        items[0]
            .item
            .as_function_call()
            .ok_or("expected function call")?
            .call_id(),
        "call_function"
    );
    assert_eq!(
        items[1]
            .item
            .as_custom_tool_call()
            .ok_or("expected custom tool call")?
            .call_id(),
        "call_custom"
    );
    Ok(())
}

#[test]
fn optional_item_id_can_be_refined_but_not_rebound() -> TestResult {
    let idless = json!({
        "type": "function_call",
        "call_id": "call_a",
        "name": "lookup",
        "arguments": "",
        "status": "in_progress"
    });
    let identified = json!({
        "id": "fc_a",
        "type": "function_call",
        "call_id": "call_a",
        "name": "lookup",
        "arguments": "{}",
        "status": "completed"
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, idless))?;
    let update = reconciler.ingest(&done(2, 0, identified))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item".into());
    };
    assert_eq!(item.provenance.item_id.as_deref(), Some("fc_a"));

    let conflicting = json!({
        "id": "fc_other",
        "type": "function_call",
        "call_id": "call_a",
        "name": "lookup",
        "arguments": "{}",
        "status": "completed"
    });
    assert_eq!(
        reconciler.ingest(&event(
            "response.completed",
            3,
            json!({"response": {"output": [conflicting]}}),
        )),
        Err(ResponseReconciliationError::OutputIndexRebound { output_index: 0 })
    );
    Ok(())
}

#[test]
fn a_present_item_id_remains_bound_when_later_authority_omits_it() -> TestResult {
    let announced = function_call("fc_a", "", "in_progress");
    let idless = json!({
        "type": "function_call",
        "call_id": "call_fc_a",
        "name": "lookup",
        "arguments": "{}",
        "status": "completed"
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, announced))?;
    let update = reconciler.ingest(&done(2, 0, idless))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item".into());
    };
    assert_eq!(item.provenance.item_id.as_deref(), Some("fc_a"));
    Ok(())
}

#[test]
fn required_delta_item_id_refines_an_idless_call_announcement() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(
        1,
        0,
        json!({
            "type": "function_call",
            "call_id": "call_a",
            "name": "lookup",
            "arguments": "",
            "status": "in_progress"
        }),
    ))?;
    reconciler.ingest(&delta(
        "response.function_call_arguments.delta",
        2,
        "fc_stream",
        0,
        json!({"delta": "{}"}),
    ))?;
    let update = reconciler.ingest(&done(
        3,
        0,
        json!({
            "type": "function_call",
            "call_id": "call_a",
            "name": "lookup",
            "arguments": "{}",
            "status": "completed"
        }),
    ))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item".into());
    };
    assert_eq!(item.provenance.item_id.as_deref(), Some("fc_stream"));

    assert_eq!(
        reconciler.ingest(&added(
            4,
            1,
            json!({
                "id": "fc_stream",
                "type": "function_call",
                "call_id": "call_b",
                "name": "lookup",
                "arguments": "",
                "status": "in_progress"
            }),
        )),
        Err(ResponseReconciliationError::ItemIdRebound {
            item_id: "fc_stream".to_owned(),
            prior_index: 0,
            new_index: 1,
        })
    );
    Ok(())
}

#[test]
fn a_delta_cannot_rebind_an_identified_output_index() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, function_call("fc_a", "", "in_progress")))?;
    assert_eq!(
        reconciler.ingest(&delta(
            "response.function_call_arguments.delta",
            2,
            "fc_other",
            0,
            json!({"delta": "{}"}),
        )),
        Err(ResponseReconciliationError::OutputIndexRebound { output_index: 0 })
    );
    Ok(())
}

#[test]
fn tool_deltas_still_require_provider_item_id() -> TestResult {
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(
        1,
        0,
        json!({
            "type": "function_call",
            "call_id": "call_a",
            "name": "lookup",
            "arguments": "",
            "status": "in_progress"
        }),
    ))?;
    assert_eq!(
        reconciler.ingest(&event(
            "response.function_call_arguments.delta",
            2,
            json!({"output_index": 0, "delta": "{}"}),
        )),
        Err(ResponseReconciliationError::InvalidEnvelopeField {
            event_type: "response delta",
            field: "item_id",
        })
    );
    Ok(())
}
