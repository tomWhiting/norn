//! Tool definition serialization for the `OpenAI` Responses API.

use crate::provider::request::ToolDefinition;
use crate::provider::tools::{
    HostedToolDefinition, HostedWebSearchTool, ProviderToolDefinition, WebSearchFilters,
    WebSearchUserLocation,
};

/// Serializes a provider tool into the JSON value expected by the
/// Responses API `tools` array.
pub fn serialize_tool(tool: &ProviderToolDefinition) -> serde_json::Value {
    match tool {
        ProviderToolDefinition::Function(function) => serialize_function(function),
        ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(web_search)) => {
            serialize_web_search(web_search)
        }
    }
}

fn serialize_function(tool: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": super::schema_downlevel::downlevel_function_parameters(
            &tool.name,
            &tool.parameters,
        ),
        "strict": false,
    })
}

fn serialize_web_search(tool: &HostedWebSearchTool) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("type".to_owned(), serde_json::json!("web_search"));

    if let Some(size) = &tool.search_context_size {
        map.insert("search_context_size".to_owned(), serde_json::json!(size));
    }
    if let Some(filters) = serialize_filters(tool.filters.as_ref()) {
        map.insert("filters".to_owned(), filters);
    }
    if let Some(location) = serialize_user_location(tool.user_location.as_ref()) {
        map.insert("user_location".to_owned(), location);
    }
    if let Some(external_web_access) = tool.external_web_access {
        map.insert(
            "external_web_access".to_owned(),
            serde_json::json!(external_web_access),
        );
    }
    if let Some(return_token_budget) = &tool.return_token_budget {
        map.insert(
            "return_token_budget".to_owned(),
            return_token_budget.clone(),
        );
    }
    if let Some(content_types) = &tool.search_content_types {
        map.insert(
            "search_content_types".to_owned(),
            serde_json::json!(content_types),
        );
    }

    serde_json::Value::Object(map)
}

fn serialize_filters(filters: Option<&WebSearchFilters>) -> Option<serde_json::Value> {
    let filters = filters?;
    let mut map = serde_json::Map::new();
    if !filters.allowed_domains.is_empty() {
        map.insert(
            "allowed_domains".to_owned(),
            serde_json::json!(&filters.allowed_domains),
        );
    }
    if !filters.blocked_domains.is_empty() {
        map.insert(
            "blocked_domains".to_owned(),
            serde_json::json!(&filters.blocked_domains),
        );
    }
    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    }
}

fn serialize_user_location(location: Option<&WebSearchUserLocation>) -> Option<serde_json::Value> {
    let location = location?;
    let mut map = serde_json::Map::new();
    map.insert(
        "type".to_owned(),
        serde_json::json!(&location.location_type),
    );
    if let Some(city) = &location.city {
        map.insert("city".to_owned(), serde_json::json!(city));
    }
    if let Some(country) = &location.country {
        map.insert("country".to_owned(), serde_json::json!(country));
    }
    if let Some(region) = &location.region {
        map.insert("region".to_owned(), serde_json::json!(region));
    }
    if let Some(timezone) = &location.timezone {
        map.insert("timezone".to_owned(), serde_json::json!(timezone));
    }

    Some(serde_json::Value::Object(map))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::provider::tools::{
        HostedToolDefinition, HostedWebSearchTool, ProviderToolDefinition, WebSearchContextSize,
        WebSearchFilters, WebSearchUserLocation, WebSearchUserLocationType,
    };

    #[test]
    fn function_tool_serialization() {
        let tool = ProviderToolDefinition::Function(ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                }
            }),
        });

        let json = serialize_tool(&tool);
        assert_eq!(json["type"], "function");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["strict"], false);
        assert!(json.get("response_format").is_none());
    }

    /// Composite-tool schemas (root `oneOf`) must be down-leveled before
    /// they reach the wire — OpenAI rejects the whole request otherwise
    /// (regression: HTTP 400 invalid_function_parameters on the `task`
    /// tool).
    #[test]
    fn function_tool_with_one_of_schema_is_downleveled() {
        let tool = ProviderToolDefinition::Function(ToolDefinition {
            name: "task".to_string(),
            description: "Task management".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "description": "Create a thing.",
                        "properties": {
                            "action": { "const": "create" },
                            "name": { "type": "string" }
                        },
                        "required": ["action", "name"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "description": "List all things.",
                        "properties": {
                            "action": { "const": "list" }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                ]
            }),
        });

        let json = serialize_tool(&tool);
        let parameters = &json["parameters"];
        assert_eq!(parameters["type"], "object");
        assert!(parameters.get("oneOf").is_none());
        assert_eq!(
            parameters["properties"]["action"]["enum"],
            serde_json::json!(["create", "list"])
        );
        // Only the discriminator is required by every command; `name` is
        // per-command and moves into the discriminator description.
        assert_eq!(parameters["required"], serde_json::json!(["action"]));
    }

    #[test]
    fn hosted_web_search_serializes_minimal_current_tool_type() {
        let tool = ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(
            HostedWebSearchTool::default(),
        ));

        let json = serialize_tool(&tool);
        assert_eq!(json["type"], "web_search");
        assert!(json.get("name").is_none());
        assert!(json.get("parameters").is_none());
        assert!(json.get("filters").is_none());
        assert!(json.get("user_location").is_none());
    }

    #[test]
    fn hosted_web_search_serializes_configured_controls() {
        let tool =
            ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(HostedWebSearchTool {
                search_context_size: Some(WebSearchContextSize::High),
                filters: Some(WebSearchFilters {
                    allowed_domains: vec!["example.com".to_owned()],
                    blocked_domains: vec!["blocked.example".to_owned()],
                }),
                user_location: Some(WebSearchUserLocation {
                    location_type: WebSearchUserLocationType::Approximate,
                    city: Some("Melbourne".to_owned()),
                    country: Some("AU".to_owned()),
                    region: None,
                    timezone: None,
                }),
                external_web_access: Some(true),
                return_token_budget: Some(serde_json::json!(4096)),
                search_content_types: None,
            }));

        let json = serialize_tool(&tool);
        assert_eq!(json["type"], "web_search");
        assert_eq!(json["search_context_size"], "high");
        assert_eq!(
            json["filters"]["allowed_domains"],
            serde_json::json!(["example.com"])
        );
        assert_eq!(
            json["filters"]["blocked_domains"],
            serde_json::json!(["blocked.example"])
        );
        assert_eq!(json["user_location"]["city"], "Melbourne");
        assert_eq!(json["external_web_access"], true);
        assert_eq!(json["return_token_budget"], 4096);
    }

    #[test]
    fn mixed_tools_serialize_correctly() {
        let func_tool = ProviderToolDefinition::Function(ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({}),
        });
        let web_tool = ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(
            HostedWebSearchTool::default(),
        ));

        let tools: Vec<serde_json::Value> = vec![&func_tool, &web_tool]
            .iter()
            .map(|t| serialize_tool(t))
            .collect();

        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[1]["type"], "web_search");
    }
}
