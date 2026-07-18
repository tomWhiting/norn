use serde_json::Value;

use super::{
    historical_replay_items, historical_shape_matrix_items, public_output_item_inventory,
    spawn_lifecycle_items, spawn_shape_matrix_items,
};
use crate::provider::openai::response_contract::{
    OutputItemActionability, OutputItemRepresentation, PUBLIC_OUTPUT_ITEMS,
};
use crate::provider::response_item::ResponseItem;

fn item_types(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| item.get("type").and_then(Value::as_str))
        .collect()
}

#[test]
fn fixture_covers_public_output_manifest_exactly_in_order() {
    let inventory = public_output_item_inventory("manifest", "manifest text");
    let expected = PUBLIC_OUTPUT_ITEMS
        .iter()
        .map(|entry| entry.name())
        .collect::<Vec<_>>();
    assert_eq!(item_types(&inventory), expected);
}

#[test]
fn fixture_and_parser_agree_on_every_manifest_representation()
-> Result<(), Box<dyn std::error::Error>> {
    let inventory = public_output_item_inventory("parser", "parser text");
    assert_eq!(inventory.len(), PUBLIC_OUTPUT_ITEMS.len());
    for (raw, entry) in inventory.into_iter().zip(PUBLIC_OUTPUT_ITEMS) {
        let item = ResponseItem::from_value(raw)?;
        let representation = match &item {
            ResponseItem::Known(_) => OutputItemRepresentation::KnownOpaque,
            ResponseItem::Message(_)
            | ResponseItem::Reasoning(_)
            | ResponseItem::FunctionCall(_)
            | ResponseItem::CustomToolCall(_)
            | ResponseItem::WebSearchCall(_)
            | ResponseItem::Compaction(_) => OutputItemRepresentation::TypedCore,
            ResponseItem::Opaque(_) => {
                return Err(format!("public item {} parsed as unknown", entry.name()).into());
            }
        };
        assert_eq!(item.item_type(), entry.name());
        assert_eq!(representation, entry.representation(), "{}", entry.name());
    }
    Ok(())
}

#[test]
fn manifest_actionability_sets_are_pinned_next_to_schema_complete_fixtures() {
    let names = |actionability| {
        PUBLIC_OUTPUT_ITEMS
            .iter()
            .filter(|entry| entry.actionability() == actionability)
            .map(|entry| entry.name())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(OutputItemActionability::Executable),
        [
            "function_call",
            "computer_call",
            "local_shell_call",
            "apply_patch_call",
            "mcp_approval_request",
            "custom_tool_call",
        ]
    );
    assert_eq!(
        names(OutputItemActionability::Conditional),
        ["tool_search_call", "shell_call"]
    );
    assert_eq!(names(OutputItemActionability::Inert).len(), 20);
}

#[test]
fn lifecycle_fixture_excludes_only_unresolved_client_execution() {
    let items = spawn_lifecycle_items("lifecycle", "lifecycle text");
    assert_eq!(
        item_types(&items),
        [
            "message",
            "file_search_call",
            "function_call_output",
            "web_search_call",
            "computer_call_output",
            "reasoning",
            "program",
            "program_output",
            "tool_search_call",
            "tool_search_output",
            "additional_tools",
            "compaction",
            "image_generation_call",
            "code_interpreter_call",
            "local_shell_call_output",
            "shell_call",
            "shell_call_output",
            "apply_patch_call_output",
            "mcp_call",
            "mcp_list_tools",
            "mcp_approval_response",
            "custom_tool_call_output",
        ]
    );
}

#[test]
fn historical_fixture_adds_resolved_function_and_custom_call_pairs() {
    let items = historical_replay_items("history", "history text");
    assert_eq!(items.len(), 24);
    for pair in [
        ["function_call", "function_call_output"],
        ["custom_tool_call", "custom_tool_call_output"],
    ] {
        assert!(
            pair.iter()
                .all(|expected| item_types(&items).contains(expected))
        );
    }
}

#[test]
fn lifecycle_shape_matrices_pair_populated_and_optional_absent_items() {
    let spawn = spawn_shape_matrix_items("shape_spawn", "shape spawn");
    let history = historical_shape_matrix_items("shape_history", "shape history");
    assert_eq!(spawn.len(), 48);
    assert_eq!(history.len(), 52);
    let spawn_types = item_types(&spawn);
    let history_types = item_types(&history);
    let expected_spawn_counts = [
        2, 2, 0, 2, 5, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0, 2, 1, 4, 0, 2, 2, 2, 0, 2, 0, 2,
    ];
    let expected_history_counts = [
        2, 2, 2, 2, 5, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0, 2, 1, 4, 0, 2, 2, 2, 0, 2, 2, 2,
    ];

    for ((entry, expected_spawn), expected_history) in PUBLIC_OUTPUT_ITEMS
        .iter()
        .zip(expected_spawn_counts)
        .zip(expected_history_counts)
    {
        let spawn_count = spawn_types
            .iter()
            .filter(|item_type| **item_type == entry.name())
            .count();
        let history_count = history_types
            .iter()
            .filter(|item_type| **item_type == entry.name())
            .count();
        assert_eq!(spawn_count, expected_spawn, "{}", entry.name());
        assert_eq!(history_count, expected_history, "{}", entry.name());
    }
}

#[test]
fn fixture_pins_nested_multimodal_and_annotation_unions() {
    let inventory = public_output_item_inventory("nested", "nested text");
    let message = &inventory[0];
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    assert_eq!(item_types(content), ["output_text", "refusal"]);
    let annotations = content
        .first()
        .and_then(|part| part.get("annotations"))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    assert_eq!(
        item_types(annotations),
        [
            "file_citation",
            "url_citation",
            "container_file_citation",
            "file_path",
        ]
    );

    for output_index in [3, 27] {
        let parts = inventory[output_index]
            .get("output")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        assert_eq!(
            item_types(parts),
            ["input_text", "input_image", "input_file"]
        );
    }
    let code_outputs = inventory[15]
        .get("outputs")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    assert_eq!(item_types(code_outputs), ["logs", "image"]);
    assert_eq!(
        inventory[6].pointer("/output/type").and_then(Value::as_str),
        Some("computer_screenshot")
    );
}

#[test]
fn audio_is_a_response_scoped_event_family_not_an_output_item() {
    let inventory = public_output_item_inventory("no_audio", "no audio");
    assert!(
        item_types(&inventory)
            .iter()
            .all(|item_type| !item_type.contains("audio"))
    );
    assert!(
        PUBLIC_OUTPUT_ITEMS
            .iter()
            .all(|entry| !entry.name().contains("audio"))
    );
}
