//! Compact tool assertions for missing evidence, failure/commit separation and safe identity.

use norn::provider::request::ToolCallKind;
use norn::session_view::{DisplayText, ToolState, ToolView};

use super::{state_label, summarize};

fn tool(name: &str) -> ToolView {
    ToolView {
        call_id: Some("call-7".to_owned()),
        stream_item_id: None,
        name: Some(DisplayText::new(name)),
        description: None,
        description_error: None,
        kind: Some(ToolCallKind::Function),
        arguments: None,
        result: None,
        invocation_event: None,
        invocation_attempt: None,
        result_event: None,
        result_parent: None,
        state: ToolState::Running,
        result_state: None,
        duration_ms: None,
        committed: None,
    }
}

#[test]
fn known_and_unknown_calls_keep_missing_description_compact_and_facts_in_details() {
    for name in [
        "edit",
        "write",
        "apply_patch",
        "bash",
        "mcp__server__action",
    ] {
        let view = tool(name);
        let summary = summarize(&view, false);
        assert!(!summary.expanded);
        assert_eq!(summary.call_id, Some("call-7"));
        assert!(summary.description.is_none());
        let header = summary.header();
        assert_eq!(header, format!("{name}: description unavailable · running"));
        let details = summary.details_header();
        for fragment in [
            name,
            "description unavailable",
            "call call-7",
            "running",
            "duration unavailable",
            "commit evidence unavailable",
        ] {
            assert!(details.contains(fragment), "missing {fragment}: {details}");
        }
        assert!(!summary.expanded);
        assert_eq!(summarize(&view, true).header(), details);
    }
}

#[test]
fn compact_description_and_failure_keep_zero_duration_and_reveal_commit_in_details() {
    let mut view = tool("edit");
    view.description = Some(DisplayText::new("Repair the exact selected expression"));
    view.state = ToolState::Failed;
    view.result_state = Some(ToolState::Failed);
    view.committed = Some(true);
    view.duration_ms = Some(0);
    let summary = summarize(&view, false);
    assert_eq!(summary.description, view.description.as_ref());
    assert_eq!(summary.committed, Some(true));
    let header = summary.header();
    assert_eq!(
        header,
        "edit: Repair the exact selected expression · failed · 0ms"
    );
    assert!(
        summary
            .details_header()
            .contains("failed · committed · 0 ms")
    );
    assert_eq!(summarize(&view, true).header(), summary.details_header());
}

#[test]
fn assembling_alias_is_not_presented_as_a_call_id() {
    let mut view = tool("pending");
    view.name = None;
    view.call_id = None;
    view.stream_item_id = Some("stream-only".to_owned());
    view.state = ToolState::Assembling;
    let summary = summarize(&view, false);
    let header = summary.header();
    assert_eq!(
        header,
        "tool name unavailable: description unavailable · assembling"
    );
    let details = summary.details_header();
    assert!(details.contains("call ID unavailable"));
    assert!(!details.contains("stream-only"));
}

#[test]
fn original_description_and_call_controls_cannot_execute_terminal_commands() {
    let mut view = tool("unknown\u{1b}]52;c;payload\u{7}");
    view.description = Some(DisplayText::new("first\nsecond\tcolumn\u{202e}"));
    view.call_id = Some("call\u{1b}[2J\nnext".to_owned());
    let summary = summarize(&view, false);
    assert_eq!(summary.description, view.description.as_ref());
    let header = summary.header();
    assert!(!header.chars().any(char::is_control));
    assert!(!header.contains('\u{202e}'));
    assert!(header.contains("first\\nsecond\\tcolumn"));
    let details = summary.details_header();
    assert!(!details.chars().any(char::is_control));
    assert!(details.contains("\\u{1b}"));
}

#[test]
fn incomplete_invocation_preserves_independent_result_outcome() {
    let mut view = tool("edit");
    view.state = ToolState::Incomplete;
    view.result_state = Some(ToolState::Blocked);
    view.committed = Some(false);
    view.description_error = Some(DisplayText::new("malformed arguments at line 1"));
    let summary = summarize(&view, false);
    let header = summary.header();
    assert_eq!(
        header,
        "edit: description unavailable · incomplete · result blocked"
    );
    let details = summary.details_header();
    assert!(details.contains("incomplete · not committed"));
    assert!(header.contains("result blocked"));
    assert!(details.contains("description error: malformed arguments at line 1"));
}

#[test]
fn long_call_ids_and_unavailable_metadata_do_not_wrap_the_collapsed_description() {
    let mut view = tool("mcp__collaboration__send_message");
    view.description = Some(DisplayText::new("Notify the owner"));
    view.call_id = Some("an-actual-long-call-id-".repeat(12));
    view.description_error = Some(DisplayText::new("recorded parse diagnostic"));
    let summary = summarize(&view, false);
    assert_eq!(
        summary.header(),
        "mcp__collaboration__send_message: Notify the owner · running"
    );
    assert_eq!(summary.call_id, view.call_id.as_deref());
    assert_eq!(summary.description, view.description.as_ref());
    let details = summary.details_header();
    assert!(details.contains("an-actual-long-call-id-"));
    assert!(details.contains("duration unavailable"));
    assert!(details.contains("commit evidence unavailable"));
    assert!(details.contains("description error: recorded parse diagnostic"));
}

#[test]
fn every_observed_lifecycle_has_its_own_label() {
    for (state, label) in [
        (ToolState::Assembling, "assembling"),
        (ToolState::Running, "running"),
        (ToolState::Completed, "completed"),
        (ToolState::Failed, "failed"),
        (ToolState::Blocked, "blocked"),
        (ToolState::Cancelled, "cancelled"),
        (ToolState::Incomplete, "incomplete"),
    ] {
        assert_eq!(state_label(state), label);
    }
}

#[test]
fn supplied_name_always_accompanies_original_description_in_both_views() {
    for name in ["read", "mcp__custom__query"] {
        let mut view = tool(name);
        view.description = Some(DisplayText::new("Read the selected evidence"));
        let summary = summarize(&view, false);
        assert_eq!(summary.name_label(), name);
        assert_eq!(
            summary.header(),
            format!("{name}: Read the selected evidence · running")
        );
        assert!(
            summary
                .details_header()
                .starts_with(&format!("{name} · Read the selected evidence ·"))
        );
        assert_eq!(summary.description, view.description.as_ref());
    }
}
