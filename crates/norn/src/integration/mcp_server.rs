//! MCP server — exposes the tools of a [`ToolRegistry`] over the standard
//! Model Context Protocol so external agents (Claude Code included) can
//! call them.
//!
//! The server implements three JSON-RPC methods:
//!
//! - `initialize` — handshake.
//! - `tools/list` — enumerate registered tools.
//! - `tools/call` — dispatch the full Norn lifecycle for a named tool.
//!
//! Transport is handled at the boundary: [`McpServer::serve_stdio`] runs a
//! request/response loop on `stdin`/`stdout`; [`McpServer::handle_message`]
//! is the pure dispatch entry point that any transport can drive.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::integration::mcp_client::{MCP_PROTOCOL_VERSION, McpTransport};
use crate::r#loop::runner::ToolExecutor;
use crate::tool::registry::ToolRegistry;

/// Configuration for an [`McpServer`].
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// Transport on which the server listens. Stdio runs on the process's
    /// stdin/stdout; Http is reserved for future implementations.
    pub transport: McpTransport,
    /// Server name returned during `initialize`.
    pub server_name: String,
    /// Server version returned during `initialize`.
    pub server_version: String,
}

/// MCP server backed by a shared [`ToolRegistry`].
pub struct McpServer {
    registry: Arc<ToolRegistry>,
    config: McpServerConfig,
}

