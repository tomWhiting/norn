//! Provider-facing tool definitions and capabilities.
//!
//! The capability-based projection of registry tools into this surface
//! lives in [`super::surface`] — the single resolution step the provider
//! request, the tool catalog, and the system-prompt tools section all
//! derive from.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::request::ToolDefinition;

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
