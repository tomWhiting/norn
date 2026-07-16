//! Request serialization for OpenAI-compatible Chat Completions.

use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::request::{
    Message, MessageRole, ProviderOptions, ProviderRequest, ReasoningEffort, ToolCallKind,
};
use crate::provider::tools::ProviderToolDefinition;

const CATALOG_PROVIDER: &str = "openai";
const CATALOG_BACKEND: &str = "openai_compatible_chat";

/// Serialized payload for `POST /chat/completions`.
#[derive(Debug, Serialize)]
pub(super) struct ChatCompletionsPayload {
    /// Model identifier.
    pub model: String,
    /// Chat message history.
    pub messages: Vec<serde_json::Value>,
    /// Function tools.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    /// Tool selection policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Always true: SSE streaming.
    pub stream: bool,
    /// Streaming options.
    pub stream_options: StreamOptions,
    /// Optional provider service tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Optional reasoning effort control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Chat Completions `stream_options` object.
#[derive(Debug, Serialize)]
pub(super) struct StreamOptions {
    /// Request a final usage-bearing chunk when the backend supports it.
    pub include_usage: bool,
}

/// Builds the Chat Completions API payload from a provider-neutral request.
///
/// # Errors
///
/// Returns [`ProviderError`] when the request contains a tool surface or
/// replay item that Chat Completions cannot represent safely.
pub(super) fn build_payload(request: &ProviderRequest) -> Result<serde_json::Value, ProviderError> {
    let messages = request
        .messages
        .iter()
        .map(serialize_message)
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .iter()
        .map(serialize_tool)
        .collect::<Result<Vec<_>, _>>()?;
    let tool_choice = if tools.is_empty() {
        None
    } else {
        Some("auto".to_string())
    };
    let payload = ChatCompletionsPayload {
        model: request.model.clone(),
        messages,
        tools,
        tool_choice,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        service_tier: service_tier_provider_value(request)?,
        reasoning_effort: request.reasoning_effort,
    };
    let mut value =
        serde_json::to_value(payload).map_err(|err| ProviderError::RequestSerializationFailed {
            reason: format!("failed to serialize chat completions request: {err}"),
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
    let option_object = select_chat_completion_options(&options.0)?;
    let Some(payload_object) = payload.as_object_mut() else {
        return Err(ProviderError::RequestSerializationFailed {
            reason: "chat completions payload was not a JSON object".to_string(),
        });
    };
    for (key, value) in option_object {
        reject_protected_option_key(key)?;
        payload_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn select_chat_completion_options(
    options: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, ProviderError> {
    let object = options
        .as_object()
        .ok_or_else(|| ProviderError::InvalidRequest {
            message:
                "provider_options for openai-compatible chat completions must be a JSON object"
                    .to_string(),
        })?;
    if let Some(scoped) = object
        .get("api_options")
        .and_then(|value| value.get("openai_chat_completions"))
    {
        return scoped
            .as_object()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message:
                    "provider_options.api_options.openai_chat_completions must be a JSON object"
                        .to_string(),
            });
    }
    if let Some(scoped) = object.get("openai_chat_completions") {
        return scoped
            .as_object()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: "provider_options.openai_chat_completions must be a JSON object"
                    .to_string(),
            });
    }
    Ok(object)
}

fn reject_protected_option_key(key: &str) -> Result<(), ProviderError> {
    if matches!(
        key,
        "model" | "messages" | "tools" | "stream" | "functions" | "function_call"
    ) {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "provider_options.openai_chat_completions.{key} is owned by Norn and cannot be overridden",
            ),
        });
    }
    Ok(())
}

fn serialize_message(message: &Message) -> Result<serde_json::Value, ProviderError> {
    match message.role {
        MessageRole::System | MessageRole::Developer => {
            Ok(chat_message("system", message.content.as_deref()))
        }
        MessageRole::User => Ok(chat_message("user", message.content.as_deref())),
        MessageRole::Assistant => serialize_assistant_message(message),
        MessageRole::ToolResult => serialize_tool_result(message),
    }
}

fn chat_message(role: &str, content: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "role": role,
        "content": content.unwrap_or(""),
    })
}

