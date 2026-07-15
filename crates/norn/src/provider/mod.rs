//! LLM provider abstraction and implementations.

pub use self::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AGENT_MESSAGE_SENT_EVENT_TYPE, AgentEvent, AgentEventKind,
    AgentEventSender, AgentMessageLifecycle, AgentStreamRetry, AgentUsageEstimate,
    SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE, SharedAgentEventChannel,
    SubagentDescriptor, SubagentKind, SubagentLifecycle,
};
pub use self::api_shape::{
    ApiShape, ApiShapeParseError, ProviderProfileId, ProviderProfileIdError,
};
pub use self::auth::{
    ApiKeyAuthProvider, AuthProvider, AuthSource, LoginConfig, OAuthAuthProvider,
    build_from_auth_source, command_account_root, list_auth_accounts, login, login_named, logout,
    logout_all_auth_accounts, logout_named, provider_account_root, use_auth_account,
};
pub use self::events::{ProviderEvent, StopReason};
pub use self::reasoning::{ReasoningContentPart, ReasoningItem, ReasoningSummaryPart};
pub use self::request::{
    AssistantToolCall, Message, MessageRole, ProviderConfig, ProviderOptions, ProviderRequest,
    ReasoningEffort, ReasoningSummary, SecretString, ServiceTier, ToolDefinition,
};
pub use self::surface::{
    ResolvedTool, ResolvedToolSurface, ToolPresentation, collect_function_definitions,
    hosted_surface_description, hosted_surface_usage, hosted_tools_prompt_section,
    reframe_catalog_entries, reframe_prompt_entries, resolve_tool_presentation,
};
pub use self::tools::{
    HostedToolDefinition, HostedWebSearchTool, ProviderCapabilities, ProviderToolDefinition,
    WebSearchContentType, WebSearchContextSize, WebSearchFilters, WebSearchUserLocation,
    WebSearchUserLocationType,
};
pub use self::traits::{Provider, ProviderStream};
pub use self::usage::Usage;
pub use crate::error::ProviderError;

pub mod agent_event;
pub mod api_shape;
pub mod auth;
pub mod debug;
pub(crate) mod endpoint;
pub mod events;
pub(crate) mod exec;
pub(crate) mod http_client;
pub mod mock;
pub mod openai;
pub mod openai_compatible;
pub mod openai_oauth;
pub mod reasoning;
pub mod request;
pub(crate) mod startup_trace;
pub mod surface;
pub mod tools;
pub mod traits;
pub mod usage;
