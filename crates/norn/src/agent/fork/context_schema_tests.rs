use super::test_support::{
    TestResult, assistant_with_tool_calls, filtered_payload, golden_identity_policy, label,
    tool_result, user_msg,
};
use super::*;

#[test]
fn context_filter_default_keeps_everything() -> TestResult {
    let events = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&[("tc1", "read")]),
        tool_result("tc1", "read"),
    ];
    let filter = ContextFilter::default();
    assert!(filter.is_identity());
    let before = serde_json::to_vec(&events)?;
    let out = filter.apply(&events)?;
    let after = serde_json::to_vec(&out)?;
    assert_eq!(
        after, before,
        "the identity filter must be an exact audit copy"
    );
    Ok(())
}

#[test]
fn context_filter_exclude_tool_calls_drops_tool_results_and_strips_tool_calls() -> TestResult {
    let events = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&[("tc1", "read")]),
        tool_result("tc1", "read"),
    ];
    let filter = ContextFilter {
        include_system: true,
        include_recent_n: None,
        exclude_tool_calls: true,
    };
    assert!(!filter.is_identity());
    let out = filter.apply(&events)?;
    let out = filtered_payload(&out)
        .ok_or_else(|| std::io::Error::other("filtered fork omitted its epoch boundary"))?;
    assert_eq!(out.len(), 2, "tool result should be dropped");
    let SessionEvent::AssistantMessage { tool_calls, .. } = &out[1] else {
        return Err(std::io::Error::other("filtered row was not an assistant message").into());
    };
    assert!(tool_calls.is_empty(), "tool_calls should be stripped");
    Ok(())
}

#[test]
fn context_filter_include_recent_n_truncates_to_last_n() -> TestResult {
    let events: Vec<SessionEvent> = (0..10).map(|i| user_msg(&format!("msg {i}"))).collect();
    let filter = ContextFilter {
        include_system: true,
        include_recent_n: Some(3),
        exclude_tool_calls: false,
    };
    let out = filter.apply(&events)?;
    let out = filtered_payload(&out)
        .ok_or_else(|| std::io::Error::other("filtered fork omitted its epoch boundary"))?;
    assert_eq!(out.len(), 3);
    let SessionEvent::UserMessage { content, .. } = &out[0] else {
        return Err(std::io::Error::other("filtered row was not a user message").into());
    };
    assert_eq!(content, "msg 7");
    Ok(())
}

#[test]
fn context_filter_include_recent_n_trims_leading_orphan_tool_results() -> TestResult {
    let events = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&[("tc1", "bash")]),
        tool_result("tc1", "bash"),
        user_msg("next"),
        assistant_with_tool_calls(&[("tc2", "read")]),
        tool_result("tc2", "read"),
    ];
    let filter = ContextFilter {
        include_system: true,
        include_recent_n: Some(4),
        exclude_tool_calls: false,
    };
    let out = filter.apply(&events)?;
    let out = filtered_payload(&out)
        .ok_or_else(|| std::io::Error::other("filtered fork omitted its epoch boundary"))?;
    assert!(
        !matches!(out.first(), Some(SessionEvent::ToolResult { .. })),
        "leading ToolResult must be trimmed to avoid orphan tool results",
    );
    let SessionEvent::UserMessage { content, .. } = &out[0] else {
        return Err(std::io::Error::other("trim did not expose a user message").into());
    };
    assert_eq!(content, "next");
    Ok(())
}

#[test]
fn context_filter_exclude_system_drops_label_events() -> TestResult {
    let events = vec![user_msg("hi"), label(), user_msg("bye")];
    let filter = ContextFilter {
        include_system: false,
        include_recent_n: None,
        exclude_tool_calls: false,
    };
    let out = filter.apply(&events)?;
    let out = filtered_payload(&out)
        .ok_or_else(|| std::io::Error::other("filtered fork omitted its epoch boundary"))?;
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|e| !matches!(e, SessionEvent::Label { .. })));
    Ok(())
}

#[test]
fn combine_system_instruction_empty_parent_uses_preamble_only() {
    let combined = combine_system_instruction(FORK_SYSTEM_PREAMBLE, "");
    assert_eq!(combined, FORK_SYSTEM_PREAMBLE);
}

#[test]
fn combine_system_instruction_prepends_preamble_to_parent_base() -> TestResult {
    let parent = "You are the parent agent. Be terse.";
    let slugs = vec!["check_code".to_owned()];
    let policy = golden_identity_policy();
    let preamble = build_fork_preamble(&ForkIdentity {
        parent_agent_id: "5e1f0000-0000-0000-0000-000000000001",
        path_address: "root/fork-kestrel",
        requirement_slugs: &slugs,
        granted: &policy,
    });
    let combined = combine_system_instruction(&preamble, parent);
    assert!(
        combined.starts_with(FORK_SYSTEM_PREAMBLE),
        "fork preamble must come first: {combined}",
    );
    assert!(
        combined.contains(parent),
        "parent base must be retained verbatim: {combined}",
    );
    let preamble_position = combined
        .find(&preamble)
        .ok_or_else(|| std::io::Error::other("combined instruction omitted its preamble"))?;
    let parent_position = combined
        .find(parent)
        .ok_or_else(|| std::io::Error::other("combined instruction omitted its parent base"))?;
    assert!(
        preamble_position < parent_position,
        "the whole structured preamble precedes the parent base: {combined}",
    );
    Ok(())
}

