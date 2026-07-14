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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::{IntegrationError, McpRemoteError};
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;

use super::mcp_protocol::{ClientProtocolState, McpRoot};
use super::mcp_proxy::McpProxyTool;
#[cfg(test)]
use super::mcp_types::DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES;
pub use super::mcp_types::{McpServerConfig, McpToolDef, McpTransport};
use super::mcp_wire::{InitializeResult, JsonRpcNotification, JsonRpcRequest, ToolsListResult};
pub(crate) use super::mcp_wire::{JsonRpcResponse, ToolCallContent, ToolsCallResult};

#[path = "mcp_client_transport.rs"]
mod transport_support;
pub(crate) use transport_support::Transport;
use transport_support::{ClientRequestGuard, unusable_client_error};

#[path = "mcp_client_schema.rs"]
mod tool_schema;
use tool_schema::validate_discovered_tools;

/// Default protocol version sent in the `initialize` handshake.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

static CLIENT_INSTANCE: AtomicU64 = AtomicU64::new(1);

fn validate_client_settings(config: &McpServerConfig) -> Result<(), IntegrationError> {
    if config.max_inbound_message_bytes == 0 {
        return Err(IntegrationError::McpInvalidClientSetting {
            setting: "max_inbound_message_bytes",
        });
    }
    if config.request_timeout_ms == Some(0) {
        return Err(IntegrationError::McpInvalidClientSetting {
            setting: "request_timeout_ms",
        });
    }
    Ok(())
}

// -- McpClient ------------------------------------------------------------

/// Shared handle backing an [`McpClient`] and every [`McpProxyTool`] it
/// produces. Owns the transport and assigns monotonically increasing
/// JSON-RPC ids. Public so that proxy tools constructed elsewhere can hold
/// the same handle.
pub struct McpClientInner {
    transport: Box<dyn Transport>,
    id_counter: Mutex<u64>,
    context_call: Mutex<()>,
    server_name: String,
    protocol: Arc<ClientProtocolState>,
    instance_id: u64,
    live: AtomicBool,
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
        if !self.is_live() {
            return Err(unusable_client_error());
        }
        let mut request = ClientRequestGuard::new(&self.live);
        let response = self.transport.request(payload, id).await;
        request.finish_if(response.is_ok());
        let resp = response?;
        if resp.jsonrpc.as_deref() != Some("2.0") {
            self.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: "MCP response did not declare JSON-RPC 2.0".to_owned(),
            });
        }
        if resp.id.as_ref() != Some(&serde_json::json!(id)) {
            self.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: format!("JSON-RPC response id did not match request {id}"),
            });
        }
        if resp.result.is_some() == resp.error.is_some() {
            self.invalidate().await;
            return Err(IntegrationError::McpError {
                reason: "MCP response must contain exactly one of result or error".to_owned(),
            });
        }
        if let Some(err) = resp.error {
            return Err(McpRemoteError::new(err.code, err.message).into());
        }
        resp.result.ok_or_else(|| IntegrationError::McpError {
            reason: "missing result in JSON-RPC response".to_owned(),
        })
    }

    pub(crate) async fn rpc_with_roots<P: Serialize>(
        &self,
        roots: Vec<McpRoot>,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value, IntegrationError> {
        let _context_call = self.context_call.lock().await;
        self.set_roots_unlocked(roots).await?;
        self.rpc(method, params).await
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
        if !self.is_live() {
            return Err(unusable_client_error());
        }
        let mut request = ClientRequestGuard::new(&self.live);
        let result = self.transport.notify(payload).await;
        request.finish_if(result.is_ok());
        result
    }

    pub(crate) async fn set_roots(&self, roots: Vec<McpRoot>) -> Result<bool, IntegrationError> {
        let _context_call = self.context_call.lock().await;
        self.set_roots_unlocked(roots).await
    }

    async fn set_roots_unlocked(&self, roots: Vec<McpRoot>) -> Result<bool, IntegrationError> {
        let previous = self.protocol.roots()?;
        if !self.protocol.replace_roots(roots)? {
            return Ok(false);
        }
        if let Err(error) = self
            .notify("notifications/roots/list_changed", serde_json::json!({}))
            .await
        {
            self.protocol.replace_roots(previous.to_vec())?;
            return Err(error);
        }
        Ok(true)
    }

    fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire) && self.transport.is_live()
    }

    async fn invalidate(&self) {
        self.live.store(false, Ordering::Release);
        self.transport.invalidate().await;
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
        let roots = config
            .working_dir
            .as_deref()
            .map(McpRoot::from_path)
            .transpose()?
            .into_iter()
            .collect();
        Self::connect_with_roots(config, roots).await
    }

    /// Establish a connection with the initial contextual roots advertised by
    /// `roots/list`. Roots do not grant filesystem authority.
    pub async fn connect_with_roots(
        config: McpServerConfig,
        roots: Vec<McpRoot>,
    ) -> Result<Self, IntegrationError> {
        validate_client_settings(&config)?;
        let protocol = Arc::new(ClientProtocolState::new(roots));
        let transport: Box<dyn Transport> = match &config.transport {
            McpTransport::Stdio { command, args } => {
                Box::new(super::mcp_stdio::StdioTransport::spawn(
                    command,
                    args,
                    &config.env,
                    config.working_dir.as_deref(),
                    Arc::clone(&protocol),
                    config.max_inbound_message_bytes,
                    config.request_timeout_ms,
                )?)
            }
            McpTransport::Http { url } => Box::new(super::mcp_http::HttpTransport::new(
                url.clone(),
                &config.headers,
                Arc::clone(&protocol),
                config.max_inbound_message_bytes,
                config.request_timeout_ms,
            )?),
        };

        let inner = Arc::new(McpClientInner {
            transport,
            id_counter: Mutex::new(0),
            context_call: Mutex::new(()),
            server_name: config.name.clone(),
            protocol,
            instance_id: CLIENT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            live: AtomicBool::new(true),
        });

        let init_params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"roots": {"listChanged": true}},
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
            inner.invalidate().await;
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
            client.tools = client.discover_tools().await?;
        }
        Ok(client)
    }

    /// Build a client from a transport already wrapped in
    /// [`Arc<McpClientInner>`] form. Test-only constructor.
    #[cfg(test)]
    pub(crate) fn from_transport(name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            inner: Arc::new(McpClientInner {
                transport,
                id_counter: Mutex::new(0),
                context_call: Mutex::new(()),
                server_name: name.into(),
                protocol: Arc::new(ClientProtocolState::new(Vec::new())),
                instance_id: CLIENT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                live: AtomicBool::new(true),
            }),
            tools: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_tools(mut self, tools: Vec<McpToolDef>) -> Self {
        self.tools = tools;
        self
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

    /// Replace the contextual roots and notify the server when they changed.
    ///
    /// # Errors
    ///
    /// Returns an MCP transport error if the change notification cannot be
    /// delivered. The previous local root view is restored before returning.
    pub async fn set_roots(&self, roots: Vec<McpRoot>) -> Result<bool, IntegrationError> {
        self.inner.set_roots(roots).await
    }

    /// Snapshot the contextual roots currently advertised to this server.
    pub fn roots(&self) -> Result<Arc<[McpRoot]>, IntegrationError> {
        self.inner.protocol.roots()
    }

    /// Subscribe to monotonic `notifications/tools/list_changed` revisions.
    pub fn subscribe_tool_list_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.protocol.subscribe_tool_list_changes()
    }

    pub(crate) async fn refreshed_tools(&self) -> Result<Self, IntegrationError> {
        let tools = match self.discover_tools().await {
            Ok(tools) => tools,
            Err(error) => {
                self.inner.invalidate().await;
                return Err(error);
            }
        };
        Ok(Self {
            inner: Arc::clone(&self.inner),
            tools,
        })
    }

    #[cfg(test)]
    pub(crate) fn notify_tool_list_changed_for_test(&self) -> Result<(), IntegrationError> {
        self.inner
            .protocol
            .inspect(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed"
            }))
            .map(|_message| ())
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if
)]
mod tests {
    use super::super::mcp_wire::JsonRpcError;
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
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

