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
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::IntegrationError;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;

use super::mcp_proxy::McpProxyTool;

/// Default protocol version sent in the `initialize` handshake.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Wall-clock budget for any single MCP request.
pub(super) const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Local logical name (used for diagnostics and proxy-tool prefixing).
    pub name: String,
    /// Transport used to reach the server.
    pub transport: McpTransport,
    /// Environment variables passed through to a stdio server.
    pub env: HashMap<String, String>,
    /// HTTP headers attached to every remote-transport request.
    pub headers: HashMap<String, String>,
    /// Working directory supplied explicitly to a stdio server.
    pub working_dir: Option<PathBuf>,
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

// -- JSON-RPC envelopes ----------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest<'a, T: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: T,
}

#[derive(Serialize)]
struct JsonRpcNotification<'a, T: Serialize> {
    jsonrpc: &'static str,
    method: &'a str,
    params: T,
}

/// JSON-RPC 2.0 response envelope. Exposed at crate-private visibility so
/// the [`Transport`] trait can name it.
#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcResponse {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<serde_json::Value>,
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
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    protocol_version: String,
    capabilities: ServerCapabilities,
    server_info: ServerInfo,
}

#[derive(Deserialize)]
struct ServerCapabilities {
    #[serde(default)]
    tools: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ServerInfo {
    name: String,
    version: String,
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
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError>;
    async fn notify(&self, payload: String) -> Result<(), IntegrationError>;
    fn supports_protocol_version(&self, version: &str) -> bool;
    async fn set_protocol_version(&self, _version: &str) {}
    async fn invalidate(&self) {}
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
        let resp = self.transport.request(payload, id).await?;
        if resp.jsonrpc.as_deref() != Some("2.0") {
            self.transport.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: "MCP response did not declare JSON-RPC 2.0".to_owned(),
            });
        }
        if resp.id.as_ref() != Some(&serde_json::json!(id)) {
            self.transport.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: format!("JSON-RPC response id did not match request {id}"),
            });
        }
        if resp.result.is_some() == resp.error.is_some() {
            return Err(IntegrationError::McpError {
                reason: "MCP response must contain exactly one of result or error".to_owned(),
            });
        }
        if let Some(err) = resp.error {
            return Err(IntegrationError::McpError {
                reason: format!("server error ({}): {}", err.code, err.message),
            });
        }
        resp.result.ok_or_else(|| IntegrationError::McpError {
            reason: "missing result in JSON-RPC response".to_owned(),
        })
    }

    async fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), IntegrationError> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };
        let payload =
            serde_json::to_string(&notification).map_err(|error| IntegrationError::McpError {
                reason: format!("serialize notification: {error}"),
            })?;
        self.transport.notify(payload).await
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
                Box::new(super::mcp_stdio::StdioTransport::spawn(
                    command,
                    args,
                    &config.env,
                    config.working_dir.as_deref(),
                )?)
            }
            McpTransport::Http { url } => Box::new(super::mcp_http::HttpTransport::new(
                url.clone(),
                &config.headers,
            )?),
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
        let init_value = inner.rpc("initialize", init_params).await?;
        let initialized: InitializeResult =
            serde_json::from_value(init_value).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid MCP initialize result: {error}"),
            })?;
        if !inner
            .transport
            .supports_protocol_version(&initialized.protocol_version)
        {
            inner.transport.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: format!(
                    "MCP server selected unsupported protocol version '{}'",
                    initialized.protocol_version,
                ),
            });
        }
        inner
            .transport
            .set_protocol_version(&initialized.protocol_version)
            .await;
        tracing::debug!(
            server = %config.name,
            implementation = %initialized.server_info.name,
            version = %initialized.server_info.version,
            protocol = %initialized.protocol_version,
            "MCP server initialized",
        );

        inner
            .notify("notifications/initialized", serde_json::json!({}))
            .await?;

        let mut client = Self {
            inner,
            tools: Vec::new(),
        };
        if initialized.capabilities.tools.is_some() {
            client.tools = tokio::time::timeout(MCP_REQUEST_TIMEOUT, client.discover_tools())
                .await
                .map_err(|_elapsed| IntegrationError::McpError {
                    reason: "MCP tool discovery exceeded the request deadline".to_owned(),
                })??;
        }
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
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = std::collections::HashSet::new();
        loop {
            let params = cursor.as_ref().map_or_else(
                || serde_json::json!({}),
                |value| serde_json::json!({"cursor": value}),
            );
            let value = self.inner.rpc("tools/list", params).await?;
            let parsed: ToolsListResult =
                serde_json::from_value(value).map_err(|error| IntegrationError::McpError {
                    reason: format!("invalid tools/list response: {error}"),
                })?;
            tools.extend(parsed.tools);
            let Some(next_cursor) = parsed.next_cursor else {
                validate_discovered_tools(&tools)?;
                return Ok(tools);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(IntegrationError::McpError {
                    reason: "MCP tools/list repeated a pagination cursor".to_owned(),
                });
            }
            cursor = Some(next_cursor);
        }
    }

    /// Wrap every discovered tool in an [`McpProxyTool`] and register them
    /// in `registry`. The returned count tells the caller how many tools
    /// were registered.
    pub fn register_tools(&self, registry: &mut ToolRegistry) -> usize {
        let mut n = 0;
        for tool in &self.tools {
            registry.register(Box::new(McpProxyTool::new(
                self.name(),
                tool.clone(),
                Arc::clone(&self.inner),
            )));
            n += 1;
        }
        n
    }

    /// Build server-qualified proxy tools without registering them.
    pub fn proxy_tools(&self) -> Vec<Box<dyn Tool + Send + Sync>> {
        self.tools
            .iter()
            .map(|tool| {
                Box::new(McpProxyTool::new(
                    self.name(),
                    tool.clone(),
                    Arc::clone(&self.inner),
                )) as Box<dyn Tool + Send + Sync>
            })
            .collect()
    }

    pub(crate) fn qualified_tool_names(&self) -> impl Iterator<Item = String> + '_ {
        self.tools
            .iter()
            .map(|tool| super::mcp_proxy::qualified_tool_name(self.name(), tool.name.as_str()))
    }
}

