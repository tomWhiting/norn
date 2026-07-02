//! Request serialization for the `OpenAI` Responses API.

use serde::Serialize;

use super::tools::serialize_tool;
use crate::error::ProviderError;
use crate::provider::request::{
    Message, MessageRole, ProviderOptions, ProviderRequest, ReasoningEffort, ReasoningSummary,
    ToolCallKind,
};

/// Catalog provider identifier every `OpenAI` Responses connection
/// resolves against.
pub(super) const CATALOG_PROVIDER: &str = "openai";
/// Catalog backend identifier for OAuth connections against the compiled
/// `ChatGPT` base URL (the Codex subscription backend).
pub(super) const CATALOG_BACKEND_CODEX_SUBSCRIPTION: &str = "codex_subscription";
/// Catalog backend identifier for direct Responses API connections
/// (API-key auth or an explicit base URL).
pub(super) const CATALOG_BACKEND_RESPONSES_API: &str = "responses_api";

/// Serialized payload for `POST /v1/responses`.
#[derive(Debug, Serialize)]
pub(crate) struct ResponsesApiPayload {
    /// Model identifier.
    pub model: String,
    /// System instructions (extracted from system messages).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// Conversation input items.
    pub input: Vec<serde_json::Value>,
    /// Tool definitions.
    pub tools: Vec<serde_json::Value>,
    /// Tool selection policy.
    pub tool_choice: String,
    /// Whether the model may issue parallel tool calls.
    pub parallel_tool_calls: bool,
    /// Always true — SSE streaming.
    pub stream: bool,
    /// Whether the API should persist this response for chaining.
    pub store: bool,
    /// Optional provider service tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Server-side reasoning content to include in the response.
    pub include: Vec<String>,
    /// Optional reasoning effort control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningParam>,
    /// Deterministic prompt cache key for consistent cache hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Previous response ID for conversation chaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Cache retention policy (`in_memory` or `24h`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<String>,
    /// Provider-side context management controls.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_management: Vec<ContextManagementItem>,
}

/// The `reasoning` object in the Responses API request.
#[derive(Debug, Serialize)]
pub(crate) struct ReasoningParam {
    /// Reasoning effort level. Omitted when the caller wants to accept
    /// the server's default for the chosen model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Summary verbosity level.
    pub summary: ReasoningSummary,
}

/// One `context_management` item for the Responses API.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContextManagementItem {
    /// Server-side compaction using an absolute rendered-token threshold.
    Compaction {
        /// Rendered-token threshold at which compaction should run.
        compact_threshold: u64,
    },
}

/// Builds the API payload from a `ProviderRequest`.
///
/// `catalog_backend` is the model-catalog backend identifier of the
/// connection the calling provider actually uses
/// ([`CATALOG_BACKEND_CODEX_SUBSCRIPTION`] or
/// [`CATALOG_BACKEND_RESPONSES_API`]); service tiers are resolved against
/// it so a tier available only on one backend can never leak onto another.
///
/// # Errors
///
/// Returns [`ProviderError::RequestSerializationFailed`] when the payload
/// cannot be serialized, or when a `ToolResult` message in the conversation
/// history is missing its `tool_call_id` — the API requires `call_id` on
/// every `function_call_output` (and `custom_tool_call_output`) item, so
/// synthesising an empty string would silently corrupt the conversation. A
/// missing `tool_call_id` is always an upstream bug; surfacing it here lets
/// the caller fail the turn rather than dispatch an unmoored tool result.
/// Returns [`ProviderError::InvalidRequest`] when the requested service tier
/// is not supported for the model on `catalog_backend`.
pub(crate) fn build_payload(
    request: &ProviderRequest,
    catalog_backend: &str,
) -> Result<serde_json::Value, ProviderError> {
    let mut instructions = String::new();
    let mut input = Vec::new();

    for msg in &request.messages {
        match msg.role {
            MessageRole::System => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                if let Some(content) = &msg.content {
                    instructions.push_str(content);
                }
            }
            MessageRole::Developer => {
                input.push(serialize_developer_message(msg));
            }
            MessageRole::User => {
                input.push(serialize_user_message(msg));
            }
            MessageRole::Assistant => {
                serialize_assistant_into(&mut input, msg);
            }
            MessageRole::ToolResult => {
                input.push(serialize_tool_result(msg)?);
            }
        }
    }

    let tools: Vec<serde_json::Value> = request.tools.iter().map(serialize_tool).collect();

    let reasoning = Some(ReasoningParam {
        effort: request.reasoning_effort,
        summary: request.reasoning_summary.clone().unwrap_or_default(),
    });

    let include = if request.reasoning_effort.is_some() {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        Vec::new()
    };

    let context_management = if let Some(management) = request.context_management.as_ref() {
        vec![ContextManagementItem::Compaction {
            compact_threshold: management.compact_threshold_tokens,
        }]
    } else {
        Vec::new()
    };
    let service_tier = service_tier_provider_value(request, catalog_backend)?;

    let payload = ResponsesApiPayload {
        model: request.model.clone(),
        instructions,
        input,
        tools,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        stream: true,
        store: request.store,
        service_tier,
        include,
        reasoning,
        prompt_cache_key: request.cache_key.clone(),
        previous_response_id: request.previous_response_id.clone(),
        prompt_cache_retention: None,
        context_management,
    };
    let mut value =
        serde_json::to_value(payload).map_err(|err| ProviderError::RequestSerializationFailed {
            reason: format!("failed to serialize responses request: {err}"),
        })?;
    merge_provider_options(&mut value, request.config.as_ref())?;
    Ok(value)
}