#[derive(Deserialize, Debug)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcResponse<'a> {
    jsonrpc: &'static str,
    id: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl McpServer {
    /// Build a new server holding a shared reference to `registry`.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, config: McpServerConfig) -> Self {
        Self { registry, config }
    }

    /// Logical server name.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.config.server_name
    }

    /// Handle a single JSON-RPC request body and produce the JSON response
    /// body. Always returns a syntactically valid JSON-RPC response; the
    /// `error` field is populated when dispatch fails.
    pub async fn handle_message(&self, body: &str) -> String {
        let null_id = serde_json::Value::Null;
        let request: JsonRpcRequest = match serde_json::from_str(body) {
            Ok(req) => req,
            Err(e) => {
                return Self::error_response(&null_id, -32700, format!("parse error: {e}"));
            }
        };
        // Notifications (no id) get acknowledged with an empty response.
        let id = request.id.clone().unwrap_or(serde_json::Value::Null);

        if request.jsonrpc.as_deref() != Some("2.0") && request.jsonrpc.is_some() {
            return Self::error_response(&id, -32600, "unsupported jsonrpc version".to_owned());
        }

        match request.method.as_str() {
            "initialize" => self.handle_initialize(&id),
            "notifications/initialized" => Self::ok_response(&id, serde_json::json!({})),
            "tools/list" => self.handle_tools_list(&id),
            "tools/call" => self.handle_tools_call(&id, request.params).await,
            other => Self::error_response(&id, -32601, format!("method not found: {other}")),
        }
    }

    fn handle_initialize(&self, id: &serde_json::Value) -> String {
        let result = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": self.config.server_name,
                "version": self.config.server_version,
            }
        });
        Self::ok_response(id, result)
    }

    fn handle_tools_list(&self, id: &serde_json::Value) -> String {
        let mut tools = Vec::new();
        for name in self.registry.names() {
            if let Some(tool) = self.registry.get(name) {
                tools.push(serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "inputSchema": tool.input_schema(),
                }));
            }
        }
        Self::ok_response(id, serde_json::json!({"tools": tools}))
    }

    async fn handle_tools_call(&self, id: &serde_json::Value, params: serde_json::Value) -> String {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_owned(),
            None => {
                return Self::error_response(id, -32602, "missing 'name' parameter".to_owned());
            }
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let executor: &dyn ToolExecutor = self.registry.as_ref();
        let call_id = id
            .as_str()
            .map_or_else(|| id.to_string(), std::borrow::ToOwned::to_owned);
        match executor.execute(&name, &call_id, arguments).await {
            Ok(content) => {
                let text = serde_json::to_string(&content).unwrap_or_default();
                let result = serde_json::json!({
                    "content": [
                        {"type": "text", "text": text}
                    ],
                    "isError": false
                });
                Self::ok_response(id, result)
            }
            Err(err) => {
                let result = serde_json::json!({
                    "content": [
                        {"type": "text", "text": err.to_string()}
                    ],
                    "isError": true
                });
                Self::ok_response(id, result)
            }
        }
    }

    fn ok_response(id: &serde_json::Value, result: serde_json::Value) -> String {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        };
        serde_json::to_string(&resp).unwrap_or_else(|e| {
            format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"serialize failed: {e}"}}}}"#)
        })
    }

    fn error_response(id: &serde_json::Value, code: i64, message: String) -> String {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        };
        serde_json::to_string(&resp).unwrap_or_else(|e| {
            format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"serialize failed: {e}"}}}}"#)
        })
    }

    /// Serve MCP on the process's stdin/stdout, reading newline-delimited
    /// JSON-RPC requests until EOF.
    ///
    /// # Errors
    ///
    /// Propagates [`std::io::Error`] from the underlying I/O.
    pub async fn serve_stdio(&self) -> std::io::Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response = self.handle_message(trimmed).await;
            stdout.write_all(response.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        Ok(())
    }
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
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::error::ToolError;
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::{Tool, ToolOutput};

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "echoes the input"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}}
            })
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(
            &self,
            envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let msg = envelope
                .model_args
                .get("message")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(ToolOutput {
                content: serde_json::json!({"echoed": msg}),
                is_error: false,
                duration: Duration::ZERO,
            })
        }
    }

    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing"
        }

        fn description(&self) -> &str {
            "always fails"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed {
                reason: "boom".to_owned(),
            })
        }
    }

    fn make_server() -> McpServer {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        registry.register(Box::new(FailingTool));
        McpServer::new(
            Arc::new(registry),
            McpServerConfig {
                transport: McpTransport::Stdio {
                    command: String::new(),
                    args: Vec::new(),
                },
                server_name: "norn-mcp".to_owned(),
                server_version: "test".to_owned(),
            },
        )
    }

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test]
    async fn initialize_returns_protocol_and_server_info() {
        let server = make_server();
        let resp = server
            .handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .await;
        let v = parse(&resp);
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["serverInfo"]["name"], "norn-mcp");
        assert_eq!(v["result"]["serverInfo"]["version"], "test");
        assert_eq!(v["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    // R4 acceptance: tools/list returns all registered tool definitions.
    #[tokio::test]
    async fn tools_list_returns_registered_tools() {
        let server = make_server();
        let resp = server
            .handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#)
            .await;
        let v = parse(&resp);
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let names: std::collections::HashSet<&str> =
            tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains("echo"));
        assert!(names.contains("failing"));
        let echo = tools.iter().find(|t| t["name"] == "echo").unwrap();
        assert_eq!(echo["description"], "echoes the input");
        assert!(echo["inputSchema"].is_object());
    }

    // R4 acceptance: tools/call dispatches through the full Tool lifecycle.
    #[tokio::test]
    async fn tools_call_dispatches_through_registry() {
        let server = make_server();
        let resp = server
            .handle_message(
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"message":"hi"}}}"#,
            )
            .await;
        let v = parse(&resp);
        assert_eq!(v["result"]["isError"], false);
        let content = &v["result"]["content"][0];
        assert_eq!(content["type"], "text");
        let body: serde_json::Value =
            serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["echoed"], "hi");
    }

    #[tokio::test]
    async fn tools_call_failure_returns_is_error() {
        let server = make_server();
        let resp = server
            .handle_message(
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"failing","arguments":{}}}"#,
            )
            .await;
        let v = parse(&resp);
        assert_eq!(v["result"]["isError"], true);
        let content = &v["result"]["content"][0];
        assert!(content["text"].as_str().unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let server = make_server();
        let resp = server
            .handle_message(r#"{"jsonrpc":"2.0","id":5,"method":"does/not/exist"}"#)
            .await;
        let v = parse(&resp);
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn malformed_body_returns_parse_error() {
        let server = make_server();
        let resp = server.handle_message("not json").await;
        let v = parse(&resp);
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_missing_name_errors() {
        let server = make_server();
        let resp = server
            .handle_message(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}"#)
            .await;
        let v = parse(&resp);
        assert_eq!(v["error"]["code"], -32602);
    }
}
