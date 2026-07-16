use crate::r#loop::ensure_tool_results_complete;
use crate::provider::openai::output_item_test_fixtures::response_items_named;
use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::store::EventStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn canonical_assistant(
    suffix: &str,
    names: &[&str],
) -> Result<SessionEvent, Box<dyn std::error::Error>> {
    let response_items = response_items_named(suffix, names)?;
    if response_items.len() != names.len() {
        return Err("canonical fixture selection was incomplete".into());
    }
    Ok(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    })
}

#[tokio::test]
async fn completion_guard_does_not_duplicate_canonical_function_or_custom_outputs() -> TestResult {
    for (suffix, names) in [
        ("guard_function", ["function_call", "function_call_output"]),
        (
            "guard_custom",
            ["custom_tool_call", "custom_tool_call_output"],
        ),
    ] {
        let store = EventStore::new();
        store.append(canonical_assistant(suffix, &names)?)?;
        ensure_tool_results_complete(&store).await;
        assert_eq!(store.events().len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn completion_guard_repairs_an_older_orphan_past_a_later_assistant_turn() -> TestResult {
    let store = EventStore::new();
    store.append(canonical_assistant("older", &["custom_tool_call"])?)?;
    store.append(canonical_assistant("later", &["message"])?)?;
    ensure_tool_results_complete(&store).await;
    let events = store.events();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events.last(),
        Some(SessionEvent::ToolResult { tool_call_id, .. }) if tool_call_id == "call_ctc_older"
    ));
    Ok(())
}
