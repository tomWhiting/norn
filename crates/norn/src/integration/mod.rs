//! Integration with Claude Runner, MCP, Rhai, hooks, extensions, session
//! variables, and diagnostics.

pub use crate::error::IntegrationError;

pub mod claude;
pub mod diagnostics;
pub mod extensions;
pub mod hooks;
pub mod mcp_client;
mod mcp_http;
pub mod mcp_proxy;
pub mod mcp_runtime;
pub mod mcp_server;
mod mcp_stdio;
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
pub use mcp_client::{
    MCP_PROTOCOL_VERSION, McpClient, McpClientInner, McpServerConfig as McpClientConfig,
    McpToolDef, McpTransport,
};
pub use mcp_proxy::McpProxyTool;
pub use mcp_runtime::McpRuntime;
pub use mcp_server::{McpServer, McpServerConfig};
pub use rhai::{
    AgentHandle, NornRhaiContext, build_norn_engine, eval_with_args, register_norn_builtins,
};
pub use variables::{SessionVariable, VariableSource, VariableStore, expand};
