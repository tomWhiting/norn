use uuid::Uuid;

use super::fork_seed::seed_fork_events;
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

#[test]
fn fork_seed_does_not_duplicate_canonical_function_or_custom_outputs() -> TestResult {
    for (suffix, names) in [
        ("seed_function", ["function_call", "function_call_output"]),
        (
            "seed_custom",
            ["custom_tool_call", "custom_tool_call_output"],
        ),
    ] {
        let store = EventStore::new();
        let parent = canonical_assistant(suffix, &names)?;
        seed_fork_events(&store, &[parent], None, Uuid::new_v4())?;
        let events = store.events();
        assert_eq!(events.len(), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SessionEvent::ToolResult { .. }))
        );
    }
    Ok(())
}
