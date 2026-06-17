//! Provider wire/API shape identifiers.
//!
//! These names describe the request/response contract a provider runtime
//! serializes to. They are deliberately separate from provider products,
//! deployment profiles, and model identifiers: the same provider can expose
//! multiple API shapes, and the same model can have different capabilities
//! through different shapes.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Known provider API shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiShape {
    /// `OpenAI` Responses-style request and SSE event stream.
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// OpenAI-compatible Chat Completions request and stream chunks.
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    /// Anthropic Messages request and event stream.
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    /// `OpenAI` Harmony prompt/response format for direct gpt-oss inference.
    #[serde(rename = "openai_harmony")]
    OpenAiHarmony,
    /// LM Studio native REST API shape.
    #[serde(rename = "lmstudio_native")]
    LmStudioNative,
    /// Typed process/RPC adapter for agent runtimes.
    #[serde(rename = "agent_rpc")]
    AgentRpc,
    /// Agent Client Protocol surface integration.
    #[serde(rename = "agent_client_protocol")]
    AgentClientProtocol,
}

impl ApiShape {
    /// Stable config/catalog identifier for this API shape.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenAiHarmony => "openai_harmony",
            Self::LmStudioNative => "lmstudio_native",
            Self::AgentRpc => "agent_rpc",
            Self::AgentClientProtocol => "agent_client_protocol",
        }
    }
}

impl FromStr for ApiShape {
    type Err = ApiShapeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai_responses" | "openai-responses" | "responses" => Ok(Self::OpenAiResponses),
            "openai_chat_completions"
            | "openai-chat-completions"
            | "chat_completions"
            | "chat-completions" => Ok(Self::OpenAiChatCompletions),
            "anthropic_messages" | "anthropic-messages" | "messages" => Ok(Self::AnthropicMessages),
            "openai_harmony" | "openai-harmony" | "harmony" => Ok(Self::OpenAiHarmony),
            "lmstudio_native" | "lmstudio-native" => Ok(Self::LmStudioNative),
            "agent_rpc" | "agent-rpc" => Ok(Self::AgentRpc),
            "agent_client_protocol" | "agent-client-protocol" | "acp" => {
                Ok(Self::AgentClientProtocol)
            }
            other => Err(ApiShapeParseError {
                value: other.to_owned(),
            }),
        }
    }
}

/// Error returned when parsing an unknown [`ApiShape`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown API shape '{value}'")]
pub struct ApiShapeParseError {
    value: String,
}

/// Provider profile identifier.
///
/// A provider profile names a configured deployment/auth target such as a
/// Codex subscription, `OpenAI` API-key account, LM Studio server, or internal
/// gateway. It is not the same thing as an API shape or model ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderProfileId(String);

impl ProviderProfileId {
    /// Creates a provider profile id after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderProfileIdError`] when the identifier is empty or
    /// contains control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderProfileIdError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ProviderProfileIdError::Empty);
        }
        if trimmed.chars().any(char::is_control) {
            return Err(ProviderProfileIdError::ControlCharacter);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Returns the stable profile identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validation error for [`ProviderProfileId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderProfileIdError {
    /// Empty profile id.
    #[error("provider profile id must not be empty")]
    Empty,
    /// Profile id contains a control character.
    #[error("provider profile id must not contain control characters")]
    ControlCharacter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_shape_parse_accepts_primary_ids() {
        assert_eq!(
            "openai_chat_completions".parse::<ApiShape>(),
            Ok(ApiShape::OpenAiChatCompletions),
        );
        assert_eq!(
            "anthropic_messages".parse::<ApiShape>(),
            Ok(ApiShape::AnthropicMessages),
        );
    }

    #[test]
    fn provider_profile_rejects_empty() {
        assert!(matches!(
            ProviderProfileId::new(" "),
            Err(ProviderProfileIdError::Empty),
        ));
    }
}
