use serde_json::{Value, json};

/// All 16 public tool-definition variants and their 18 accepted type literals.
pub(crate) fn public_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "name": "lookup_record",
            "description": "Look up one record.",
            "parameters": {
                "type": "object",
                "properties": {"record_id": {"type": "string"}},
                "required": ["record_id"],
                "additionalProperties": false
            },
            "strict": true,
            "defer_loading": true,
            "allowed_callers": ["direct", "programmatic"]
        }),
        json!({
            "type": "file_search",
            "vector_store_ids": ["vs_inventory"],
            "max_num_results": 4,
            "ranking_options": {"ranker": "auto", "score_threshold": 0.2}
        }),
        json!({"type": "computer"}),
        json!({
            "type": "computer_use_preview",
            "display_height": 768,
            "display_width": 1024,
            "environment": "browser"
        }),
        json!({
            "type": "web_search",
            "search_context_size": "medium",
            "user_location": {
                "type": "approximate",
                "city": "Melbourne",
                "country": "AU",
                "region": "Victoria",
                "timezone": "Australia/Melbourne"
            }
        }),
        json!({"type": "web_search_2025_08_26"}),
        json!({
            "type": "mcp",
            "server_label": "docs",
            "server_url": "https://example.test/mcp",
            "server_description": "Fixture MCP server",
            "require_approval": "never",
            "defer_loading": true,
            "allowed_tools": ["lookup"]
        }),
        json!({
            "type": "code_interpreter",
            "container": "container_inventory",
            "allowed_callers": ["direct"]
        }),
        json!({"type": "programmatic_tool_calling"}),
        json!({
            "type": "image_generation",
            "action": "generate",
            "background": "transparent",
            "output_format": "png",
            "quality": "high",
            "size": "1024x1024"
        }),
        json!({"type": "local_shell"}),
        json!({
            "type": "shell",
            "allowed_callers": ["direct"],
            "environment": {
                "type": "container_auto",
                "file_ids": ["file_inventory"],
                "memory_limit": "4g",
                "network_policy": {"type": "disabled"},
                "skills": [{
                    "type": "skill_reference",
                    "skill_id": "skill_inventory",
                    "version": "latest"
                }]
            }
        }),
        json!({
            "type": "custom",
            "name": "freeform_lookup",
            "description": "Accept free-form lookup text.",
            "defer_loading": true,
            "format": {"type": "text"}
        }),
        json!({
            "type": "namespace",
            "name": "inventory",
            "description": "Inventory operations.",
            "tools": [{
                "type": "function",
                "name": "read",
                "description": "Read an inventory item.",
                "parameters": {"type": "object", "properties": {}},
                "strict": true
            }]
        }),
        json!({
            "type": "tool_search",
            "execution": "server",
            "description": "Find deferred tools."
        }),
        json!({
            "type": "web_search_preview",
            "search_content_types": ["text"],
            "search_context_size": "low"
        }),
        json!({"type": "web_search_preview_2025_03_11"}),
        json!({"type": "apply_patch", "allowed_callers": ["direct"]}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_covers_every_public_tool_definition_type_literal_in_order() {
        let tools = public_tool_definitions();
        let actual = tools
            .iter()
            .filter_map(|tool| tool.get("type").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                "function",
                "file_search",
                "computer",
                "computer_use_preview",
                "web_search",
                "web_search_2025_08_26",
                "mcp",
                "code_interpreter",
                "programmatic_tool_calling",
                "image_generation",
                "local_shell",
                "shell",
                "custom",
                "namespace",
                "tool_search",
                "web_search_preview",
                "web_search_preview_2025_03_11",
                "apply_patch",
            ]
        );
    }
}
