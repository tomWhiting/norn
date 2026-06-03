//! LLM provider abstraction and implementations.

pub use self::agent_event::{AgentEvent, AgentEventSender, SharedAgentEventChannel};
pub use self::auth::{
    ApiKeyAuthProvider, AuthProvider, AuthSource, LoginConfig, OAuthAuthProvider,
    build_from_auth_source, login, logout,
};
pub use self::events::{ProviderEvent, StopReason};
pub use self::request::{
    AssistantToolCall, Message, MessageRole, ProviderConfig, ProviderOptions, ProviderRequest,
    ReasoningEffort, ReasoningSummary, SecretString, ToolDefinition,
};
pub use self::tools::{
    HostedToolDefinition, HostedWebSearchTool, ProviderCapabilities, ProviderToolDefinition,
    WebSearchContentType, WebSearchContextSize, WebSearchFilters, WebSearchUserLocation,
    WebSearchUserLocationType, resolve_provider_tools,
};
pub use self::traits::{Provider, ProviderStream};
pub use self::usage::Usage;
pub use crate::error::ProviderError;

pub mod agent_event;
pub mod auth;
pub mod debug;
pub mod events;
pub mod mock;
pub mod openai;
pub mod openai_oauth;
pub mod request;
pub mod tools;
pub mod traits;
pub mod usage;
