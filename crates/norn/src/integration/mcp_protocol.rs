//! Shared MCP client protocol state and inbound message handling.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use tokio::sync::watch;

use crate::error::IntegrationError;

/// A contextual URI advertised to an MCP server through `roots/list`.
///
/// Roots help a server understand the client's current working context. They
/// do not grant filesystem access and are not a Norn confinement boundary.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct McpRoot {
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl McpRoot {
    /// Create a root from an absolute URI and an optional display name.
    pub fn new(uri: impl Into<String>, name: Option<String>) -> Result<Self, IntegrationError> {
        let uri = uri.into();
        url::Url::parse(&uri).map_err(|_error| IntegrationError::McpError {
            reason: "MCP root must be an absolute URI".to_owned(),
        })?;
        Ok(Self { uri, name })
    }

    /// Create a contextual `file:` root from an absolute directory path.
    pub fn from_path(path: &Path) -> Result<Self, IntegrationError> {
        if !path.is_absolute() {
            return Err(IntegrationError::McpError {
                reason: "MCP root paths must be absolute".to_owned(),
            });
        }
        let uri = url::Url::from_directory_path(path).map_err(|()| IntegrationError::McpError {
            reason: "MCP root path could not be represented as a file URI".to_owned(),
        })?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned);
        Self::new(uri.to_string(), name)
    }

    /// Absolute URI sent to the MCP server.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Optional human-readable root name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

impl fmt::Debug for McpRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpRoot")
            .field("uri", &"[REDACTED]")
            .field("name", &self.name.as_ref().map(|_name| "[REDACTED]"))
            .finish()
    }
}

pub(crate) enum InboundMessage {
    Response,
    Consumed,
    Reply(serde_json::Value),
}

pub(crate) struct ClientProtocolState {
    roots: RwLock<Arc<[McpRoot]>>,
    tool_list_revision: AtomicU64,
    tool_list_changes: watch::Sender<u64>,
}

impl ClientProtocolState {
    pub(crate) fn new(roots: Vec<McpRoot>) -> Self {
        let (tool_list_changes, _receiver) = watch::channel(0);
        Self {
            roots: RwLock::new(roots.into()),
            tool_list_revision: AtomicU64::new(0),
            tool_list_changes,
        }
    }

    pub(crate) fn replace_roots(&self, roots: Vec<McpRoot>) -> Result<bool, IntegrationError> {
        let mut current = self
            .roots
            .write()
            .map_err(|_poisoned| protocol_state_error())?;
        if current.as_ref() == roots.as_slice() {
            return Ok(false);
        }
        *current = roots.into();
        Ok(true)
    }

    pub(crate) fn roots(&self) -> Result<Arc<[McpRoot]>, IntegrationError> {
        self.roots
            .read()
            .map(|roots| Arc::clone(&roots))
            .map_err(|_poisoned| protocol_state_error())
    }

    pub(crate) fn subscribe_tool_list_changes(&self) -> watch::Receiver<u64> {
        self.tool_list_changes.subscribe()
    }

    pub(crate) fn inspect(
        &self,
        message: &serde_json::Value,
    ) -> Result<InboundMessage, IntegrationError> {
        let Some(method) = message.get("method").and_then(serde_json::Value::as_str) else {
            return Ok(InboundMessage::Response);
        };
        if message.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
            return Err(IntegrationError::McpError {
                reason: "MCP server message did not declare JSON-RPC 2.0".to_owned(),
            });
        }
        let Some(id) = message.get("id") else {
            if method == "notifications/tools/list_changed" {
                let revision = self.tool_list_revision.fetch_add(1, Ordering::AcqRel) + 1;
                self.tool_list_changes.send_replace(revision);
            }
            return Ok(InboundMessage::Consumed);
        };
        if !(id.is_string() || id.is_number() || id.is_null()) {
            return Err(IntegrationError::McpError {
                reason: "MCP server request contained an invalid JSON-RPC id".to_owned(),
            });
        }
        let reply = match method {
            "ping" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "roots/list" => {
                let roots = self.roots()?;
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"roots": roots}})
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not supported by norn"}
            }),
        };
        Ok(InboundMessage::Reply(reply))
    }
}

fn protocol_state_error() -> IntegrationError {
    IntegrationError::McpError {
        reason: "MCP client protocol state is unavailable".to_owned(),
    }
}

#[cfg(test)]
#[path = "mcp_protocol_tests.rs"]
mod tests;
