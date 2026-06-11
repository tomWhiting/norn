//! MCP client — connects to external Model Context Protocol tool servers,
//! discovers their tools, and registers them in a [`ToolRegistry`] as
//! [`McpProxyTool`] instances.
//!
//! The client implements MCP via JSON-RPC 2.0 over either stdio (a spawned
//! subprocess) or HTTP. The MCP wire shape is small enough that we define
//! it inline rather than pulling in an external crate. Methods supported:
//!
//! - `initialize` — handshake, exchanges protocol version.
//! - `tools/list` — returns `{ tools: [{ name, description, inputSchema }] }`.
//! - `tools/call` — `{ name, arguments }` → `{ content: [...], isError }`.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::error::IntegrationError;
use crate::tool::registry::ToolRegistry;

use super::mcp_proxy::McpProxyTool;

/// Default protocol version sent in the `initialize` handshake.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Wall-clock budget for any single MCP request.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Transport for reaching an MCP server.
#[derive(Clone, Debug)]
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

/// Configuration for one MCP server connection.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// Local logical name (used for diagnostics and proxy-tool prefixing).
    pub name: String,
    /// Transport used to reach the server.
    pub transport: McpTransport,
    /// Environment variables passed through to a stdio server.
    pub env: HashMap<String, String>,
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

// -- JSON-RPC envelopes ----------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest<'a, T: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: T,
}

/// JSON-RPC 2.0 response envelope. Exposed at crate-private visibility so
/// the [`Transport`] trait can name it.
#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcResponse {
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct ToolsListResult {
    #[serde(default)]
    tools: Vec<McpToolDef>,
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

// -- Transport implementations --------------------------------------------

#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn rpc(&self, payload: String) -> Result<JsonRpcResponse, IntegrationError>;
}

struct StdioTransport {
    state: Mutex<StdioState>,
}

struct StdioState {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct HttpTransport {
    client: reqwest::Client,
    url: String,
}

#[async_trait]
impl Transport for StdioTransport {
    async fn rpc(&self, payload: String) -> Result<JsonRpcResponse, IntegrationError> {
        let fut = async {
            let mut state = self.state.lock().await;
            state
                .stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| IntegrationError::McpError {
                    reason: format!("write failed: {e}"),
                })?;
            state
                .stdin
                .write_all(b"\n")
                .await
                .map_err(|e| IntegrationError::McpError {
                    reason: format!("write failed: {e}"),
                })?;
            state
                .stdin
                .flush()
                .await
                .map_err(|e| IntegrationError::McpError {
                    reason: format!("flush failed: {e}"),
                })?;

            let mut line = String::new();
            let read = state.stdout.read_line(&mut line).await.map_err(|e| {
                IntegrationError::McpError {
                    reason: format!("read failed: {e}"),
                }
            })?;
            if read == 0 {
                return Err(IntegrationError::McpError {
                    reason: "server closed stdout".to_owned(),
                });
            }
            serde_json::from_str::<JsonRpcResponse>(line.trim()).map_err(|e| {
                IntegrationError::McpError {
                    reason: format!("invalid JSON-RPC response: {e}: {line}"),
                }
            })
        };