fn validate_discovered_tools(tools: &[McpToolDef]) -> Result<(), IntegrationError> {
    let mut names = std::collections::HashSet::new();
    for tool in tools {
        if tool.name.trim().is_empty() {
            return Err(IntegrationError::McpError {
                reason: "MCP tools/list returned an empty tool name".to_owned(),
            });
        }
        if !names.insert(tool.name.as_str()) {
            return Err(IntegrationError::McpError {
                reason: format!("MCP tools/list returned duplicate tool '{}'", tool.name),
            });
        }
        if schema_uses_envelope_key(&tool.input_schema) {
            return Err(IntegrationError::McpError {
                reason: format!(
                    "MCP tool '{}' uses a Norn-reserved tool envelope property",
                    tool.name,
                ),
            });
        }
    }
    Ok(())
}

fn schema_uses_envelope_key(schema: &serde_json::Value) -> bool {
    let declares = |value: &serde_json::Value| {
        value
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key(crate::tool::envelope::ENVELOPE_DESCRIPTION_KEY)
                    || properties.contains_key(crate::tool::envelope::ENVELOPE_METADATA_KEY)
            })
    };
    declares(schema)
        || schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|variants| variants.iter().any(declares))
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
    clippy::collapsible_if
)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::traits::Tool;

    fn scripted_error(reason: impl Into<String>) -> IntegrationError {
        IntegrationError::McpError {
            reason: reason.into(),
        }
    }

    fn require_error<T>(
        result: Result<T, IntegrationError>,
        context: &str,
    ) -> Result<IntegrationError, std::io::Error> {
        match result {
            Err(error) => Ok(error),
            Ok(_) => Err(std::io::Error::other(context.to_owned())),
        }
    }

    fn list_response(tools: serde_json::Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: None,
            result: Some(serde_json::json!({"tools": tools})),
            error: None,
        }
    }

    fn paged_list_response(tools: serde_json::Value, next_cursor: Option<&str>) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: None,
            result: Some(serde_json::json!({
                "tools": tools,
                "nextCursor": next_cursor,
            })),
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
        async fn request(
            &self,
            payload: String,
            _request_id: u64,
        ) -> Result<JsonRpcResponse, IntegrationError> {
            // Capture the method for assertion.
            let parsed: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|error| scripted_error(format!("invalid scripted request: {error}")))?;
            let method = parsed
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            self.seen_methods
                .lock()
                .map_err(|_poisoned| scripted_error("scripted method lock was poisoned"))?
                .push(method);

            let mut responses = self
                .responses
                .lock()
                .map_err(|_poisoned| scripted_error("scripted response lock was poisoned"))?;
            if responses.is_empty() {
                return Err(IntegrationError::McpError {
                    reason: "no scripted responses left".to_owned(),
                });
            }
            let mut response = responses.remove(0);
            if response.id.is_none() {
                response.id = parsed.get("id").cloned();
            }
            Ok(response)
        }

        async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
            let parsed: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
                scripted_error(format!("invalid scripted notification: {error}"))
            })?;
            assert!(
                parsed.get("id").is_none(),
                "notification must not carry an id"
            );
            let method = parsed
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            self.seen_methods
                .lock()
                .map_err(|_poisoned| scripted_error("scripted method lock was poisoned"))?
                .push(method);
            Ok(())
        }

        fn supports_protocol_version(&self, _version: &str) -> bool {
            true
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
        let alpha = super::super::mcp_proxy::qualified_tool_name("test", "alpha");
        let beta = super::super::mcp_proxy::qualified_tool_name("test", "beta");
        assert!(registry.get(&alpha).is_some());
        assert!(registry.get(&beta).is_some());
    }

    #[tokio::test]
    async fn discovery_follows_every_page() -> Result<(), IntegrationError> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![
                paged_list_response(serde_json::json!([{"name": "alpha"}]), Some("page-2")),
                paged_list_response(serde_json::json!([{"name": "beta"}]), None),
            ]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let client = McpClient::from_transport("test", transport);

        let tools = client.discover_tools().await?;

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "alpha");
        assert_eq!(tools[1].name, "beta");
        Ok(())
    }

    #[tokio::test]
    async fn repeated_pagination_cursor_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![
                paged_list_response(serde_json::json!([]), Some("same")),
                paged_list_response(serde_json::json!([]), Some("same")),
            ]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let client = McpClient::from_transport("test", transport);

        let error = require_error(
            client.discover_tools().await,
            "repeated pagination cursor was accepted",
        )?;

        assert!(error.to_string().contains("repeated a pagination cursor"));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_and_reserved_tool_schemas_are_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let duplicate = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![list_response(serde_json::json!([
                {"name": "same"},
                {"name": "same"}
            ]))]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let duplicate_client = McpClient::from_transport("test", duplicate);
        let duplicate_error = require_error(
            duplicate_client.discover_tools().await,
            "duplicate MCP tool names were accepted",
        )?;
        assert!(duplicate_error.to_string().contains("duplicate tool"));

        let reserved = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![list_response(serde_json::json!([{
                "name": "reserved",
                "inputSchema": {
                    "type": "object",
                    "properties": {"tool_use_description": {"type": "string"}}
                }
            }]))]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let reserved_client = McpClient::from_transport("test", reserved);
        let reserved_error = require_error(
            reserved_client.discover_tools().await,
            "reserved MCP tool schema property was accepted",
        )?;
        assert!(
            reserved_error
                .to_string()
                .contains("reserved tool envelope")
        );
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_response_id_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![JsonRpcResponse {
                jsonrpc: Some("2.0".to_owned()),
                id: Some(serde_json::json!(999)),
                result: Some(serde_json::json!({"tools": []})),
                error: None,
            }]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let client = McpClient::from_transport("test", transport);

        let error = require_error(
            client.discover_tools().await,
            "mismatched MCP response id was accepted",
        )?;

        assert!(error.to_string().contains("did not match request"));
        Ok(())
    }

    #[tokio::test]
    async fn notification_has_no_id_and_consumes_no_response() -> Result<(), IntegrationError> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(Vec::new()),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let inner = McpClient::from_transport("test", transport).inner;

        inner
            .notify("notifications/initialized", serde_json::json!({}))
            .await?;
        Ok(())
    }

    #[test]
    fn server_config_debug_redacts_environment_and_headers() {
        let config = McpServerConfig {
            name: "example".to_owned(),
            transport: McpTransport::Http {
                url: "https://example.test/mcp".to_owned(),
            },
            env: HashMap::from([("TOKEN".to_owned(), "env-secret-sentinel".to_owned())]),
            headers: HashMap::from([(
                "Authorization".to_owned(),
                "header-secret-sentinel".to_owned(),
            )]),
            working_dir: None,
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains("env-secret-sentinel"));
        assert!(!rendered.contains("header-secret-sentinel"));
        assert!(rendered.contains("env_entries: 1"));
        assert!(rendered.contains("header_entries: 1"));
    }

    #[tokio::test]
    async fn proxy_tool_forwards_call() -> Result<(), Box<dyn std::error::Error>> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![
                list_response(serde_json::json!([
                    {"name": "echo", "description": "echo back", "inputSchema": {"type": "object"}}
                ])),
                JsonRpcResponse {
                    jsonrpc: Some("2.0".to_owned()),
                    id: None,
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
        client.tools = client.discover_tools().await?;
        let mut registry = ToolRegistry::new();
        client.register_tools(&mut registry);

        let qualified = super::super::mcp_proxy::qualified_tool_name("test", "echo");
        let tool = registry
            .get(&qualified)
            .ok_or_else(|| std::io::Error::other("qualified echo tool was not registered"))?;
        let envelope = ToolEnvelope {
            tool_call_id: "tc_1".to_owned(),
            tool_name: qualified,
            model_args: serde_json::json!({"text": "hello"}),
            metadata: serde_json::Value::Null,
        };
        let ctx = ToolContext::empty();
        let output = tool.execute(&envelope, &ctx).await?;
        assert_eq!(output.content["text"], "hello world");
        assert!(!output.is_error());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_tool_result_is_not_reported_as_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![
                list_response(serde_json::json!([{"name": "broken"}])),
                JsonRpcResponse {
                    jsonrpc: Some("2.0".to_owned()),
                    id: None,
                    result: Some(serde_json::json!({"content": "not-an-array"})),
                    error: None,
                },
            ]),
            seen_methods: StdMutex::new(Vec::new()),
        });
        let mut client = McpClient::from_transport("test", transport);
        client.tools = client.discover_tools().await?;
        let mut registry = ToolRegistry::new();
        client.register_tools(&mut registry);
        let qualified = super::super::mcp_proxy::qualified_tool_name("test", "broken");
        let tool = registry
            .get(&qualified)
            .ok_or_else(|| std::io::Error::other("qualified broken tool was not registered"))?;
        let result = tool
            .execute(
                &ToolEnvelope {
                    tool_call_id: "tc_broken".to_owned(),
                    tool_name: qualified,
                    model_args: serde_json::json!({}),
                    metadata: serde_json::Value::Null,
                },
                &ToolContext::empty(),
            )
            .await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn server_error_propagates() {
        let transport = Box::new(ScriptedTransport {
            responses: StdMutex::new(vec![JsonRpcResponse {
                jsonrpc: Some("2.0".to_owned()),
                id: None,
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
        let tool = McpProxyTool::new("test", def, inner);
        _assert_object_safe(Box::new(tool));
    }
}