fn serialize_assistant_message(message: &Message) -> Result<serde_json::Value, ProviderError> {
    if !message.response_items.is_empty() {
        return Err(ProviderError::UnsupportedFeature {
            feature: "canonical Responses item replay on chat_completions".to_owned(),
        });
    }
    let mut map = serde_json::Map::new();
    map.insert("role".to_string(), serde_json::json!("assistant"));
    map.insert(
        "content".to_string(),
        message
            .content
            .as_deref()
            .map_or(serde_json::Value::Null, serde_json::Value::from),
    );
    if !message.tool_calls.is_empty() {
        let calls = message
            .tool_calls
            .iter()
            .map(|call| {
                if call.kind != ToolCallKind::Function {
                    return Err(ProviderError::UnsupportedFeature {
                        feature: "custom tool call replay on chat_completions".to_string(),
                    });
                }
                Ok(serde_json::json!({
                    "id": call.call_id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments,
                    },
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        map.insert("tool_calls".to_string(), serde_json::Value::Array(calls));
    }
    Ok(serde_json::Value::Object(map))
}

fn serialize_tool_result(message: &Message) -> Result<serde_json::Value, ProviderError> {
    if message.tool_call_kind.unwrap_or_default() != ToolCallKind::Function {
        return Err(ProviderError::UnsupportedFeature {
            feature: "custom tool result replay on chat_completions".to_string(),
        });
    }
    let call_id = message
        .tool_call_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ProviderError::RequestSerializationFailed {
            reason: "tool result missing tool_call_id; refusing to dispatch an unmoored chat tool result"
                .to_owned(),
        })?;
    Ok(serde_json::json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": message.content.as_deref().unwrap_or(""),
    }))
}

fn serialize_tool(tool: &ProviderToolDefinition) -> Result<serde_json::Value, ProviderError> {
    let ProviderToolDefinition::Function(function) = tool else {
        return Err(ProviderError::UnsupportedFeature {
            feature: "hosted tools on chat_completions".to_string(),
        });
    };
    Ok(serde_json::json!({
        "type": "function",
        "function": {
            "name": function.name,
            "description": function.description,
            "parameters": crate::provider::openai::schema_downlevel::downlevel_function_parameters(
                &function.name,
                &function.parameters,
            ),
            "strict": false,
        },
    }))
}

fn service_tier_provider_value(request: &ProviderRequest) -> Result<Option<String>, ProviderError> {
    let Some(tier) = request.service_tier else {
        return Ok(None);
    };
    let Some(provider_value) = crate::model_catalog::service_tier_provider_value(
        CATALOG_PROVIDER,
        CATALOG_BACKEND,
        &request.model,
        tier.as_str(),
    ) else {
        return Err(ProviderError::InvalidRequest {
            message: format!(
                "service tier '{}' is not supported for model '{}' on {CATALOG_PROVIDER}.{CATALOG_BACKEND}",
                tier.as_str(),
                request.model,
            ),
        });
    };
    Ok(Some(provider_value.to_owned()))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use super::*;
    use crate::provider::request::{
        AssistantToolCall, ProviderContextManagement, ServiceTier, ToolDefinition,
    };
    use crate::provider::tools::{
        HostedToolDefinition, HostedWebSearchTool, ProviderToolDefinition,
    };

    fn base_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![
                Message {
                    response_items: Vec::new(),
                    reasoning: Vec::new(),
                    role: MessageRole::System,
                    content: Some("system".to_owned()),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
                    response_items: Vec::new(),
                    reasoning: Vec::new(),
                    role: MessageRole::Developer,
                    content: Some("developer".to_owned()),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
                Message {
                    response_items: Vec::new(),
                    reasoning: Vec::new(),
                    role: MessageRole::User,
                    content: Some("hello".to_owned()),
                    thinking: String::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_call_kind: None,
                },
            ],
            tools: vec![ProviderToolDefinition::Function(ToolDefinition {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                }),
            })],
            model: "local-model".to_owned(),
            reasoning_effort: Some(ReasoningEffort::Low),
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: Some("session-cache".to_owned()),
            previous_response_id: Some("resp_previous".to_owned()),
            store: true,
            context_management: Some(ProviderContextManagement {
                compact_threshold_tokens: 120_000,
            }),
        }
    }

    #[test]
    fn payload_uses_chat_shape_and_omits_responses_only_fields() {
        let payload = build_payload(&base_request()).unwrap();
        let value = serde_json::to_value(payload).unwrap();

        assert_eq!(value["model"], "local-model");
        assert_eq!(value["stream"], true);
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["reasoning_effort"], "low");
        assert!(value.get("store").is_none());
        assert!(value.get("previous_response_id").is_none());
        assert!(value.get("context_management").is_none());
        assert!(value.get("prompt_cache_key").is_none());

        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[2]["role"], "user");

        let tool = &value["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "read_file");
    }

    #[test]
    fn max_reasoning_effort_uses_canonical_wire_value() {
        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffort::Max);
        let value = serde_json::to_value(build_payload(&request).unwrap()).unwrap();
        assert_eq!(value["reasoning_effort"], "max");
    }

    #[test]
    fn assistant_tool_call_and_result_use_chat_replay_shape() {
        let mut request = base_request();
        request.messages.push(Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            tool_calls: vec![AssistantToolCall {
                call_id: "call_123".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"README.md"}"#.to_owned(),
                kind: ToolCallKind::Function,
            }],
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
        request.messages.push(Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::ToolResult,
            content: Some("contents".to_owned()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_123".to_owned()),
            tool_name: Some("tool-name-secret-must-not-escape".to_owned()),
            tool_call_kind: Some(ToolCallKind::Function),
        });

        let value = serde_json::to_value(build_payload(&request).unwrap()).unwrap();
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["tool_calls"][0]["id"], "call_123");
        assert_eq!(
            messages[3]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[4]["tool_call_id"], "call_123");
    }

    #[test]
    fn missing_tool_result_id_is_hard_error() {
        let mut request = base_request();
        request.messages.push(Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::ToolResult,
            content: Some("contents".to_owned()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: Some("read_file".to_owned()),
            tool_call_kind: Some(ToolCallKind::Function),
        });

        let err = build_payload(&request).unwrap_err();
        assert!(matches!(
            err,
            ProviderError::RequestSerializationFailed { .. }
        ));
        assert!(!err.to_string().contains("tool-name-secret-must-not-escape"));
    }

    #[test]
    fn hosted_tools_fail_closed() {
        let mut request = base_request();
        request.tools = vec![ProviderToolDefinition::Hosted(
            HostedToolDefinition::WebSearch(HostedWebSearchTool::default()),
        )];

        let err = build_payload(&request).unwrap_err();
        assert!(matches!(err, ProviderError::UnsupportedFeature { .. }));
    }

    #[test]
    fn service_tier_uses_compatible_backend_catalog() {
        let mut request = base_request();
        request.service_tier = Some(ServiceTier::Fast);

        let err = build_payload(&request).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn provider_options_merge_advanced_chat_fields() {
        let mut request = base_request();
        request.config = Some(ProviderOptions(serde_json::json!({
            "logprobs": true,
            "top_logprobs": 5,
            "seed": 1234,
            "response_format": {"type": "json_object"}
        })));

        let value = build_payload(&request).unwrap();

        assert_eq!(value["logprobs"], true);
        assert_eq!(value["top_logprobs"], 5);
        assert_eq!(value["seed"], 1234);
        assert_eq!(value["response_format"]["type"], "json_object");
        assert_eq!(value["model"], "local-model");
        assert_eq!(value["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn provider_options_support_scoped_chat_fields() {
        let mut request = base_request();
        request.config = Some(ProviderOptions(serde_json::json!({
            "api_options": {
                "openai_chat_completions": {
                    "logit_bias": {"42": -100},
                    "temperature": 0.2
                }
            }
        })));

        let value = build_payload(&request).unwrap();

        assert_eq!(value["logit_bias"]["42"], -100);
        assert_eq!(value["temperature"], 0.2);
        assert!(value.get("api_options").is_none());
    }

    #[test]
    fn provider_options_reject_protected_chat_fields() {
        let mut request = base_request();
        request.config = Some(ProviderOptions(serde_json::json!({
            "messages": []
        })));

        let err = build_payload(&request).unwrap_err();

        assert!(matches!(err, ProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn chat_completions_never_replays_reasoning_items() {
        // The encrypted-reasoning replay contract is Responses-API-only:
        // an assistant message carrying captured reasoning items (even
        // with encrypted_content) serializes to a plain chat message with
        // no reasoning payload anywhere in the request.
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
        let mut request = base_request();
        request.messages.push(Message {
            response_items: Vec::new(),
            role: MessageRole::Assistant,
            content: Some("answer".to_owned()),
            thinking: "summary".to_owned(),
            reasoning: vec![ReasoningItem {
                id: "rs_1".to_owned(),
                summary: vec![ReasoningSummaryPart::SummaryText {
                    text: "thought".to_owned(),
                }],
                content: None,
                encrypted_content: Some("opaque-blob".to_owned()),
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });

        let value = serde_json::to_value(build_payload(&request).unwrap()).unwrap();
        let serialized = value.to_string();
        assert!(
            !serialized.contains("opaque-blob"),
            "encrypted reasoning must never reach the chat payload: {serialized}"
        );
        assert!(
            !serialized.contains("\"reasoning\""),
            "no reasoning items may be serialized on chat_completions: {serialized}"
        );
        let messages = value["messages"].as_array().unwrap();
        let assistant = messages.last().unwrap();
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "answer");
    }

    #[test]
    fn canonical_response_items_fail_closed_before_chat_serialization() {
        use crate::provider::response_item::{
            ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
        };

        let mut request = base_request();
        request.messages.push(Message {
            response_items: vec![ResponseTranscriptItem {
                item: ResponseItem::from_value(serde_json::json!({
                    "type": "future_response_item",
                    "id": "item_1",
                    "secret_extension": "must-not-be-flattened"
                }))
                .unwrap(),
                provenance: ResponseStreamProvenance::default(),
            }],
            reasoning: Vec::new(),
            role: MessageRole::Assistant,
            content: Some("lossy projection".to_owned()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });

        let error = build_payload(&request).unwrap_err();
        match error {
            ProviderError::UnsupportedFeature { feature } => {
                assert_eq!(
                    feature,
                    "canonical Responses item replay on chat_completions"
                );
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }
}