fn merge_provider_options(
    payload: &mut serde_json::Value,
    options: Option<&ProviderOptions>,
) -> Result<(), ProviderError> {
    let Some(options) = options else {
        return Ok(());
    };
    let option_object = select_responses_options(&options.0)?;
    let Some(payload_object) = payload.as_object_mut() else {
        return Err(ProviderError::RequestSerializationFailed {
            reason: "responses payload was not a JSON object".to_string(),
        });
    };
    for (key, value) in option_object {
        reject_protected_option_key(key)?;
        payload_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn select_responses_options(
    options: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, ProviderError> {
    let object = options
        .as_object()
        .ok_or_else(|| ProviderError::InvalidRequest {
            message: "provider_options for OpenAI Responses must be a JSON object".to_string(),
        })?;
    if let Some(scoped) = object
        .get("api_options")
        .and_then(|value| value.get("openai_responses"))
    {
        return scoped
            .as_object()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: "provider_options.api_options.openai_responses must be a JSON object"
                    .to_string(),
            });
    }
    if let Some(scoped) = object.get("openai_responses") {
        return scoped
            .as_object()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: "provider_options.openai_responses must be a JSON object".to_string(),
            });
    }
    Ok(object)
}

fn reject_protected_option_key(key: &str) -> Result<(), ProviderError> {
    if matches!(
        key,
        "model"
            | "instructions"
            | "input"
            | "tools"
            | "tool_choice"
            | "parallel_tool_calls"
            | "stream"
            | "store"
            | "include"
            | "reasoning"
            | "prompt_cache_key"
            | "previous_response_id"
            | "context_management"
    ) {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "provider_options.openai_responses.{key} is owned by Norn and cannot be overridden",
            ),
        });
    }
    Ok(())
}

/// Resolves the requested service tier against the backend the provider
/// actually uses, never the catalog default: a tier catalogued only for
/// the Codex subscription backend must not resolve for a direct
/// Responses API connection.
fn service_tier_provider_value(
    request: &ProviderRequest,
    catalog_backend: &str,
) -> Result<Option<String>, ProviderError> {
    let Some(tier) = request.service_tier else {
        return Ok(None);
    };
    let Some(provider_value) = crate::model_catalog::service_tier_provider_value(
        CATALOG_PROVIDER,
        catalog_backend,
        &request.model,
        tier.as_str(),
    ) else {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "service tier '{}' is not supported for model '{}' on {CATALOG_PROVIDER}.{catalog_backend}",
                tier.as_str(),
                request.model,
            ),
        });
    };
    Ok(Some(provider_value.to_owned()))
}

fn serialize_developer_message(msg: &Message) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "role": "developer",
        "content": msg.content.as_deref().unwrap_or(""),
    })
}

