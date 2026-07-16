use std::collections::BTreeSet;

use serde_json::{Value, json};

fn discriminators(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .filter_map(|value| value.get("type").and_then(Value::as_str))
        .collect()
}

fn key_set(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .into_iter()
        .flat_map(serde_json::Map::keys)
        .map(String::as_str)
        .collect()
}

fn computer_actions() -> Vec<Value> {
    vec![
        json!({"type": "click", "button": "left", "x": 10, "y": 20, "keys": ["SHIFT"]}),
        json!({"type": "double_click", "keys": null, "x": 11, "y": 21}),
        json!({
            "type": "drag",
            "path": [{"x": 10, "y": 20}, {"x": 30, "y": 40}],
            "keys": null
        }),
        json!({"type": "keypress", "keys": ["CTRL", "A"]}),
        json!({"type": "move", "x": 12, "y": 22, "keys": []}),
        json!({"type": "screenshot"}),
        json!({
            "type": "scroll",
            "scroll_x": 0,
            "scroll_y": 480,
            "x": 13,
            "y": 23,
            "keys": null
        }),
        json!({"type": "type", "text": "inventory"}),
        json!({"type": "wait"}),
    ]
}

fn patch_operations() -> Vec<Value> {
    vec![
        json!({
            "type": "create_file",
            "path": "created.txt",
            "diff": "@@\n+created"
        }),
        json!({"type": "delete_file", "path": "deleted.txt"}),
        json!({
            "type": "update_file",
            "path": "updated.txt",
            "diff": "@@\n-old\n+new"
        }),
    ]
}

fn web_actions() -> Vec<Value> {
    vec![
        json!({
            "type": "search",
            "query": "inventory",
            "queries": ["inventory", "fixture"],
            "sources": [{"type": "url", "url": "https://example.test/inventory"}]
        }),
        json!({"type": "open_page", "url": "https://example.test/inventory"}),
        json!({
            "type": "find_in_page",
            "pattern": "fixture",
            "url": "https://example.test/inventory"
        }),
    ]
}

fn shell_outcomes() -> Vec<Value> {
    vec![
        json!({"type": "exit", "exit_code": 0}),
        json!({"type": "timeout"}),
    ]
}

fn caller_shapes() -> Vec<Value> {
    vec![
        json!({"type": "direct"}),
        json!({"type": "program", "caller_id": "call_program"}),
    ]
}

pub(super) fn nested_output_item_matrix(id_suffix: &str) -> Vec<Value> {
    let mut items = Vec::new();
    for (index, action) in computer_actions().into_iter().enumerate() {
        items.push(json!({
            "type": "computer_call",
            "id": format!("cc_{id_suffix}_{index}"),
            "call_id": format!("call_cc_{id_suffix}_{index}"),
            "pending_safety_checks": [],
            "status": "completed",
            "action": action
        }));
    }
    for (index, operation) in patch_operations().into_iter().enumerate() {
        items.push(json!({
            "type": "apply_patch_call",
            "id": format!("ap_{id_suffix}_{index}"),
            "call_id": format!("call_ap_{id_suffix}_{index}"),
            "operation": operation,
            "status": "completed",
            "caller": {"type": "direct"}
        }));
    }
    for (index, action) in web_actions().into_iter().enumerate() {
        items.push(json!({
            "type": "web_search_call",
            "id": format!("ws_{id_suffix}_{index}"),
            "status": "completed",
            "action": action
        }));
    }
    for (index, (outcome, caller)) in shell_outcomes()
        .into_iter()
        .zip(caller_shapes())
        .enumerate()
    {
        items.push(json!({
            "type": "shell_call_output",
            "id": format!("sco_{id_suffix}_{index}"),
            "call_id": format!("call_sc_{id_suffix}_{index}"),
            "max_output_length": 4096,
            "output": [{
                "outcome": outcome,
                "stderr": "",
                "stdout": format!("nested outcome {index}\n")
            }],
            "status": "completed",
            "caller": caller
        }));
    }
    items
}

#[test]
fn computer_action_union_covers_all_nine_official_variants_and_fields() {
    let actions = computer_actions();
    assert_eq!(
        discriminators(&actions),
        [
            "click",
            "double_click",
            "drag",
            "keypress",
            "move",
            "screenshot",
            "scroll",
            "type",
            "wait",
        ]
    );
    let expected_keys = [
        &["button", "keys", "type", "x", "y"][..],
        &["keys", "type", "x", "y"],
        &["keys", "path", "type"],
        &["keys", "type"],
        &["keys", "type", "x", "y"],
        &["type"],
        &["keys", "scroll_x", "scroll_y", "type", "x", "y"],
        &["text", "type"],
        &["type"],
    ];
    for (action, expected) in actions.iter().zip(expected_keys) {
        assert_eq!(key_set(action), expected.iter().copied().collect());
    }
}

#[test]
fn patch_web_outcome_and_caller_unions_are_exhaustive() {
    assert_eq!(
        discriminators(&patch_operations()),
        ["create_file", "delete_file", "update_file"]
    );
    assert_eq!(
        discriminators(&web_actions()),
        ["search", "open_page", "find_in_page"]
    );
    assert_eq!(discriminators(&shell_outcomes()), ["exit", "timeout"]);
    assert_eq!(discriminators(&caller_shapes()), ["direct", "program"]);

    assert_eq!(
        key_set(&patch_operations()[0]),
        ["diff", "path", "type"].into_iter().collect()
    );
    assert_eq!(
        key_set(&patch_operations()[1]),
        ["path", "type"].into_iter().collect()
    );
    assert_eq!(
        key_set(&web_actions()[2]),
        ["pattern", "type", "url"].into_iter().collect()
    );
    assert_eq!(
        key_set(&shell_outcomes()[0]),
        ["exit_code", "type"].into_iter().collect()
    );
    assert_eq!(
        key_set(&caller_shapes()[1]),
        ["caller_id", "type"].into_iter().collect()
    );
}

#[test]
fn nested_output_matrix_contains_every_isolated_union_variant() {
    let matrix = nested_output_item_matrix("matrix");
    assert_eq!(matrix.len(), 17);
    assert_eq!(
        discriminators(&matrix),
        [
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "computer_call",
            "apply_patch_call",
            "apply_patch_call",
            "apply_patch_call",
            "web_search_call",
            "web_search_call",
            "web_search_call",
            "shell_call_output",
            "shell_call_output",
        ]
    );
}