/// R4 golden-file test: fixed inputs render byte-for-byte to the
/// checked-in `testdata/fork_preamble.golden.md` — contract slugs,
/// path address, parent id, and the delegation budget all present.
/// A plain file plus `assert_eq!`, no snapshot crate.
#[test]
fn fork_preamble_matches_golden_file() {
    let slugs = vec!["check_code".to_owned(), "run_tests".to_owned()];
    let policy = golden_identity_policy();
    let rendered = build_fork_preamble(&ForkIdentity {
        parent_agent_id: "5e1f0000-0000-0000-0000-000000000001",
        path_address: "root/fork-kestrel",
        requirement_slugs: &slugs,
        granted: &policy,
    });
    assert_eq!(
        rendered,
        include_str!("../testdata/fork_preamble.golden.md"),
        "the fork preamble rendering is pinned by the golden file; \
         update testdata/fork_preamble.golden.md deliberately when the \
         preamble changes",
    );
}

/// R4: a leaf fork (depth 0, messaging none) is told plainly that it
/// cannot delegate and cannot message — the budget is never a
/// surprise discovered at call-rejection time.
#[test]
fn fork_preamble_tells_a_leaf_it_is_a_leaf() {
    use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    let policy = ChildPolicy {
        messaging: MessagingScope::None,
        delegation: DelegationBudget {
            remaining_depth: 0,
            max_concurrent_children: 0,
        },
        inbound_capacity: 1,
        loop_config: None,
    };
    let rendered = build_fork_preamble(&ForkIdentity {
        parent_agent_id: "parent-id",
        path_address: "root/fork-a",
        requirement_slugs: &[],
        granted: &policy,
    });
    assert!(
        rendered.contains("Remaining delegation depth: 0"),
        "{rendered}"
    );
    assert!(rendered.contains("you are a leaf"), "{rendered}");
    assert!(rendered.contains("Messaging scope: none"), "{rendered}");
    assert!(
        rendered.contains("none declared"),
        "an empty contract is stated honestly: {rendered}",
    );
}

#[test]
fn build_fork_output_schema_uses_object_keyed_requirements() -> TestResult {
    let reqs = vec![
        ForkRequirement {
            name: "Summarise the diff".to_string(),
            description: "summarise the diff".to_string(),
        },
        ForkRequirement {
            name: "Check for bugs".to_string(),
            description: "look for common bugs".to_string(),
        },
    ];
    let schema = build_fork_output_schema(&reqs);

    // Top-level required
    let required: Vec<&str> = schema["required"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("schema required field was not an array"))?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(required.contains(&"response"));
    assert!(required.contains(&"requirements"));

    // Requirements is an object, not an array
    assert_eq!(
        schema["properties"]["requirements"]["type"], "object",
        "requirements must be an object schema",
    );

    // Each requirement name is slugified and present as a required key
    let req_required: Vec<&str> = schema["properties"]["requirements"]["required"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("requirements.required field was not an array"))?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(req_required.contains(&"summarise_the_diff"));
    assert!(req_required.contains(&"check_for_bugs"));

    // Each requirement has completed and completion_notes properties
    let props = schema["properties"]["requirements"]["properties"]
        .as_object()
        .ok_or_else(|| std::io::Error::other("requirements properties were not an object"))?;
    assert!(props.contains_key("summarise_the_diff"));
    assert!(props.contains_key("check_for_bugs"));

    let item = &props["summarise_the_diff"];
    assert_eq!(item["type"], "object");
    assert!(item["properties"]["completed"]["type"] == "boolean");
    assert!(item["properties"]["completion_notes"]["type"] == "string");

    // completed and completion_notes are NOT required within each requirement
    assert!(
        item.get("required").is_none() || item["required"].as_array().is_none_or(Vec::is_empty),
        "inner fields must not be required — partial output is valid",
    );
    Ok(())
}

#[test]
fn build_fork_output_schema_validates_well_formed_object_output() -> TestResult {
    let reqs = vec![
        ForkRequirement {
            name: "task a".to_string(),
            description: "first".to_string(),
        },
        ForkRequirement {
            name: "task b".to_string(),
            description: "second".to_string(),
        },
    ];
    let schema = build_fork_output_schema(&reqs);
    let compiled = jsonschema::validator_for(&schema)?;

    // Valid: all requirements present with both fields
    let valid_full = serde_json::json!({
        "response": "done",
        "requirements": {
            "task_a": { "completed": true, "completion_notes": "ok" },
            "task_b": { "completed": false, "completion_notes": "skipped" },
        },
    });
    assert!(compiled.is_valid(&valid_full));

    // Valid: partial output (empty requirement objects — fields not required)
    let valid_partial = serde_json::json!({
        "response": "timed out",
        "requirements": {
            "task_a": {},
            "task_b": { "completed": true },
        },
    });
    assert!(compiled.is_valid(&valid_partial));

    // Invalid: missing requirements entirely
    let invalid_no_reqs = serde_json::json!({"response": "missing requirements"});
    assert!(
        !compiled.is_valid(&invalid_no_reqs),
        "missing required requirements must fail validation",
    );

    // Invalid: missing a required requirement key
    let invalid_missing_key = serde_json::json!({
        "response": "done",
        "requirements": {
            "task_a": { "completed": true },
        },
    });
    assert!(
        !compiled.is_valid(&invalid_missing_key),
        "missing requirement key must fail validation",
    );

    // Invalid: extra requirement key not in schema
    let invalid_extra_key = serde_json::json!({
        "response": "done",
        "requirements": {
            "task_a": { "completed": true },
            "task_b": { "completed": true },
            "task_c": { "completed": true },
        },
    });
    assert!(
        !compiled.is_valid(&invalid_extra_key),
        "extra requirement key must fail validation",
    );
    Ok(())
}
