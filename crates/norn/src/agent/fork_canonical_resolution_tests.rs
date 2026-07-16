use crate::agent::{ContextFilter, verify_no_orphan_tool_calls};
use crate::provider::openai::output_item_test_fixtures::response_items_named;
use crate::provider::openai::request::build_payload;
use crate::provider::request::{ProviderRequest, ToolCallCaller, ToolCallKind};
use crate::session::conversion::events_to_messages;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

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
fn fork_verification_accepts_canonical_function_and_custom_outputs() -> TestResult {
    for (suffix, names) in [
        ("verify_function", ["function_call", "function_call_output"]),
        (
            "verify_custom",
            ["custom_tool_call", "custom_tool_call_output"],
        ),
    ] {
        let event = canonical_assistant(suffix, &names)?;
        let orphans = verify_no_orphan_tool_calls(&[event], "unrelated_fork_call");
        assert!(orphans.is_empty());
    }
    Ok(())
}

#[test]
fn fork_verification_reports_an_older_orphan_past_a_later_assistant_turn() -> TestResult {
    let older = canonical_assistant("older", &["function_call"])?;
    let later = canonical_assistant("later", &["message"])?;
    let orphans = verify_no_orphan_tool_calls(&[older, later], "unrelated_fork_call");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, "call_fc_older");
    Ok(())
}

#[test]
fn exclude_tool_calls_removes_canonical_calls_and_outputs_from_exact_replay() -> TestResult {
    let source = canonical_assistant(
        "filter",
        &[
            "message",
            "function_call",
            "function_call_output",
            "reasoning",
            "custom_tool_call",
            "custom_tool_call_output",
            "program",
            "program_output",
        ],
    )?;
    let expected = response_items_named("filter", &["message", "reasoning"])?
        .into_iter()
        .map(|entry| entry.item.raw().clone())
        .collect::<Vec<_>>();
    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: None,
        exclude_tool_calls: true,
    }
    .apply(&[source]);
    let request = ProviderRequest {
        messages: events_to_messages(&filtered),
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    };
    let payload = build_payload(&request, "test")?;
    let input = payload
        .get("input")
        .and_then(serde_json::Value::as_array)
        .ok_or("filtered request input was not an array")?;
    assert_eq!(input, &expected);
    Ok(())
}

#[test]
fn recent_filter_drops_outputs_whose_calls_precede_metadata_boundary() -> TestResult {
    let legacy_call = SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "call_legacy_cut".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({}),
            kind: ToolCallKind::Function,
            caller: ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    };
    let metadata = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "boundary".to_owned(),
        data: serde_json::json!({"kept": true}),
    };
    let legacy_output = SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "call_legacy_cut".to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::json!({"content": "orphan"}),
        spool_ref: None,
        duration_ms: 1,
    };
    let tail = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "tail".to_owned(),
    };
    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(3),
        exclude_tool_calls: false,
    }
    .apply(&[legacy_call, metadata, legacy_output, tail]);
    assert_eq!(filtered.len(), 2);
    assert!(matches!(filtered[0], SessionEvent::Custom { .. }));
    assert!(matches!(filtered[1], SessionEvent::UserMessage { .. }));

    let canonical_call = canonical_assistant("canonical_cut", &["function_call"])?;
    let canonical_output = canonical_assistant("canonical_cut", &["function_call_output"])?;
    let metadata = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "canonical_boundary".to_owned(),
        data: serde_json::json!({"kept": true}),
    };
    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(3),
        exclude_tool_calls: false,
    }
    .apply(&[
        canonical_call,
        metadata,
        canonical_output,
        filtered[1].clone(),
    ]);
    assert_eq!(filtered.len(), 2);
    assert!(matches!(filtered[0], SessionEvent::Custom { .. }));
    assert!(matches!(filtered[1], SessionEvent::UserMessage { .. }));
    Ok(())
}
