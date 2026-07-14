//! Public MCP client configuration and discovered-tool types.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default maximum size of one inbound MCP JSON-RPC message or SSE event.
///
/// This matches the 10 MiB stdio buffer limit in the official MCP TypeScript
/// SDK v1 while remaining overridable for each server definition.
pub const DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

/// Transport for reaching an MCP server.
#[derive(Clone)]
pub enum McpTransport {
    /// Spawn `command` with `args` and exchange JSON-RPC over stdio.
    Stdio {
        /// Executable name or absolute path.
        command: String,
        /// Arguments passed to the executable.
        args: Vec<String>,
    },
    /// Reach the server via HTTP at `url`.
    Http {
        /// Base URL of the MCP endpoint that accepts JSON-RPC POSTs.
        url: String,
    },
}

impl fmt::Debug for McpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio { args, .. } => formatter
                .debug_struct("Stdio")
                .field("command_present", &true)
                .field("args_count", &args.len())
                .finish(),
            Self::Http { .. } => formatter
                .debug_struct("Http")
                .field("url_present", &true)
                .finish(),
        }
    }
}

/// Configuration for one MCP server connection.
#[derive(Clone)]
pub struct McpServerConfig {
    /// Local logical name used for diagnostics and proxy-tool prefixing.
    pub name: String,
    /// Transport used to reach the server.
    pub transport: McpTransport,
    /// Environment variables passed through to a stdio server.
    pub env: HashMap<String, String>,
    /// HTTP headers attached to every remote-transport request.
    pub headers: HashMap<String, String>,
    /// Working directory supplied explicitly to a stdio server.
    pub working_dir: Option<PathBuf>,
    /// Maximum accepted bytes for one inbound JSON-RPC message or SSE event.
    pub max_inbound_message_bytes: usize,
    /// Optional per-request deadline in milliseconds. `None` means no
    /// client-side timeout.
    pub request_timeout_ms: Option<u64>,
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field("env_entries", &self.env.len())
            .field("header_entries", &self.headers.len())
            .field("working_dir_present", &self.working_dir.is_some())
            .field("max_inbound_message_bytes", &self.max_inbound_message_bytes)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .finish()
    }
}

/// Tool definition discovered from an MCP server.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpToolDef {
    /// Tool identifier.
    pub name: String,
    /// Description shown to the model.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the tool arguments.
    #[serde(default = "empty_schema", rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

fn empty_schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}