        match tokio::time::timeout(MCP_REQUEST_TIMEOUT, fut).await {
            Ok(r) => r,
            Err(_) => Err(IntegrationError::McpError {
                reason: "MCP request timed out".to_owned(),
            }),
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Best-effort cleanup: kill the subprocess if it's still running.
        if let Ok(mut guard) = self.state.try_lock() {
            let _ = guard.child.start_kill();
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn rpc(&self, payload: String) -> Result<JsonRpcResponse, IntegrationError> {
        let fut = async {
            let resp = self
                .client
                .post(&self.url)
                .header("Content-Type", "application/json")
                .body(payload)
                .send()
                .await
                .map_err(|e| IntegrationError::McpError {
                    reason: format!("HTTP request failed: {e}"),
                })?;
            if !resp.status().is_success() {
                return Err(IntegrationError::McpError {
                    reason: format!("HTTP status {}", resp.status()),
                });
            }
            let text = resp.text().await.map_err(|e| IntegrationError::McpError {
                reason: format!("HTTP body read failed: {e}"),
            })?;
            serde_json::from_str::<JsonRpcResponse>(&text).map_err(|e| IntegrationError::McpError {
                reason: format!("invalid JSON-RPC response: {e}: {text}"),
            })
        };

        match tokio::time::timeout(MCP_REQUEST_TIMEOUT, fut).await {
            Ok(r) => r,
            Err(_) => Err(IntegrationError::McpError {
                reason: "MCP request timed out".to_owned(),
            }),
        }
    }
}

// -- McpClient ------------------------------------------------------------

/// Shared handle backing an [`McpClient`] and every [`McpProxyTool`] it
/// produces. Owns the transport and assigns monotonically increasing
/// JSON-RPC ids. Public so that proxy tools constructed elsewhere can hold
/// the same handle.
pub struct McpClientInner {
    transport: Box<dyn Transport>,
    id_counter: Mutex<u64>,
    server_name: String,
}

impl McpClientInner {
    pub(crate) async fn rpc<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value, IntegrationError> {
        let id = {
            let mut g = self.id_counter.lock().await;
            *g += 1;
            *g
        };
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let payload = serde_json::to_string(&req).map_err(|e| IntegrationError::McpError {
            reason: format!("serialize request: {e}"),
        })?;
        let resp = self.transport.rpc(payload).await?;
        if let Some(err) = resp.error {
            return Err(IntegrationError::McpError {
                reason: format!("server error ({}): {}", err.code, err.message),
            });
        }
        resp.result.ok_or_else(|| IntegrationError::McpError {
            reason: "missing result in JSON-RPC response".to_owned(),
        })
    }
}

/// Client for one MCP server. Multiple clients can talk to multiple servers
/// independently; share one across agents via [`Arc`] when only the
/// [`McpProxyTool`] handles are needed.
pub struct McpClient {
    inner: Arc<McpClientInner>,
    tools: Vec<McpToolDef>,
}

impl McpClient {
    /// Establish a connection per `config`, perform the MCP `initialize`
    /// handshake, and discover tools.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::McpError`] on transport failures, invalid
    /// responses, or server-side errors.
    pub async fn connect(config: McpServerConfig) -> Result<Self, IntegrationError> {
        let transport: Box<dyn Transport> = match &config.transport {
            McpTransport::Stdio { command, args } => {
                let mut cmd = Command::new(command);
                cmd.args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                for (k, v) in &config.env {
                    cmd.env(k, v);
                }
                let mut child = cmd.spawn().map_err(|e| IntegrationError::McpError {
                    reason: format!("failed to spawn MCP server '{command}': {e}"),
                })?;
                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| IntegrationError::McpError {
                        reason: "stdin handle missing on spawned MCP server".to_owned(),
                    })?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| IntegrationError::McpError {
                        reason: "stdout handle missing on spawned MCP server".to_owned(),
                    })?;
                Box::new(StdioTransport {
                    state: Mutex::new(StdioState {
                        child,
                        stdin,
                        stdout: BufReader::new(stdout),
                    }),
                })
            }
            McpTransport::Http { url } => {
                let client = reqwest::Client::builder()
                    .timeout(MCP_REQUEST_TIMEOUT)
                    .build()
                    .map_err(|e| IntegrationError::McpError {
                        reason: format!("failed to build HTTP client: {e}"),
                    })?;
                Box::new(HttpTransport {
                    client,
                    url: url.clone(),
                })
            }
        };

        let inner = Arc::new(McpClientInner {
            transport,
            id_counter: Mutex::new(0),
            server_name: config.name.clone(),
        });

        let init_params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "norn",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let _ = inner.rpc("initialize", init_params).await?;

        // Best-effort initialized notification. Failure here is non-fatal —
        // some servers do not expect it.
        let _ = inner
            .rpc("notifications/initialized", serde_json::json!({}))
            .await;

        let mut client = Self {
            inner,
            tools: Vec::new(),
        };
        client.tools = client.discover_tools().await?;
        Ok(client)
    }

    /// Build a client from a transport already wrapped in
    /// [`Arc<McpClientInner>`] form. Test-only constructor.
    #[cfg(test)]
    fn from_transport(name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            inner: Arc::new(McpClientInner {
                transport,
                id_counter: Mutex::new(0),
                server_name: name.into(),
            }),
            tools: Vec::new(),
        }
    }