fn serialize_user_message(msg: &Message) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": msg.content.as_deref().unwrap_or("") }],
    })
}

/// Serializes an assistant message into the input array.
///
/// The Responses API expects tool calls as top-level items in the input
/// array, not nested inside an assistant message's content. Text output (if
/// any) is emitted as a separate `output_text` content item inside an
/// assistant-role message.
///
/// Captured reasoning items that carry `encrypted_content` are replayed
/// first, as `"reasoning"` input items **before** the assistant content and
/// tool calls they preceded in the model's original output — the Codex CLI
/// reference behaviour for stateless threading
/// (`response_threading: false`): without the echo the ChatGPT/codex
/// backend drops the model's reasoning between tool-call iterations. Items
/// without `encrypted_content` are never echoed (the server cannot
/// reconstruct reasoning state from them and rejects bare reasoning items),
/// and the server-internal `rs_*` item id is deliberately omitted from the
/// echo (`protocol-models.rs:757-761`).
///
/// Each `AssistantToolCall` is replayed with the wire envelope its `kind`
/// requires:
/// * [`ToolCallKind::Function`] → `function_call` item with an `arguments`
///   JSON string.
/// * [`ToolCallKind::Custom`] → `custom_tool_call` item with an `input`
///   freeform string (no JSON envelope).
///
/// The `call_id` is the only identifier the model correlates on replay; the
/// `fc_*`/`ctc_*` item `id` is server-internal and is not echoed (the Codex
/// reference applies `skip_serializing` to the same field at
/// `protocol-models.rs:779-781`). Empty `call_id` is a bug, not a
/// fallback — `tc.call_id` carries the value `assemble_response` propagated.
fn serialize_assistant_into(input: &mut Vec<serde_json::Value>, msg: &Message) {
    for item in &msg.reasoning {
        let Some(encrypted_content) = &item.encrypted_content else {
            continue;
        };
        let mut reasoning_item = serde_json::json!({
            "type": "reasoning",
            "summary": item.summary,
            "encrypted_content": encrypted_content,
        });
        if let (Some(content), Some(object)) = (&item.content, reasoning_item.as_object_mut()) {
            object.insert("content".to_string(), serde_json::json!(content));
        }
        input.push(reasoning_item);
    }

    if let Some(content) = &msg.content {
        input.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": content }],
        }));
    }

    for tc in &msg.tool_calls {
        let item = match tc.kind {
            ToolCallKind::Function => serde_json::json!({
                "type": "function_call",
                "call_id": tc.call_id,
                "name": tc.name,
                "arguments": tc.arguments,
            }),
            ToolCallKind::Custom => serde_json::json!({
                "type": "custom_tool_call",
                "call_id": tc.call_id,
                "name": tc.name,
                "input": tc.arguments,
            }),
        };
        input.push(item);
    }
}

