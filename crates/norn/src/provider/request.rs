//! Provider request types and configuration.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::auth::AuthSource;
use super::tools::ProviderToolDefinition;

/// Model reasoning effort level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// No reasoning.
    None,
    /// Minimal reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Maximum reasoning effort.
    High,
    /// Extended reasoning budget.
    #[serde(rename = "xhigh")]
    XHigh,
}

/// Reasoning summary verbosity level.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    /// Let the model decide summary verbosity.
    #[default]
    Auto,
    /// Brief, minimal summary.
    Concise,
    /// Verbose, thorough summary.
    Detailed,
}

/// A locally-executed function tool definition.
///
/// It contains only what the model needs to know about a tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool identifier used in tool call responses.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// Role of a message in the conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    /// System-level instructions.
    System,
    /// Dynamic developer context injected into the input array.
    Developer,
    /// User input.
    User,
    /// Assistant (model) output.
    Assistant,
    /// Result from a tool call.
    ToolResult,
}

/// Discriminates between the two tool-call surface kinds the `OpenAI` Responses
/// API supports.
///
/// The wire shapes are deliberately different: structured `function_call`
/// items carry a JSON `arguments` string, while freeform `custom_tool_call`
/// items carry an `input` string (no JSON envelope). The output side mirrors
/// the distinction — `function_call_output` versus `custom_tool_call_output`.
/// Carrying the kind through assembly, persistence, and replay lets the
/// serializer choose the correct wire envelope on every echo without re-
/// inspecting the original event payload.
///
/// `Default` is [`ToolCallKind::Function`] so persisted [`ToolCallEvent`] and
/// [`AssistantToolCall`] records that pre-date this field deserialize as
/// function calls — every tool produced before custom tools landed was a
/// `function_call`.
///
/// [`ToolCallEvent`]: crate::session::events::ToolCallEvent
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    /// Structured tool call. Arguments are a JSON string the model emits;
    /// results echo as `function_call_output`.
    #[default]
    Function,
    /// Freeform tool call. Input is an opaque string (not JSON); results
    /// echo as `custom_tool_call_output`.
    Custom,
}

/// A tool call made by the assistant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantToolCall {
    /// Provider-assigned correlation identifier (`call_*`). This is the
    /// only identifier the model accepts on a follow-up
    /// `function_call_output` echo.
    pub call_id: String,
    /// Name of the tool being called.
    pub name: String,
    /// Arguments (`function_call`) or freeform input (`custom_tool_call`),
    /// disambiguated by `kind`.
    pub arguments: String,
    /// Which surface kind this call uses. See [`ToolCallKind`].
    #[serde(default)]
    pub kind: ToolCallKind,
}

/// A message in the conversation history.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// The role of this message's author.
    pub role: MessageRole,
    /// Text content of the message (may be empty for tool-only messages).
    pub content: Option<String>,
    /// Accumulated reasoning/thinking content for Assistant messages. Empty
    /// when the model produced no reasoning. Carried so multi-turn
    /// conversations can echo reasoning back to providers that accept it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    /// Tool calls made by the assistant (only present for Assistant messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AssistantToolCall>,
    /// The tool call ID this result is for (only present for `ToolResult` messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The tool name this result is for (only present for `ToolResult` messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool call kind this result is for (only present for `ToolResult`
    /// messages). [`None`] is treated as [`ToolCallKind::Function`] when the
    /// result is serialized — preserving legacy behaviour for any caller that
    /// has not yet plumbed the kind through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_kind: Option<ToolCallKind>,
}

/// Provider-specific configuration options.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderOptions(pub serde_json::Value);

/// Provider-side context management controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderContextManagement {
    /// Token threshold at which the provider should compact context.
    pub compact_threshold_tokens: u64,
}

/// Normalized request sent to any provider.
///
/// No `output_schema` field: schema enforcement is the agent loop's
/// responsibility via a dynamic schema tool in the `tools` vec.
#[derive(Clone, Debug)]
pub struct ProviderRequest {
    /// Conversation history.
    pub messages: Vec<Message>,
    /// Tool definitions available to the model.
    pub tools: Vec<ProviderToolDefinition>,
    /// Model identifier.
    pub model: String,
    /// Optional reasoning effort control.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Reasoning summary verbosity. Defaults to `Auto` when reasoning is
    /// enabled and this field is `None`.
    pub reasoning_summary: Option<ReasoningSummary>,
    /// Provider-specific options (service tier, etc.).
    pub config: Option<ProviderOptions>,
    /// Cache key for prompt caching. When set, the provider uses this to
    /// enable deterministic caching of the instructions and input prefix.
    pub cache_key: Option<String>,
    /// Previous response ID for conversation chaining. When set, the
    /// provider sends only incremental input items and the API
    /// reconstructs the full conversation from the referenced response.
    pub previous_response_id: Option<String>,
    /// Whether to persist the response on the API server. Required for
    /// `previous_response_id` chaining over HTTP.
    pub store: bool,
    /// Optional provider-side context management policy.
    pub context_management: Option<ProviderContextManagement>,
}

/// A string that redacts its contents in `Debug` and `Display`.
///
/// Prevents credential exposure in logs.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    /// Creates a new secret string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the secret value. Use with care.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Construction-time configuration for a provider instance.
///
/// Not part of the `Provider` trait's `stream` method signature.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// Authentication source. OAuth is the default and recommended
    /// path; the `ApiKey` variant exists for env-gated integration
    /// tests only.
    pub auth_source: AuthSource,
    /// Optional base URL override.
    pub base_url: Option<String>,
    /// Request timeout.
    pub timeout: Duration,
    /// Maximum number of retries on transient failures.
    pub max_retries: u32,
    /// Provider-specific construction options.
    pub provider_options: Option<serde_json::Value>,
    /// JSONL file for writing raw API request/response dumps.
    /// When set, the provider appends structured entries for each call.
    pub debug_dump_file: Option<PathBuf>,
    /// Permits-per-minute granted by the provider's rate limiter.
    ///
    /// `None` falls back to the provider-specific compiled default
    /// (currently `60` for the `OpenAI` backend — see
    /// [`crate::provider::openai::OpenAiProvider`]).
    pub rate_limit: Option<u32>,
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

    #[test]
    fn secret_string_debug_is_redacted() {
        let secret = SecretString::new("my-api-key-12345");
        let debug_output = format!("{secret:?}");
        assert_eq!(debug_output, "[REDACTED]");
        assert!(!debug_output.contains("my-api-key"));
    }

    #[test]
    fn secret_string_display_is_redacted() {
        let secret = SecretString::new("supersecret");
        let display_output = format!("{secret}");
        assert_eq!(display_output, "[REDACTED]");
    }

    #[test]
    fn secret_string_expose_returns_value() {
        let secret = SecretString::new("my-key");
        assert_eq!(secret.expose(), "my-key");
    }

    #[test]
    fn provider_request_has_no_output_schema() {
        let req = ProviderRequest {
            messages: vec![],
            tools: vec![],
            model: "gpt-5".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        assert!(req.tools.is_empty());
    }
}
