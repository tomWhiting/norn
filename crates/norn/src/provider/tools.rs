//! Provider-facing tool definitions and capability-based projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::request::ToolDefinition;
use crate::tools::web::WEB_SEARCH_TOOL_NAME;

/// Provider capabilities that affect request construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Provider can execute web search as a hosted tool.
    pub hosted_web_search: bool,
    /// Provider can continue a conversation from a previous response ID.
    pub response_threading: bool,
    /// Provider accepts server-side context-management compaction controls.
    pub server_compaction: bool,
}

impl ProviderCapabilities {
    /// Capabilities exposed by the `OpenAI` Responses API.
    #[must_use]
    pub const fn openai_responses() -> Self {
        Self {
            hosted_web_search: true,
            response_threading: true,
            server_compaction: true,
        }
    }
}

/// Tool definition as seen by an LLM provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "definition", rename_all = "snake_case")]
pub enum ProviderToolDefinition {
    /// Locally-executed function tool.
    Function(ToolDefinition),
    /// Provider-hosted tool.
    Hosted(HostedToolDefinition),
}

impl From<ToolDefinition> for ProviderToolDefinition {
    fn from(tool: ToolDefinition) -> Self {
        Self::Function(tool)
    }
}

/// Hosted tools available through provider-native APIs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
pub enum HostedToolDefinition {
    /// Hosted web search.
    WebSearch(HostedWebSearchTool),
}

/// Hosted web-search controls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostedWebSearchTool {
    /// Search context size requested from the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<WebSearchContextSize>,
    /// Domain filters for provider-hosted search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<WebSearchFilters>,
    /// Approximate user location for localized search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<WebSearchUserLocation>,
    /// Whether the provider may use live external web access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_web_access: Option<bool>,
    /// Provider-specific return token budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_token_budget: Option<Value>,
    /// Search content types requested from the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_content_types: Option<Vec<WebSearchContentType>>,
}

/// Hosted web-search context size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchContextSize {
    /// Low context budget.
    Low,
    /// Medium context budget.
    Medium,
    /// High context budget.
    High,
}

/// Domain filters for hosted web search.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchFilters {
    /// Domains the provider may search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    /// Domains the provider must not search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

/// User location for hosted web search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchUserLocation {
    /// Location approximation type.
    #[serde(rename = "type")]
    pub location_type: WebSearchUserLocationType,
    /// Approximate city.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Approximate country.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Approximate region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Approximate timezone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Hosted web-search user location precision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchUserLocationType {
    /// Approximate location only.
    Approximate,
}

/// Hosted web-search content type filter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchContentType {
    /// Search page text.
    Text,
    /// Search page image content.
    Image,
}

/// Projects local runtime tools into the provider-facing tool surface.
#[must_use]
pub fn resolve_provider_tools(
    tools: &[ToolDefinition],
    capabilities: ProviderCapabilities,
) -> Vec<ProviderToolDefinition> {
    tools
        .iter()
        .cloned()
        .map(|tool| {
            if capabilities.hosted_web_search && tool.name == WEB_SEARCH_TOOL_NAME {
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(
                    HostedWebSearchTool::default(),
                ))
            } else {
                ProviderToolDefinition::Function(tool)
            }
        })
        .collect()
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

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: "tool".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn keeps_web_search_as_function_without_provider_capability() {
        let resolved = resolve_provider_tools(
            &[tool(WEB_SEARCH_TOOL_NAME)],
            ProviderCapabilities::default(),
        );
        assert!(matches!(
            resolved.as_slice(),
            [ProviderToolDefinition::Function(function)] if function.name == WEB_SEARCH_TOOL_NAME
        ));
    }

    #[test]
    fn converts_web_search_to_hosted_when_provider_supports_it() {
        let resolved = resolve_provider_tools(
            &[tool("read_file"), tool(WEB_SEARCH_TOOL_NAME)],
            ProviderCapabilities::openai_responses(),
        );

        assert!(matches!(
            resolved.as_slice(),
            [
                ProviderToolDefinition::Function(function),
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_)),
            ] if function.name == "read_file"
        ));
    }
}