    /// Logical server name from the config.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.server_name
    }

    /// Tool definitions discovered during `connect`.
    #[must_use]
    pub fn tools(&self) -> &[McpToolDef] {
        &self.tools
    }

    /// Re-fetch the tool list from the server.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::McpError`] on transport failures or
    /// malformed responses.
    pub async fn discover_tools(&self) -> Result<Vec<McpToolDef>, IntegrationError> {
        let value = self.inner.rpc("tools/list", serde_json::json!({})).await?;
        let parsed: ToolsListResult =
            serde_json::from_value(value).map_err(|e| IntegrationError::McpError {
                reason: format!("invalid tools/list response: {e}"),
            })?;
        Ok(parsed.tools)
    }

    /// Wrap every discovered tool in an [`McpProxyTool`] and register them
    /// in `registry`. The returned count tells the caller how many tools
    /// were registered.
    pub fn register_tools(&self, registry: &mut ToolRegistry) -> usize {
        let mut n = 0;
        for tool in &self.tools {
            registry.register(Box::new(McpProxyTool::new(
                tool.clone(),
                Arc::clone(&self.inner),
            )));
            n += 1;
        }
        n
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
    use super::*;
    use std::sync::Mutex as StdMutex;

    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::traits::Tool;

    fn list_response(tools: serde_json::Value) -> JsonRpcResponse {
        JsonRpcResponse {
            result: Some(serde_json::json!({"tools": tools})),
            error: None,
        }
    }

    /// Scripted in-process transport — for each request, returns the next
    /// canned response in order. Used to verify discovery + dispatch.
    struct ScriptedTransport {
        responses: StdMutex<Vec<JsonRpcResponse>>,
        seen_methods: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn rpc(&self, payload: String) -> Result<JsonRpcResponse, IntegrationError> {
            // Capture the method for assertion.
            let parsed: serde_json::Value =
                serde_json::from_str(&payload).expect("payload is JSON");
            let method = parsed
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            self.seen_methods.lock().unwrap().push(method);

            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(IntegrationError::McpError {
                    reason: "no scripted responses left".to_owned(),
                });
            }
            Ok(responses.remove(0))
        }
    }

    #[tokio::test]
    async fn discovery_populates_registry() {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![list_response(serde_json::json!([
                {"name": "alpha", "description": "first", "inputSchema": {"type": "object"}},
                {"name": "beta", "description": "second", "inputSchema": {"type": "object"}},
            ]))]),
            seen_methods: StdMutex::new(Vec::new()),
        });

        let mut client = McpClient::from_transport("test", transport);
        client.tools = client.discover_tools().await.unwrap();
        assert_eq!(client.tools().len(), 2);
        assert_eq!(client.tools()[0].name, "alpha");
        assert_eq!(client.tools()[1].name, "beta");

        let mut registry = ToolRegistry::new();
        let n = client.register_tools(&mut registry);
        assert_eq!(n, 2);
        assert!(registry.get("alpha").is_some());
        assert!(registry.get("beta").is_some());
    }

    #[tokio::test]
    async fn proxy_tool_forwards_call() {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![
                list_response(serde_json::json!([
                    {"name": "echo", "description": "echo back", "inputSchema": {"type": "object"}}
                ])),
                JsonRpcResponse {
                    result: Some(serde_json::json!({
                        "content": [{"type": "text", "text": "hello world"}],
                        "isError": false
                    })),
                    error: None,
                },
            ]),
            seen_methods: StdMutex::new(Vec::new()),
        });

        let mut client = McpClient::from_transport("test", transport);
        client.tools = client.discover_tools().await.unwrap();
        let mut registry = ToolRegistry::new();
        client.register_tools(&mut registry);

        let tool = registry.get("echo").expect("echo registered");
        let envelope = ToolEnvelope {
            tool_call_id: "tc_1".to_owned(),
            tool_name: "echo".to_owned(),
            model_args: serde_json::json!({"text": "hello"}),
            runtime_inputs: crate::tool::envelope::RuntimeInputs::default(),
            metadata: serde_json::Value::Null,
        };
        let ctx = ToolContext::empty();
        let output = tool.execute(&envelope, &ctx).await.unwrap();
        assert_eq!(output.content["text"], "hello world");
        assert!(!output.is_error());
    }

    #[tokio::test]
    async fn server_error_propagates() {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![JsonRpcResponse {
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: "no such tool".to_owned(),
                }),
            }]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let client = McpClient::from_transport("test", transport);
        let err = client.discover_tools().await.unwrap_err();
        match err {
            IntegrationError::McpError { reason } => assert!(reason.contains("no such tool")),
            other => panic!("expected McpError, got {other:?}"),
        }
    }

    #[test]
    fn mcp_proxy_tool_implements_tool() {
        // Static coercion — compiles only if the trait is implemented.
        fn _assert_object_safe(_t: Box<dyn Tool + Send + Sync>) {}
        let def = McpToolDef {
            name: "x".to_owned(),
            description: "y".to_owned(),
            input_schema: serde_json::json!({}),
        };
        let inner = Arc::new(McpClientInner {
            transport: Box::new(ScriptedTransport {
                responses: StdMutex::new(Vec::new()),
                seen_methods: StdMutex::new(Vec::new()),
            }),
            id_counter: Mutex::new(0),
            server_name: "test".to_owned(),
        });
        let tool = McpProxyTool::new(def, inner);
        _assert_object_safe(Box::new(tool));
    }
}