/// Serializes a `ToolResult` message into a `function_call_output` or
/// `custom_tool_call_output` input item.
///
/// The wire envelope is selected from
/// [`Message::tool_call_kind`](crate::provider::request::Message::tool_call_kind):
/// `function_call_output` for [`ToolCallKind::Function`] (the default when
/// the kind is `None`, matching pre-existing tool results), and
/// `custom_tool_call_output` for [`ToolCallKind::Custom`]. The payload shape
/// (`call_id` + `output`) is identical for both envelopes — only the `type`
/// discriminator differs.
///
/// `call_id` is the only identifier the model correlates a tool result with
/// its originating call; the Responses API rejects items that omit it, and a
/// silently-empty `call_id` would corrupt the conversation by detaching the
/// result from its call. A missing `tool_call_id` is therefore returned as a
/// hard [`ProviderError::RequestSerializationFailed`] rather than papered
/// over with an empty string — every `ToolResult` is constructed from an
/// `AssembledToolCall` that already carries the `call_id`, so absence is
/// unambiguously an upstream bug.
fn serialize_tool_result(msg: &Message) -> Result<serde_json::Value, ProviderError> {
    let call_id = msg
        .tool_call_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::RequestSerializationFailed {
            reason: format!(
                "tool result for {tool_name} missing tool_call_id; refusing to dispatch an unmoored tool_call_output",
                tool_name = msg.tool_name.as_deref().unwrap_or("<unknown tool>"),
            ),
        })?;
    let item_type = match msg.tool_call_kind.unwrap_or_default() {
        ToolCallKind::Function => "function_call_output",
        ToolCallKind::Custom => "custom_tool_call_output",
    };
    Ok(serde_json::json!({
        "type": item_type,
        "call_id": call_id,
        "output": msg.content.as_deref().unwrap_or(""),
    }))
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
    use crate::provider::request::{
        ProviderContextManagement, ProviderOptions, ServiceTier, ToolDefinition,
    };
    use crate::provider::tools::{
        HostedToolDefinition, HostedWebSearchTool, ProviderToolDefinition,
    };

    fn make_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![
                Message {
                    reasoning: Vec::new(),
                    role: MessageRole::System,
                    content: Some("You are helpful.".to_string()),
                    thinking: String::new(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
                    reasoning: Vec::new(),
                    role: MessageRole::User,
                    content: Some("Hello".to_string()),
                    thinking: String::new(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
                    reasoning: Vec::new(),
                    role: MessageRole::Assistant,
                    content: Some("Hi there".to_string()),
                    thinking: String::new(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
            ],
            tools: vec![
                ToolDefinition {
                    name: "read_file".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }
                .into(),
                ToolDefinition {
                    name: "write_file".to_string(),
                    description: "Write a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }
                .into(),
            ],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    #[test]
    fn payload_shape_matches_api() {
        let req = make_request();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["model"], "gpt-4.1-mini");
        assert_eq!(json["instructions"], "You are helpful.");
        assert_eq!(json["stream"], true);
        assert_eq!(json["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["tools"].as_array().map(Vec::len), Some(2));
        assert!(json.get("response_format").is_none());
        assert!(json.get("text").is_none());
    }

    #[test]
    fn threaded_state_and_context_management_pass_through() {
        let mut req = make_request();
        req.store = true;
        req.previous_response_id = Some("resp_prev".to_string());
        req.context_management = Some(ProviderContextManagement {
            compact_threshold_tokens: 200_000,
        });

        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["store"], true);
        assert_eq!(json["previous_response_id"], "resp_prev");
        assert_eq!(json["context_management"][0]["type"], "compaction");
        assert_eq!(json["context_management"][0]["compact_threshold"], 200_000,);
    }

    #[test]
    fn prompt_cache_key_passes_through() {
        let mut req = make_request();
        req.cache_key = Some("session-cache".to_string());

        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["prompt_cache_key"], "session-cache");
    }

    #[test]
    fn payload_can_include_hosted_web_search_tool() {
        let mut req = make_request();
        req.tools.push(ProviderToolDefinition::Hosted(
            HostedToolDefinition::WebSearch(HostedWebSearchTool::default()),
        ));

        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let tools = payload["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| {
            tool.get("type").and_then(serde_json::Value::as_str) == Some("web_search")
                && tool.get("name").is_none()
                && tool.get("parameters").is_none()
        }));
    }

    #[test]
    fn system_message_becomes_instructions() {
        let req = make_request();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        assert_eq!(payload["instructions"], "You are helpful.");
        assert!(
            payload["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item.get("role").and_then(|r| r.as_str()) != Some("system"))
        );
    }

    #[test]
    fn no_response_format_in_payload() {
        let req = make_request();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(json.get("response_format").is_none());
        let json_str = serde_json::to_string(&payload).expect("serialize");
        assert!(!json_str.contains("response_format"));
        assert!(!json_str.contains("json_schema"));
    }

    #[test]
    fn reasoning_effort_high() {
        let mut req = make_request();
        req.reasoning_effort = Some(ReasoningEffort::High);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["reasoning"]["summary"], "auto");
    }

    #[test]
    fn service_tier_fast_serializes_as_openai_priority() {
        let mut req = make_request();
        req.model = "gpt-5.5".to_owned();
        req.service_tier = Some(ServiceTier::Fast);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["service_tier"], "priority");
    }

    #[test]
    fn unsupported_service_tier_returns_invalid_request() {
        let mut req = make_request();
        req.model = "gpt-5.4-mini".to_owned();
        req.service_tier = Some(ServiceTier::Fast);
        let err = build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION)
            .expect_err("unsupported tier must fail");
        assert!(matches!(err, ProviderError::InvalidRequest { .. }));
    }

    /// Regression test (final-state hardening, T1 item 9): service tiers
    /// resolve against the backend the provider actually uses. `fast` is
    /// catalogued for gpt-5.5 on the Codex subscription backend only, so
    /// the same request on the direct Responses API backend must be
    /// rejected instead of silently borrowing the subscription mapping.
    #[test]
    fn service_tier_resolution_uses_actual_backend_not_catalog_default() {
        let mut req = make_request();
        req.model = "gpt-5.5".to_owned();
        req.service_tier = Some(ServiceTier::Fast);

        let subscription = build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION)
            .expect("subscription backend supports the tier");
        assert_eq!(subscription["service_tier"], "priority");

        let err = build_payload(&req, CATALOG_BACKEND_RESPONSES_API)
            .expect_err("the responses_api backend has no catalogued tier for gpt-5.5");
        match err {
            ProviderError::InvalidRequest { message } => {
                assert!(
                    message.contains("responses_api"),
                    "error must name the actual backend: {message}"
                );
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    /// The catalog constants baked into this module must exist in the
    /// generated catalog — a rename in `assets/models.json` must fail here
    /// rather than silently making every tier resolution miss.
    #[test]
    fn catalog_backend_constants_exist_in_catalog() {
        assert!(
            crate::model_catalog::find_backend(
                CATALOG_PROVIDER,
                CATALOG_BACKEND_CODEX_SUBSCRIPTION
            )
            .is_some(),
            "codex_subscription backend missing from the model catalog"
        );
        assert!(
            crate::model_catalog::find_backend(CATALOG_PROVIDER, CATALOG_BACKEND_RESPONSES_API)
                .is_some(),
            "responses_api backend missing from the model catalog"
        );
    }

    #[test]
    fn provider_options_merge_advanced_responses_fields() {
        let mut req = make_request();
        req.config = Some(ProviderOptions(serde_json::json!({
            "api_options": {
                "openai_responses": {
                    "temperature": 0.2,
                    "top_p": 0.9,
                    "max_output_tokens": 1024,
                    "text": {"format": {"type": "json_object"}}
                }
            }
        })));

        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");

        assert_eq!(payload["temperature"], 0.2);
        assert_eq!(payload["top_p"], 0.9);
        assert_eq!(payload["max_output_tokens"], 1024);
        assert_eq!(payload["text"]["format"]["type"], "json_object");
        assert!(payload.get("api_options").is_none());
    }

    #[test]
    fn provider_options_reject_protected_responses_fields() {
        let mut req = make_request();
        req.config = Some(ProviderOptions(serde_json::json!({
            "input": []
        })));

        let err = build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).unwrap_err();

        assert!(matches!(err, ProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn reasoning_effort_medium() {
        let mut req = make_request();
        req.reasoning_effort = Some(ReasoningEffort::Medium);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "medium");
    }

    #[test]
    fn reasoning_effort_low() {
        let mut req = make_request();
        req.reasoning_effort = Some(ReasoningEffort::Low);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "low");
    }

    #[test]
    fn reasoning_defaults_when_no_effort_set() {
        let req = make_request();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["summary"], "auto");
        assert!(json["reasoning"].get("effort").is_none());
    }

    #[test]
    fn model_passed_through_without_validation() {
        let mut req = make_request();
        req.model = "custom-model-xyz-v99".to_string();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        assert_eq!(payload["model"], "custom-model-xyz-v99");
    }

    #[test]
    fn tools_serialize_as_functions() {
        let req = make_request();
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        for tool in payload["tools"].as_array().unwrap() {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["strict"], false);
        }
    }

    #[test]
    fn system_and_developer_both_become_developer_input() {
        let mut req = make_request();
        req.messages.insert(
            1,
            Message {
                reasoning: Vec::new(),
                role: MessageRole::Developer,
                content: Some("dynamic context here".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            },
        );
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        assert_eq!(
            payload["instructions"], "You are helpful.",
            "system message must go to instructions",
        );
        let dev_items: Vec<_> = payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item.get("role").and_then(|r| r.as_str()) == Some("developer"))
            .collect();
        assert_eq!(dev_items.len(), 1, "only the Developer message in input");
        assert_eq!(dev_items[0]["content"], "dynamic context here");
    }

    #[test]
    fn tool_result_missing_call_id_returns_error() {
        // F3: a ToolResult message without a tool_call_id is always an
        // upstream bug — every ToolResult is constructed from an
        // AssembledToolCall that already carries the call_id. Surfacing it
        // as a RequestSerializationFailed lets the loop refuse the turn
        // instead of dispatching an unmoored function_call_output to the
        // API.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::ToolResult,
                content: Some(r#"{"ok":true}"#.to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: Some("read".to_string()),
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        let err = build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION)
            .expect_err("missing tool_call_id must be rejected");
        match err {
            ProviderError::RequestSerializationFailed { reason } => {
                assert!(
                    reason.contains("missing tool_call_id"),
                    "reason should describe the missing field: {reason}",
                );
                assert!(
                    reason.contains("read"),
                    "reason should name the tool so the bug can be traced upstream: {reason}",
                );
            }
            other => panic!("expected RequestSerializationFailed, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_empty_call_id_returns_error() {
        // An empty string is the exact value the old `unwrap_or("")` fallback
        // produced. Treating it as missing closes the loophole — the model
        // never sees an empty call_id even if some upstream caller passes
        // `Some(String::new())`.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::ToolResult,
                content: Some("output".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: Some(String::new()),
                tool_name: Some("write".to_string()),
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        assert!(matches!(
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION),
            Err(ProviderError::RequestSerializationFailed { .. }),
        ));
    }

    #[test]
    fn custom_tool_call_serialises_with_input_field_and_custom_envelope() {
        // F5: an AssistantToolCall with ToolCallKind::Custom must echo as a
        // `custom_tool_call` item carrying `input` (not `arguments`). The
        // freeform body passes through verbatim — no JSON wrapping.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::Assistant,
                content: None,
                thinking: String::new(),
                tool_calls: vec![crate::provider::request::AssistantToolCall {
                    call_id: "call_custom".to_string(),
                    name: "freeform_tool".to_string(),
                    arguments: "*** BEGIN PATCH ***".to_string(),
                    kind: ToolCallKind::Custom,
                }],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
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
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert_eq!(input[0]["call_id"], "call_custom");
        assert_eq!(input[0]["name"], "freeform_tool");
        assert_eq!(input[0]["input"], "*** BEGIN PATCH ***");
        // The function-call-only `arguments` field must be absent so the API
        // does not double-encode the body.
        assert!(input[0].get("arguments").is_none());
    }

    #[test]
    fn function_tool_call_serialises_with_arguments_field() {
        // F5: a function-kind call must still echo with `arguments`, not
        // `input`. This proves the kind discriminator is honoured both ways.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::Assistant,
                content: None,
                thinking: String::new(),
                tool_calls: vec![crate::provider::request::AssistantToolCall {
                    call_id: "call_fn".to_string(),
                    name: "read".to_string(),
                    arguments: r#"{"path":"a"}"#.to_string(),
                    kind: ToolCallKind::Function,
                }],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
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
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["arguments"], r#"{"path":"a"}"#);
        assert!(input[0].get("input").is_none());
    }

    #[test]
    fn custom_tool_result_serialises_with_custom_output_envelope() {
        // F5: a ToolResult message tagged as Custom must echo with
        // `custom_tool_call_output`, mirroring the call's envelope.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::ToolResult,
                content: Some("hunk applied".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: Some("call_custom".to_string()),
                tool_name: Some("apply_patch".to_string()),
                tool_call_kind: Some(ToolCallKind::Custom),
            }],
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
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call_custom");
        assert_eq!(input[0]["output"], "hunk applied");
    }

    #[test]
    fn tool_call_kind_none_falls_back_to_function_call_output() {
        // Backward compatibility: a ToolResult Message produced by code that
        // does not yet plumb the kind (legacy callers, older session events)
        // must still serialise as `function_call_output`.
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::ToolResult,
                content: Some("ok".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: Some("call_legacy".to_string()),
                tool_name: Some("read".to_string()),
                tool_call_kind: None,
            }],
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
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "function_call_output");
    }

    #[test]
    fn tool_result_with_call_id_serializes_function_call_output() {
        let req = ProviderRequest {
            messages: vec![Message {
                reasoning: Vec::new(),
                role: MessageRole::ToolResult,
                content: Some(r#"{"lines":42}"#.to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: Some("call_xyz".to_string()),
                tool_name: Some("read".to_string()),
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-4.1-mini".to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_xyz");
        assert_eq!(input[0]["output"], r#"{"lines":42}"#);
    }

    // -- Encrypted reasoning replay (stateless threading) -------------------

    use crate::provider::reasoning::{ReasoningContentPart, ReasoningItem, ReasoningSummaryPart};

    fn encrypted_item(id: &str, summary: &str, blob: &str) -> ReasoningItem {
        ReasoningItem {
            id: id.to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: summary.to_owned(),
            }],
            content: None,
            encrypted_content: Some(blob.to_owned()),
        }
    }

    fn assistant_request_with_reasoning(reasoning: Vec<ReasoningItem>) -> ProviderRequest {
        ProviderRequest {
            messages: vec![Message {
                role: MessageRole::Assistant,
                content: Some("calling a tool".to_string()),
                thinking: "summary text".to_string(),
                reasoning,
                tool_calls: vec![crate::provider::request::AssistantToolCall {
                    call_id: "call_1".to_string(),
                    name: "read".to_string(),
                    arguments: r#"{"path":"a"}"#.to_string(),
                    kind: ToolCallKind::Function,
                }],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }],
            tools: vec![],
            model: "gpt-5".to_string(),
            reasoning_effort: Some(ReasoningEffort::Medium),
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        }
    }

    #[test]
    fn encrypted_reasoning_replays_before_assistant_content_and_tool_calls() {
        // Stateless threading (Codex CLI reference behaviour): captured
        // reasoning items with encrypted_content are echoed as "reasoning"
        // input items BEFORE the assistant message and tool calls they
        // preceded, with the blob passed through verbatim and the
        // server-internal rs_* id omitted.
        let req = assistant_request_with_reasoning(vec![encrypted_item(
            "rs_1",
            "I thought about it",
            "opaque-blob-1",
        )]);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 3, "reasoning + message + function_call");
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "opaque-blob-1");
        assert_eq!(input[0]["summary"][0]["type"], "summary_text");
        assert_eq!(input[0]["summary"][0]["text"], "I thought about it");
        assert!(
            input[0].get("id").is_none(),
            "server-internal rs_* id must not be echoed"
        );
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
    }

    #[test]
    fn reasoning_without_encrypted_content_is_not_replayed() {
        // Items captured on store: true responses carry no
        // encrypted_content; the stateless backend cannot reconstruct
        // reasoning state from them, so they are never echoed.
        let req = assistant_request_with_reasoning(vec![ReasoningItem {
            id: "rs_plain".to_owned(),
            summary: vec![ReasoningSummaryPart::SummaryText {
                text: "not replayable".to_owned(),
            }],
            content: None,
            encrypted_content: None,
        }]);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "message + function_call only");
        assert!(
            input.iter().all(|item| item["type"] != "reasoning"),
            "no reasoning item may be echoed without encrypted_content: {input:?}"
        );
    }

    #[test]
    fn multiple_encrypted_reasoning_items_replay_in_capture_order() {
        let req = assistant_request_with_reasoning(vec![
            encrypted_item("rs_1", "first", "blob-1"),
            encrypted_item("rs_2", "second", "blob-2"),
        ]);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "blob-1");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "blob-2");
        assert_eq!(input[2]["type"], "message");
    }

    #[test]
    fn encrypted_reasoning_replays_content_parts_when_present() {
        let mut item = encrypted_item("rs_c", "with content", "blob-c");
        item.content = Some(vec![ReasoningContentPart::ReasoningText {
            text: "raw chain".to_owned(),
        }]);
        let req = assistant_request_with_reasoning(vec![item]);
        let payload =
            build_payload(&req, CATALOG_BACKEND_CODEX_SUBSCRIPTION).expect("build_payload");
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["content"][0]["type"], "reasoning_text");
        assert_eq!(input[0]["content"][0]["text"], "raw chain");
    }
}
