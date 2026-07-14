//! Provider request types and configuration.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::auth::AuthSource;
use super::tools::ProviderToolDefinition;

/// Model reasoning effort level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// No reasoning.
    None,
    /// Minimal reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// High reasoning effort.
    High,
    /// Extra-high reasoning effort.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Maximum reasoning effort.
    Max,
}

impl ReasoningEffort {
    /// Norn-facing identifier used in config, profiles, and slash commands.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
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

/// Provider service tier requested for a model call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    /// Faster provider execution, when the selected backend/model supports it.
    Fast,
}

impl ServiceTier {
    /// Norn-facing identifier used in config, profiles, and slash commands.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
        }
    }
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
    /// when the model produced no reasoning. Carried for observability and
    /// persistence only: no request serializer reads it — replay goes
    /// through the structured [`reasoning`](Self::reasoning) items, which
    /// carry the `encrypted_content` this plain-text summary does not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    /// Structured reasoning output items captured for Assistant messages
    /// (empty when the model produced none or the provider does not emit
    /// them). The `OpenAI` Responses serializer replays each item that
    /// carries `encrypted_content` ahead of this message's content and
    /// tool calls, so stateless backends (`response_threading: false`)
    /// keep the model's reasoning across tool-call iterations. The
    /// Chat Completions serializer never replays reasoning. Persisted
    /// sessions written before this field existed deserialize to empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning: Vec<crate::provider::reasoning::ReasoningItem>,
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
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderOptions(pub serde_json::Value);

impl std::fmt::Debug for ProviderOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderOptions")
            .field("present", &true)
            .finish_non_exhaustive()
    }
}

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
    /// Optional service-tier control.
    pub service_tier: Option<ServiceTier>,
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
#[derive(Clone)]
pub struct ProviderConfig {
    /// Authentication source. OAuth is the default and recommended
    /// path; the `ApiKey` variant exists for env-gated integration
    /// tests only.
    pub auth_source: AuthSource,
    /// Optional base URL override.
    pub base_url: Option<String>,
    /// Stall deadline applied to each request phase: connection
    /// establishment, the wait for response headers, and the gap
    /// between SSE chunks mid-stream. Deliberately *not* a
    /// whole-request deadline — streamed responses are legitimately
    /// long-lived as long as data keeps arriving.
    pub timeout: Duration,
    /// Maximum number of in-provider retries applied to HTTP `429`
    /// rate-limit responses (with `Retry-After` backoff). It governs
    /// nothing else: other transient failures (timeouts, resets, 5xx)
    /// surface immediately as typed [`ProviderError`]s and are retried
    /// one layer up by the agent loop's
    /// [`RetryPolicy`](crate::agent_loop::retry::RetryPolicy).
    ///
    /// [`ProviderError`]: crate::error::ProviderError
    pub max_retries: u32,
    /// Provider-specific construction options.
    pub provider_options: Option<serde_json::Value>,
    /// JSONL file for writing raw API request/response dumps.
    /// When set, the provider appends structured entries for each call.
    pub debug_dump_file: Option<PathBuf>,
    /// Permits granted per rate-limit interval by the provider's rate
    /// limiter.
    ///
    /// `None` falls back to the provider-specific compiled default
    /// (currently `60` for the `OpenAI` backend — see
    /// [`crate::provider::openai::OpenAiProvider`]).
    pub rate_limit: Option<u32>,
    /// Replenishment window over which [`rate_limit`](Self::rate_limit)
    /// permits are granted.
    ///
    /// `None` falls back to the deliberate, owner-approved default of
    /// 60 seconds (permits-per-minute semantics).
    pub rate_limit_interval: Option<Duration>,
    /// Backoff applied to a `429` response that carries no parseable
    /// `Retry-After` header.
    ///
    /// `None` falls back to the deliberate, owner-approved default of
    /// 1 second.
    pub retry_backoff: Option<Duration>,
    /// Optional ceiling on accepted server-supplied `Retry-After`
    /// waits. When set, any larger server-requested wait is clamped to
    /// this value before it is slept on, imposed on the shared rate
    /// limiter, or surfaced in
    /// [`ProviderError::RateLimited`](crate::error::ProviderError::RateLimited).
    ///
    /// `None` honors the header as-is: there is deliberately no
    /// built-in ceiling, and all arithmetic on the accepted value is
    /// saturating, so absurd values can stall requests against this
    /// provider but can never panic.
    pub retry_after_ceiling: Option<Duration>,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let auth_source = match &self.auth_source {
            AuthSource::OAuth { .. } => "oauth",
            AuthSource::ApiKey { .. } => "api_key",
        };