    struct CancellationTransport {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Transport for CancellationTransport {
        async fn request(
            &self,
            _payload: String,
            _request_id: u64,
        ) -> Result<JsonRpcResponse, IntegrationError> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
            Ok(())
        }

        fn supports_protocol_version(&self, _version: &str) -> bool {
            true
        }
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
    async fn cancelling_an_inflight_request_marks_the_client_dead() {
        let started = Arc::new(tokio::sync::Notify::new());
        let client = Arc::new(McpClient::from_transport(
            "cancelled",
            Box::new(CancellationTransport {
                started: Arc::clone(&started),
            }),
        ));
        let task_client = Arc::clone(&client);
        let task = tokio::spawn(async move { task_client.discover_tools().await });
        started.notified().await;
        task.abort();
        let joined = task.await;

        assert!(joined.is_err_and(|error| error.is_cancelled()));
        assert!(!client.is_live());
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
            max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
            request_timeout_ms: Some(2500),
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains("env-secret-sentinel"));
        assert!(!rendered.contains("header-secret-sentinel"));
        assert!(rendered.contains("env_entries: 1"));
        assert!(rendered.contains("header_entries: 1"));
        assert!(rendered.contains("max_inbound_message_bytes: 10485760"));
        assert!(rendered.contains("request_timeout_ms: Some(2500)"));
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
            IntegrationError::McpRemote(remote) => {
                assert_eq!(remote.code(), -32000);
                assert_eq!(remote.untrusted_message(), "no such tool");
            }
            other => panic!("expected McpRemote, got {other:?}"),
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
            context_call: Mutex::new(()),
            server_name: "test".to_owned(),
            protocol: Arc::new(ClientProtocolState::new(Vec::new())),
            instance_id: CLIENT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            live: AtomicBool::new(true),
        });
        let tool = McpProxyTool::new("test", def, inner);
        _assert_object_safe(Box::new(tool));
    }
}

#[cfg(test)]
#[path = "mcp_client_protocol_tests.rs"]
mod protocol_tests;
