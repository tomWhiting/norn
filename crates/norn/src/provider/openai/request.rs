//! Request serialization for the `OpenAI` Responses API.

use serde::Serialize;

use super::tools::serialize_tool;
use crate::error::ProviderError;
use crate::provider::request::{
    Message, MessageRole, ProviderRequest, ReasoningEffort, ReasoningSummary, ToolCallKind,
};

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
/// # Errors
///
/// Returns [`ProviderError::ResponseParseError`] when a `ToolResult` message
/// in the conversation history is missing its `tool_call_id` — the API
/// requires `call_id` on every `function_call_output` (and
/// `custom_tool_call_output`) item, so synthesising an empty string would
/// silently corrupt the conversation. A missing `tool_call_id` is always an
/// upstream bug; surfacing it here lets the caller fail the turn rather than
/// dispatch an unmoored tool result.
pub(crate) fn build_payload(
    request: &ProviderRequest,
) -> Result<ResponsesApiPayload, ProviderError> {
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
    let service_tier = service_tier_provider_value(request)?;

    Ok(ResponsesApiPayload {
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
    })
}

fn service_tier_provider_value(request: &ProviderRequest) -> Result<Option<String>, ProviderError> {
    let Some(tier) = request.service_tier else {
        return Ok(None);
    };
    let Some(provider_value) = crate::model_catalog::service_tier_provider_value(
        crate::model_catalog::DEFAULT_PROVIDER,
        crate::model_catalog::DEFAULT_BACKEND,
        &request.model,
        tier.as_str(),
    ) else {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "service tier '{}' is not supported for model '{}' on {}.{}",
                tier.as_str(),
                request.model,
                crate::model_catalog::DEFAULT_PROVIDER,
                crate::model_catalog::DEFAULT_BACKEND,
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
/// hard error rather than papered over with an empty string — every
/// `ToolResult` is constructed from an `AssembledToolCall` that already
/// carries the `call_id`, so absence is unambiguously an upstream bug.
fn serialize_tool_result(msg: &Message) -> Result<serde_json::Value, ProviderError> {
    let call_id = msg
        .tool_call_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::ResponseParseError {
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
    use crate::provider::request::{ProviderContextManagement, ServiceTier, ToolDefinition};
    use crate::provider::tools::{
        HostedToolDefinition, HostedWebSearchTool, ProviderToolDefinition,
    };

    fn make_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![
                Message {
                    role: MessageRole::System,
                    content: Some("You are helpful.".to_string()),
                    thinking: String::new(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
                    role: MessageRole::User,
                    content: Some("Hello".to_string()),
                    thinking: String::new(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
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
        let payload = build_payload(&req).expect("build_payload");
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

        let payload = build_payload(&req).expect("build_payload");
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

        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["prompt_cache_key"], "session-cache");
    }

    #[test]
    fn payload_can_include_hosted_web_search_tool() {
        let mut req = make_request();
        req.tools.push(ProviderToolDefinition::Hosted(
            HostedToolDefinition::WebSearch(HostedWebSearchTool::default()),
        ));

        let payload = build_payload(&req).expect("build_payload");
        assert!(payload.tools.iter().any(|tool| {
            tool.get("type").and_then(serde_json::Value::as_str) == Some("web_search")
                && tool.get("name").is_none()
                && tool.get("parameters").is_none()
        }));
    }

    #[test]
    fn system_message_becomes_instructions() {
        let req = make_request();
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.instructions, "You are helpful.");
        assert!(
            payload
                .input
                .iter()
                .all(|item| item.get("role").and_then(|r| r.as_str()) != Some("system"))
        );
    }

    #[test]
    fn no_response_format_in_payload() {
        let req = make_request();
        let payload = build_payload(&req).expect("build_payload");
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
        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["reasoning"]["summary"], "auto");
    }

    #[test]
    fn service_tier_fast_serializes_as_openai_priority() {
        let mut req = make_request();
        req.model = "gpt-5.5".to_owned();
        req.service_tier = Some(ServiceTier::Fast);
        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["service_tier"], "priority");
    }

    #[test]
    fn unsupported_service_tier_returns_invalid_request() {
        let mut req = make_request();
        req.model = "gpt-5.4-mini".to_owned();
        req.service_tier = Some(ServiceTier::Fast);
        let err = build_payload(&req).expect_err("unsupported tier must fail");
        assert!(matches!(err, ProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn reasoning_effort_medium() {
        let mut req = make_request();
        req.reasoning_effort = Some(ReasoningEffort::Medium);
        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "medium");
    }

    #[test]
    fn reasoning_effort_low() {
        let mut req = make_request();
        req.reasoning_effort = Some(ReasoningEffort::Low);
        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["effort"], "low");
    }

    #[test]
    fn reasoning_defaults_when_no_effort_set() {
        let req = make_request();
        let payload = build_payload(&req).expect("build_payload");
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["reasoning"]["summary"], "auto");
        assert!(json["reasoning"].get("effort").is_none());
    }

    #[test]
    fn model_passed_through_without_validation() {
        let mut req = make_request();
        req.model = "custom-model-xyz-v99".to_string();
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.model, "custom-model-xyz-v99");
    }

    #[test]
    fn tools_serialize_as_functions() {
        let req = make_request();
        let payload = build_payload(&req).expect("build_payload");
        for tool in &payload.tools {
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
                role: MessageRole::Developer,
                content: Some("dynamic context here".to_string()),
                thinking: String::new(),
                tool_calls: vec![],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            },
        );
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(
            payload.instructions, "You are helpful.",
            "system message must go to instructions",
        );
        let dev_items: Vec<_> = payload
            .input
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
        // AssembledToolCall that already carries the call_id. Surfacing it as
        // a ResponseParseError lets the loop refuse the turn instead of
        // dispatching an unmoored function_call_output to the API.
        let req = ProviderRequest {
            messages: vec![Message {
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
        let err = build_payload(&req).expect_err("missing tool_call_id must be rejected");
        match err {
            ProviderError::ResponseParseError { reason } => {
                assert!(
                    reason.contains("missing tool_call_id"),
                    "reason should describe the missing field: {reason}",
                );
                assert!(
                    reason.contains("read"),
                    "reason should name the tool so the bug can be traced upstream: {reason}",
                );
            }
            other => panic!("expected ResponseParseError, got {other:?}"),
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
            build_payload(&req),
            Err(ProviderError::ResponseParseError { .. }),
        ));
    }

    #[test]
    fn custom_tool_call_serialises_with_input_field_and_custom_envelope() {
        // F5: an AssistantToolCall with ToolCallKind::Custom must echo as a
        // `custom_tool_call` item carrying `input` (not `arguments`). The
        // freeform body passes through verbatim — no JSON wrapping.
        let req = ProviderRequest {
            messages: vec![Message {
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
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.input.len(), 1);
        assert_eq!(payload.input[0]["type"], "custom_tool_call");
        assert_eq!(payload.input[0]["call_id"], "call_custom");
        assert_eq!(payload.input[0]["name"], "freeform_tool");
        assert_eq!(payload.input[0]["input"], "*** BEGIN PATCH ***");
        // The function-call-only `arguments` field must be absent so the API
        // does not double-encode the body.
        assert!(payload.input[0].get("arguments").is_none());
    }

    #[test]
    fn function_tool_call_serialises_with_arguments_field() {
        // F5: a function-kind call must still echo with `arguments`, not
        // `input`. This proves the kind discriminator is honoured both ways.
        let req = ProviderRequest {
            messages: vec![Message {
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
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.input[0]["type"], "function_call");
        assert_eq!(payload.input[0]["arguments"], r#"{"path":"a"}"#);
        assert!(payload.input[0].get("input").is_none());
    }

    #[test]
    fn custom_tool_result_serialises_with_custom_output_envelope() {
        // F5: a ToolResult message tagged as Custom must echo with
        // `custom_tool_call_output`, mirroring the call's envelope.
        let req = ProviderRequest {
            messages: vec![Message {
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
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.input[0]["type"], "custom_tool_call_output");
        assert_eq!(payload.input[0]["call_id"], "call_custom");
        assert_eq!(payload.input[0]["output"], "hunk applied");
    }

    #[test]
    fn tool_call_kind_none_falls_back_to_function_call_output() {
        // Backward compatibility: a ToolResult Message produced by code that
        // does not yet plumb the kind (legacy callers, older session events)
        // must still serialise as `function_call_output`.
        let req = ProviderRequest {
            messages: vec![Message {
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
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.input[0]["type"], "function_call_output");
    }

    #[test]
    fn tool_result_with_call_id_serializes_function_call_output() {
        let req = ProviderRequest {
            messages: vec![Message {
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
        let payload = build_payload(&req).expect("build_payload");
        assert_eq!(payload.input.len(), 1);
        assert_eq!(payload.input[0]["type"], "function_call_output");
        assert_eq!(payload.input[0]["call_id"], "call_xyz");
        assert_eq!(payload.input[0]["output"], r#"{"lines":42}"#);
    }
}