        f.debug_struct("ProviderConfig")
            .field("auth_source", &auth_source)
            .field("base_url_present", &self.base_url.is_some())
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("provider_options_present", &self.provider_options.is_some())
            .field("debug_dump_file_present", &self.debug_dump_file.is_some())
            .field("rate_limit", &self.rate_limit)
            .field("rate_limit_interval", &self.rate_limit_interval)
            .field("retry_backoff", &self.retry_backoff)
            .field("retry_after_ceiling", &self.retry_after_ceiling)
            .finish()
    }
}

#[cfg(test)]
mod provider_config_security_tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{AuthSource, ProviderConfig, ProviderOptions, SecretString};

    #[test]
    fn debug_output_exposes_only_structural_metadata() {
        let config = ProviderConfig {
            auth_source: AuthSource::ApiKey {
                key: SecretString::new("sentinel-api-key"),
            },
            base_url: Some("https://sentinel-endpoint.example/private".to_owned()),
            timeout: Duration::from_secs(30),
            max_retries: 2,
            provider_options: Some(serde_json::json!({
                "sentinel-option": "sentinel-option-value"
            })),
            debug_dump_file: Some(PathBuf::from("/tmp/sentinel-dump-path")),
            rate_limit: Some(10),
            rate_limit_interval: Some(Duration::from_mins(1)),
            retry_backoff: Some(Duration::from_secs(1)),
            retry_after_ceiling: Some(Duration::from_mins(2)),
        };

        let debug = format!("{config:?}");

        assert!(debug.contains("auth_source: \"api_key\""));
        assert!(debug.contains("base_url_present: true"));
        assert!(debug.contains("provider_options_present: true"));
        assert!(debug.contains("debug_dump_file_present: true"));
        assert!(!debug.contains("sentinel-api-key"));
        assert!(!debug.contains("sentinel-endpoint"));
        assert!(!debug.contains("sentinel-option"));
        assert!(!debug.contains("sentinel-dump-path"));
    }

    #[test]
    fn provider_options_debug_is_structural() {
        let options = ProviderOptions(serde_json::json!({
            "sentinel-option-key": "sentinel-option-value"
        }));

        let rendered = format!("{options:?}");

        assert!(rendered.contains("ProviderOptions"));
        assert!(!rendered.contains("sentinel-option-key"));
        assert!(!rendered.contains("sentinel-option-value"));
    }
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_uses_canonical_wire_names() {
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
        assert_eq!(ReasoningEffort::Max.as_str(), "max");
        assert_eq!(
            serde_json::to_value(ReasoningEffort::XHigh).expect("serialize xhigh"),
            serde_json::json!("xhigh"),
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::Max).expect("serialize max"),
            serde_json::json!("max"),
        );
    }

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
    fn message_without_reasoning_field_deserializes_to_empty() {
        // Persisted sessions written before the structured reasoning field
        // existed must keep deserializing; the field defaults to empty.
        let msg: Message =
            serde_json::from_str(r#"{"role":"Assistant","content":"hi","thinking":"t"}"#)
                .expect("legacy message deserializes");
        assert!(msg.reasoning.is_empty());
    }

    #[test]
    fn message_empty_reasoning_skipped_in_serialization() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: Some("hi".to_owned()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(
            !json.contains("\"reasoning\""),
            "empty reasoning must be skipped: {json}"
        );
    }

    #[test]
    fn message_reasoning_round_trips() {
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
        let msg = Message {
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            reasoning: vec![ReasoningItem {
                id: "rs_1".to_owned(),
                summary: vec![ReasoningSummaryPart::SummaryText {
                    text: "thought".to_owned(),
                }],
                content: None,
                encrypted_content: Some("blob".to_owned()),
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.reasoning, msg.reasoning);
    }

    #[test]
    fn provider_request_has_no_output_schema() {
        let req = ProviderRequest {
            messages: vec![],
            tools: vec![],
            model: "gpt-5".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        assert!(req.tools.is_empty());
    }
}
