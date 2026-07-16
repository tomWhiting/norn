use super::*;
use crate::provider::openai::output_item_test_fixtures::public_output_item_inventory;
use crate::provider::openai::response_contract::{OutputItemActionability, PUBLIC_OUTPUT_ITEMS};

fn error_from(
    result: Result<ReconcileUpdate, ResponseReconciliationError>,
    missing: &'static str,
) -> Result<ResponseReconciliationError, Box<dyn std::error::Error>> {
    result.err().ok_or_else(|| missing.into())
}

#[test]
fn unknown_authoritative_item_is_retained_and_fails_closed() -> TestResult {
    let unknown = json!({
        "type": "future_executable_sentinel",
        "payload": "private_payload_sentinel"
    });
    let mut reconciler = ResponseReconciler::new();

    let error = error_from(
        reconciler.ingest(&done(1, 0, unknown.clone())),
        "expected an unknown-item failure",
    )?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnknownOutputItemType { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &unknown);
    assert_eq!(error.retained_items()[0].provenance.item_id, None);
    assert_eq!(error.retained_items()[0].provenance.output_index, Some(0));
    assert_eq!(
        error.retained_items()[0].provenance.sequence_number,
        Some(1)
    );

    let rendered = error.to_string();
    assert!(!rendered.contains("future_executable_sentinel"));
    assert!(!rendered.contains("private_payload_sentinel"));
    assert_eq!(
        reconciler.ingest(&event("response.in_progress", 2, json!({}))),
        Err(ResponseReconciliationError::AlreadyFailed)
    );
    Ok(())
}

#[test]
fn pinned_but_unsupported_executable_is_distinct_and_retained() -> TestResult {
    let shell = json!({
        "id": "shell_identity_sentinel",
        "type": "shell_call",
        "call_id": "shell_call_sentinel",
        "status": "completed",
        "action": {
            "commands": ["private_command_sentinel"],
            "timeout_ms": 1_000,
            "max_output_length": 4_096
        },
        "environment": {"type": "local"}
    });
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, shell.clone()))?;

    let error = error_from(
        reconciler.ingest(&done(2, 0, shell.clone())),
        "expected an unsupported executable-item failure",
    )?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnsupportedExecutableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &shell);

    let rendered = error.to_string();
    assert!(!rendered.contains("shell_call"));
    assert!(!rendered.contains("shell_identity_sentinel"));
    assert!(!rendered.contains("private_command_sentinel"));
    Ok(())
}

#[test]
fn every_pinned_unsupported_executable_kind_fails_closed() -> TestResult {
    let unsupported: Vec<_> = PUBLIC_OUTPUT_ITEMS
        .iter()
        .filter(|entry| entry.actionability() == OutputItemActionability::Executable)
        .filter(|entry| !matches!(entry.name(), "function_call" | "custom_tool_call"))
        .collect();
    assert_eq!(unsupported.len(), 4);

    let fixtures = public_output_item_inventory("unsupported", "unsupported executable");
    for entry in unsupported {
        let raw = fixtures
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some(entry.name()))
            .cloned()
            .ok_or("missing unsupported executable fixture")?;
        let mut reconciler = ResponseReconciler::new();
        let error = error_from(
            reconciler.ingest(&done(1, 0, raw.clone())),
            "expected unsupported executable-item failure",
        )?;

        assert!(matches!(
            error,
            ResponseReconciliationError::UnsupportedExecutableItem { .. }
        ));
        assert_eq!(error.retained_items().len(), 1);
        assert_eq!(error.retained_items()[0].item.raw(), &raw);
        assert!(!error.to_string().contains(entry.name()));
    }
    Ok(())
}

#[test]
fn terminal_failure_retains_the_complete_authoritative_order() -> TestResult {
    let first = reasoning("rs_first");
    let unknown = json!({
        "type": "future_inert_or_executable",
        "payload": {"kept": true}
    });
    let last = message("msg_last", "final");
    let terminal = event(
        "response.completed",
        1,
        json!({"response": {"output": [first, unknown, last]}}),
    );
    let mut reconciler = ResponseReconciler::new();

    let error = error_from(
        reconciler.ingest(&terminal),
        "expected terminal unknown-item failure",
    )?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnknownOutputItemType { .. }
    ));
    let retained = error.retained_items();
    assert_eq!(retained.len(), 3);
    assert_eq!(retained[0].item.id(), Some("rs_first"));
    assert_eq!(retained[1].item.id(), None);
    assert_eq!(retained[2].item.id(), Some("msg_last"));
    assert_eq!(
        retained
            .iter()
            .map(|item| item.provenance.output_index)
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    Ok(())
}

#[test]
fn explicit_incomplete_done_call_is_retained_but_never_accepted() -> TestResult {
    let call = function_call("fc_incomplete", "{}", "incomplete");
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(
        1,
        0,
        function_call("fc_incomplete", "", "in_progress"),
    ))?;

    assert!(matches!(
        reconciler.ingest(&done(2, 0, call.clone()))?,
        ReconcileUpdate::CompletedItem { .. }
    ));
    let error = error_from(
        reconciler.ingest(&event(
            "response.incomplete",
            3,
            json!({"response": {"output": [call]}}),
        )),
        "expected unresolved terminal executable-item failure",
    )?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnresolvedActionableItem { .. }
    ));
    assert_eq!(error.retained_items().len(), 1);
    assert_eq!(error.retained_items()[0].item.raw(), &call);
    Ok(())
}

#[test]
fn optional_call_status_does_not_invent_an_unresolved_state() -> TestResult {
    let mut call = function_call("fc_no_status", "{}", "completed");
    call.as_object_mut()
        .ok_or("function call fixture must be an object")?
        .remove("status");
    let mut reconciler = ResponseReconciler::new();

    let update = reconciler.ingest(&done(1, 0, call.clone()))?;
    let ReconcileUpdate::CompletedItem { item, .. } = update else {
        return Err("expected completed item".into());
    };
    assert_eq!(item.item.raw(), &call);
    Ok(())
}

#[test]
fn unknown_announcement_cannot_disappear_into_empty_terminal_output() -> TestResult {
    let unknown = json!({"id": "unknown_a", "type": "future_item"});
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, unknown))?;

    let error = error_from(
        reconciler.ingest(&event(
            "response.incomplete",
            2,
            json!({"response": {"output": []}}),
        )),
        "expected unresolved unknown-item failure",
    )?;
    assert!(matches!(
        error,
        ResponseReconciliationError::UnknownOutputItemType { .. }
    ));
    assert!(error.retained_items().is_empty());
    Ok(())
}

#[test]
fn identity_error_display_does_not_render_provider_item_id() -> TestResult {
    let sentinel = "provider_controlled_identity_sentinel";
    let mut reconciler = ResponseReconciler::new();
    reconciler.ingest(&added(1, 0, message(sentinel, "first")))?;
    let error = error_from(
        reconciler.ingest(&added(2, 1, message(sentinel, "second"))),
        "expected item identity rebound",
    )?;

    assert!(matches!(
        error,
        ResponseReconciliationError::ItemIdRebound { .. }
    ));
    assert!(!error.to_string().contains(sentinel));
    Ok(())
}
