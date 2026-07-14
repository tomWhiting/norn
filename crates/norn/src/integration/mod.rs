//! Integration with Claude Runner, MCP, Rhai, hooks, extensions, session
//! variables, and diagnostics.

pub use crate::error::IntegrationError;

pub mod claude;
pub mod diagnostics;
pub mod extensions;
pub mod hooks;
mod mcp_candidate_builder;
pub mod mcp_client;
#[cfg(test)]
mod mcp_context_call_tests;
pub mod mcp_control;
mod mcp_http;
pub mod mcp_live_command;
mod mcp_protocol;
pub mod mcp_proxy;
pub mod mcp_runtime;
mod mcp_runtime_store;
pub mod mcp_server;
mod mcp_stdio;
mod mcp_transport_bounds;
mod mcp_types;
mod mcp_wire;
pub mod rhai;
pub mod variables;

pub use claude::{
    ClaudeRunnerAdapter, ClaudeRunnerConfig, NornWrappedClaudeCode, NornWrappedClaudeConfig,
    StepOutcome,
};
pub use diagnostics::{DiagnosticCollector, DiagnosticSeverity, NornDiagnostic};
pub use extensions::{
    ExtToolDef, ExtTransport, ExtensionManifest, ExtensionProxyTool, ExtensionRegistry,
};
pub use hooks::{
    CompactionHook, Hook, HookContext, HookInput, HookOutcome, HookOutput, HookRegistry,
    LlmCallSummary, NORN_AGENT_ID, NORN_HOOK_EVENT, NORN_PROFILE, NORN_PROJECT_DIR,
    NORN_SESSION_ID, PostLlmHook, PostToolFailureHook, PostToolHook, PreLlmHook, PreToolHook,
    SessionEventHook, SessionLifecycleHook, ShellCommandHook, StopHook, SubagentHook,
    UserPromptHook,
};
pub use mcp_candidate_builder::McpRuntimeCandidateBuilder;
pub use mcp_client::{
    MCP_PROTOCOL_VERSION, McpClient, McpClientInner, McpServerConfig as McpClientConfig,
    McpToolDef, McpTransport,
};
pub use mcp_control::{
    McpActivationCandidate, McpActivationRequest, McpCandidateBuilder, McpCandidateError,
    McpControlError, McpControlHandle, McpControlResponse, McpMutationResult, McpServerDetails,
    McpServerStatus,
};
pub use mcp_live_command::{
    LIVE_MCP_HELP, LiveMcpCommand, LiveMcpCommandError, execute_live_mcp_command,
    is_live_mcp_definition_input, parse_live_mcp_command,
};
pub use mcp_protocol::McpRoot;
pub use mcp_proxy::McpProxyTool;
pub use mcp_runtime::McpRuntime;
pub use mcp_runtime_store::{McpRuntimeSnapshot, McpRuntimeStore};
pub use mcp_server::{McpServer, McpServerConfig};
pub use mcp_types::DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES;
pub use rhai::{
    AgentHandle, NornRhaiContext, build_norn_engine, eval_with_args, register_norn_builtins,
};
pub use variables::{SessionVariable, VariableSource, VariableStore, expand};
