//! JSON-RPC envelopes used by the MCP client transports.

use serde::{Deserialize, Serialize};

use super::mcp_types::McpToolDef;

#[derive(Serialize)]
pub(crate) struct JsonRpcRequest<'a, T: Serialize> {
    pub(crate) jsonrpc: &'static str,
    pub(crate) id: u64,
    pub(crate) method: &'a str,
    pub(crate) params: T,
}

#[derive(Serialize)]
pub(crate) struct JsonRpcNotification<'a, T: Serialize> {
    pub(crate) jsonrpc: &'static str,
    pub(crate) method: &'a str,
    pub(crate) params: T,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcResponse {
    #[serde(default)]
    pub(crate) jsonrpc: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) result: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
}

#[derive(Deserialize)]
pub(crate) struct ToolsListResult {
    #[serde(default)]
    pub(crate) tools: Vec<McpToolDef>,
    #[serde(default, rename = "nextCursor")]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitializeResult {
    pub(crate) protocol_version: String,
    pub(crate) capabilities: ServerCapabilities,
    pub(crate) server_info: ServerInfo,
}

#[derive(Deserialize)]
pub(crate) struct ServerCapabilities {
    #[serde(default)]
    pub(crate) tools: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct ServerInfo {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct ToolsCallResult {
    #[serde(default)]
    pub(crate) content: Vec<ToolCallContent>,
    #[serde(default, rename = "isError")]
    pub(crate) is_error: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ToolCallContent {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Other,
}
