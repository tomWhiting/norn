//! Integration tests for the `tool_follow_ups!` function-like macro.
//!
//! Each test drives the macro through the public `norn::tool` re-exports,
//! invokes the generated closure against a real `ToolOutput`, and asserts the
//! resulting `Vec<FollowUpAction>` matches the live runtime types defined by
//! NTF-001 (`norn::tool::follow_up`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use norn::tool::{ExpiryCondition, ToolOutput, tool_follow_ups};

/// Builds a `ToolOutput` carrying the given JSON content.
fn output(content: serde_json::Value) -> ToolOutput {
    ToolOutput::success(content)
}

#[test]
fn single_action_with_true_condition_produces_one() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Undo the edit",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].action, "undo");
    assert_eq!(result[0].description, "Undo the edit");
    assert_eq!(result[0].tool, "EditTool");
}

#[test]
fn false_condition_filters_action_out() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "kept",
                description: "Kept action",
                when: |output| output.content["keep"].as_bool().unwrap_or(false),
                expires: Never,
                overrides: {},
            },
            {
                name: "dropped",
                description: "Dropped action",
                when: |_output| false,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({ "keep": true })));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].action, "kept");
}

#[test]
fn single_placeholder_interpolates() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Revert {path} to pre-edit content",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({ "path": "src/lib.rs" })));
    assert_eq!(
        result[0].description,
        "Revert src/lib.rs to pre-edit content"
    );
}

#[test]
fn multiple_placeholders_interpolate() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "apply",
                description: "Apply {mode} edit at {path}",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({
        "mode": "structural",
        "path": "src/handler.rs",
    })));
    assert_eq!(
        result[0].description,
        "Apply structural edit at src/handler.rs"
    );
}

#[test]
fn missing_placeholder_field_renders_missing_sentinel() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Revert {path}",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(result[0].description, "Revert <missing>");
}

#[test]
fn description_without_placeholders_passes_through() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Plain description, no placeholders",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(result[0].description, "Plain description, no placeholders");
}

#[test]
fn file_modified_shorthand_captures_path_and_hash() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Undo",
                when: |_output| true,
                expires: FileModified(
                    output.content["path"].as_str().unwrap_or(""),
                    output.content["content_hash"].as_str().unwrap_or(""),
                ),
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({
        "path": "src/x.rs",
        "content_hash": "abc123",
    })));
    match &result[0].expires {
        ExpiryCondition::FileModified { path, content_hash } => {
            assert_eq!(path, std::path::Path::new("src/x.rs"));
            assert_eq!(content_hash, "abc123");
        }
        other => panic!("expected FileModified, got {other:?}"),
    }
}

#[test]
fn any_file_modified_shorthand_captures_files() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Undo",
                when: |_output| true,
                expires: AnyFileModified(
                    output.content["files"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(path, hash)| (path.clone(), hash.as_str().unwrap_or("")))
                        .collect::<Vec<(String, &str)>>()
                ),
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({
        "files": { "a.rs": "h1", "b.rs": "h2" },
    })));
    match &result[0].expires {
        ExpiryCondition::AnyFileModified { files } => {
            assert_eq!(files.len(), 2);
            assert_eq!(
                files.get(std::path::Path::new("a.rs")).map(String::as_str),
                Some("h1")
            );
            assert_eq!(
                files.get(std::path::Path::new("b.rs")).map(String::as_str),
                Some("h2")
            );
        }
        other => panic!("expected AnyFileModified, got {other:?}"),
    }
}

#[test]
fn turn_scoped_shorthand_captures_turn_id() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Undo",
                when: |_output| true,
                expires: TurnScoped(output.content["turn_id"].as_str().unwrap_or("")),
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({ "turn_id": "turn-7" })));
    match &result[0].expires {
        ExpiryCondition::TurnScoped { turn_id } => assert_eq!(turn_id, "turn-7"),
        other => panic!("expected TurnScoped, got {other:?}"),
    }
}

#[test]
fn never_shorthand_produces_variant() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "undo",
                description: "Undo",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert!(matches!(&result[0].expires, ExpiryCondition::Never));
}

#[test]
fn overrides_produce_expected_json() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "structural",
                description: "Structural apply",
                when: |_output| true,
                expires: Never,
                overrides: { "mode": "structural" },
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(result[0].args, serde_json::json!({ "mode": "structural" }));
}

#[test]
fn empty_overrides_produce_empty_object() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "noop",
                description: "No overrides",
                when: |_output| true,
                expires: Never,
                overrides: {},
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(result[0].args, serde_json::json!({}));
}

#[test]
fn nested_overrides_preserve_structure() {
    let registrations = tool_follow_ups! {
        tool: EditTool,
        actions: [
            {
                name: "flagged",
                description: "Nested overrides",
                when: |_output| true,
                expires: Never,
                overrides: { "flags": ["allow_broken_ast"] },
            },
        ],
    };

    let result = registrations(&output(serde_json::json!({})));
    assert_eq!(
        result[0].args,
        serde_json::json!({ "flags": ["allow_broken_ast"] })
    );
}
