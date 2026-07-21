use super::test_support::{TestResult, assistant_with_tool_calls, tool_result, user_msg};
use super::*;

#[test]
fn slugify_requirement_name_basic_cases() {
    assert_eq!(
        slugify_requirement_name("Summarise the diff"),
        "summarise_the_diff"
    );
    assert_eq!(slugify_requirement_name("Check for bugs"), "check_for_bugs");
    assert_eq!(slugify_requirement_name("simple"), "simple");
    assert_eq!(slugify_requirement_name("ALL CAPS"), "all_caps");
}

#[test]
fn slugify_requirement_name_special_characters() {
    assert_eq!(slugify_requirement_name("foo--bar"), "foo_bar");
    assert_eq!(
        slugify_requirement_name("  leading spaces  "),
        "leading_spaces"
    );
    assert_eq!(slugify_requirement_name("a!@#$%b"), "a_b");
    assert_eq!(slugify_requirement_name("CamelCase123"), "camelcase123");
}

#[test]
fn format_fork_result_includes_system_notice_and_envelope() {
    let fork_id = Uuid::new_v4();
    let reqs = serde_json::json!({
        "check_code": { "completed": true, "completion_notes": "all good" },
        "run_tests": { "completed": false, "completion_notes": "timed out" },
    });
    let result = format_fork_result(fork_id, "Summary text", &reqs);
    let short_id = &fork_id.to_string()[..8];

    assert!(
        result.contains("automatically delivered by fork"),
        "system notice: {result:?}"
    );
    assert!(
        result.contains("This is not user input"),
        "user-input disclaimer: {result:?}"
    );
    assert!(result.contains(&format!("FORK RESULT ({short_id})")));
    assert!(result.contains("Summary text"));
    assert!(result.contains("**check_code** _completed_"));
    assert!(result.contains("all good"));
    assert!(result.contains("**run_tests** _not completed_"));
    assert!(result.contains("timed out"));
    assert!(result.contains("END FORK RESULT"));
}

#[test]
fn format_fork_result_handles_missing_fields() {
    let fork_id = Uuid::new_v4();
    let reqs = serde_json::json!({
        "partial": {},
    });
    let result = format_fork_result(fork_id, "partial output", &reqs);
    assert!(result.contains("**partial** _not completed_"));
    assert!(!result.contains('\0'));
}

#[test]
fn format_fork_result_handles_non_object_requirements() {
    let fork_id = Uuid::new_v4();
    let reqs = serde_json::json!("not an object");
    let result = format_fork_result(fork_id, "response", &reqs);
    assert!(result.contains("response"));
    assert!(result.contains("END FORK RESULT"));
    assert!(
        !result.contains("_completed_"),
        "no requirement lines for non-object"
    );
}

#[test]
fn format_fork_failure_includes_system_notice_and_error() {
    let fork_id = Uuid::new_v4();
    let names = vec!["check code".to_string(), "run tests".to_string()];
    let result = format_fork_failure(fork_id, "context window exceeded", &names);
    let short_id = &fork_id.to_string()[..8];

    assert!(
        result.contains("automatically delivered by fork"),
        "system notice: {result:?}"
    );
    assert!(result.contains(&format!("FORK FAILED ({short_id})")));
    assert!(result.contains("context window exceeded"));
    assert!(result.contains("Requirements were: check code, run tests"));
    assert!(result.contains("END FORK RESULT"));
}

#[test]
fn format_fork_failure_empty_requirements() {
    let fork_id = Uuid::new_v4();
    let result = format_fork_failure(fork_id, "error", &[]);
    assert!(!result.contains("Requirements were"));
    assert!(result.contains("END FORK RESULT"));
}

#[test]
fn format_spawn_result_includes_system_notice_and_output() {
    let agent_id = Uuid::new_v4();
    let result = format_spawn_result(agent_id, "reviewer", "Looks good");
    let short_id = &agent_id.to_string()[..8];

    assert!(
        result.contains("automatically delivered by reviewer"),
        "system notice: {result:?}"
    );
    assert!(result.contains(&format!("AGENT RESULT (reviewer {short_id})")));
    assert!(result.contains("Looks good"));
    assert!(result.contains("END AGENT RESULT"));
}

#[test]
fn format_spawn_failure_includes_system_notice_and_error() {
    let agent_id = Uuid::new_v4();
    let result = format_spawn_failure(agent_id, "reviewer", "timed out");
    let short_id = &agent_id.to_string()[..8];

    assert!(
        result.contains("automatically delivered by reviewer"),
        "system notice: {result:?}"
    );
    assert!(result.contains(&format!("AGENT FAILED (reviewer {short_id})")));
    assert!(result.contains("timed out"));
    assert!(result.contains("END AGENT RESULT"));
}

#[test]
fn inject_synthetic_fork_result_appends_tool_result_with_fork_name() -> TestResult {
    let fork_id = Uuid::new_v4();
    let events = vec![
        user_msg("go"),
        assistant_with_tool_calls(&[("tc-fork", "fork")]),
    ];
    let out = inject_synthetic_fork_result(events, "tc-fork", fork_id);
    let injected = out
        .last()
        .ok_or_else(|| std::io::Error::other("synthetic result produced no events"))?;
    let SessionEvent::ToolResult {
        tool_call_id,
        tool_name,
        output,
        ..
    } = injected
    else {
        return Err(std::io::Error::other(format!(
            "synthetic result was not a ToolResult: {injected:?}"
        ))
        .into());
    };
    assert_eq!(tool_call_id, "tc-fork");
    assert_eq!(tool_name, "fork");
    // Pinned vocabulary: the synthetic child-side result uses
    // the same `agent_id` field as the parent-side fork tool
    // output — never `fork_id`.
    assert_eq!(output["agent_id"], fork_id.to_string());
    assert!(
        output.get("fork_id").is_none(),
        "legacy fork_id field must not reappear",
    );
    assert_eq!(output["status"], "active");
    assert_eq!(output["message"], FORK_SYNTHETIC_RESULT_MESSAGE);
    Ok(())
}

#[test]
fn verify_no_orphan_tool_calls_returns_empty_when_all_results_present() {
    let events = vec![
        user_msg("go"),
        assistant_with_tool_calls(&[("tc-read", "read"), ("tc-fork", "fork")]),
        tool_result("tc-read", "read"),
    ];
    let orphans = verify_no_orphan_tool_calls(&events, "tc-fork");
    assert!(orphans.is_empty());
}

#[test]
fn verify_no_orphan_tool_calls_flags_missing_results() {
    let events = vec![
        user_msg("go"),
        assistant_with_tool_calls(&[
            ("tc-read", "read"),
            ("tc-search", "search"),
            ("tc-fork", "fork"),
        ]),
        // Only the `read` result has been appended so far.
        tool_result("tc-read", "read"),
    ];
    let orphans = verify_no_orphan_tool_calls(&events, "tc-fork");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, "tc-search");
    assert_eq!(orphans[0].name, "search");
}

#[test]
fn verify_no_orphan_tool_calls_empty_events_returns_empty() {
    let orphans = verify_no_orphan_tool_calls(&[], "tc-fork");
    assert!(orphans.is_empty());
}

#[test]
fn parent_system_instruction_roundtrips() {
    let ext = ParentSystemInstruction::new("be brief");
    assert_eq!(ext.as_str(), "be brief");
}
