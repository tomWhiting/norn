//! Owned set of connected MCP clients for one agent runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{McpClient, McpClientConfig};
use crate::error::IntegrationError;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;

#[path = "mcp_runtime_candidate.rs"]
mod candidate;
pub use candidate::{McpRuntimeCandidate, McpRuntimeServerState, McpRuntimeServerStatus};

/// Keeps connected servers alive and installs their qualified proxy tools.
pub struct McpRuntime {
    pub(super) clients: BTreeMap<String, Arc<McpClient>>,
    pub(super) failures: BTreeMap<String, String>,
    pub(super) statuses: BTreeMap<String, McpRuntimeServerStatus>,
}

impl McpRuntime {
    /// Empty connected runtime used before the first live server is added.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            clients: BTreeMap::new(),
            failures: BTreeMap::new(),
            statuses: BTreeMap::new(),
        }
    }

    /// Connect selected servers independently and retain both healthy clients
    /// and per-server failures.
    pub async fn connect(configs: impl IntoIterator<Item = McpClientConfig>) -> Self {
        let mut configs: Vec<_> = configs.into_iter().collect();
        configs.sort_by(|left, right| left.name.cmp(&right.name));
        let attempts = configs.into_iter().map(|config| async move {
            let name = config.name.clone();
            (name, McpClient::connect(config).await)
        });
        let mut clients = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for (name, result) in futures_util::future::join_all(attempts).await {
            match result {
                Ok(client) => {
                    clients.insert(name, Arc::new(client));
                }
                Err(error) => {
                    failures.insert(name, error.to_string());
                }
            }
        }
        Self {
            clients,
            failures,
            statuses: BTreeMap::new(),
        }
    }

    /// Number of connected servers, including servers with no tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Whether no server is connected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Connected server names in deterministic order.
    pub fn server_names(&self) -> impl Iterator<Item = &str> {
        self.clients.keys().map(String::as_str)
    }

    pub(crate) fn tool_change_subscriptions(
        &self,
    ) -> Vec<(String, u64, tokio::sync::watch::Receiver<u64>)> {
        self.clients
            .iter()
            .map(|(name, client)| {
                (
                    name.clone(),
                    client.instance_id(),
                    client.subscribe_tool_list_changes(),
                )
            })
            .collect()
    }

    pub(crate) async fn refreshed_tools(
        &self,
        name: &str,
        instance_id: u64,
    ) -> Result<Option<Arc<Self>>, IntegrationError> {
        let Some(client) = self.clients.get(name) else {
            return Ok(None);
        };
        if client.instance_id() != instance_id {
            return Ok(None);
        }
        let refreshed = Arc::new(client.refreshed_tools().await?);
        let mut clients = self.clients.clone();
        clients.insert(name.to_owned(), refreshed);
        Ok(Some(Arc::new(Self {
            clients,
            failures: self.failures.clone(),
            statuses: self.statuses.clone(),
        })))
    }

    /// Failed server names and diagnostics in deterministic order.
    pub fn failures(&self) -> impl Iterator<Item = (&str, &str)> {
        self.failures
            .iter()
            .map(|(name, reason)| (name.as_str(), reason.as_str()))
    }

    /// Per-server outcomes for snapshot-backed runtime generations.
    pub fn server_statuses(&self) -> impl Iterator<Item = &McpRuntimeServerStatus> {
        self.statuses.values()
    }

    /// Look up one snapshot-backed server outcome.
    #[must_use]
    pub fn server_status(&self, name: &str) -> Option<&McpRuntimeServerStatus> {
        self.statuses.get(name)
    }

    /// Provider-facing tool names belonging to a selected server subset.
    pub fn tool_names_for_servers(
        &self,
        servers: &[String],
    ) -> Result<Vec<String>, IntegrationError> {
        let mut names = Vec::new();
        for server in servers {
            if let Some(client) = self.clients.get(server) {
                names.extend(client.qualified_tool_names());
            } else if !self.knows_server(server) {
                return Err(IntegrationError::McpError {
                    reason: format!("MCP server selection names unknown server '{server}'"),
                });
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Every provider-facing MCP tool name in the connected pool.
    pub fn tool_names(&self) -> Vec<String> {
        self.clients
            .values()
            .flat_map(|client| client.qualified_tool_names())
            .collect()
    }

    /// Build every proxy tool in the connected pool.
    pub fn proxy_tools(&self) -> Vec<Box<dyn Tool + Send + Sync>> {
        self.clients
            .values()
            .flat_map(|client| client.proxy_tools())
            .collect()
    }

    /// Build proxy tools for a selected server view. A configured server that
    /// failed to connect contributes no tools; an unknown name is a typed
    /// configuration error.
    pub fn proxy_tools_for_servers(
        &self,
        servers: &[String],
    ) -> Result<Vec<Box<dyn Tool + Send + Sync>>, IntegrationError> {
        let mut tools = Vec::new();
        for server in servers {
            if let Some(client) = self.clients.get(server) {
                tools.extend(client.proxy_tools());
            } else if !self.knows_server(server) {
                return Err(IntegrationError::McpError {
                    reason: format!("MCP server selection names unknown server '{server}'"),
                });
            }
        }
        Ok(tools)
    }

    /// Narrow a registry's current MCP surface while leaving non-MCP tools
    /// and pre-existing profile gates unchanged.
    pub fn restrict_registry_to_servers(
        &self,
        registry: &mut ToolRegistry,
        servers: &[String],
    ) -> Result<(), IntegrationError> {
        let all: std::collections::BTreeSet<_> = self.tool_names().into_iter().collect();
        let selected: std::collections::BTreeSet<_> =
            self.tool_names_for_servers(servers)?.into_iter().collect();
        let available = registry
            .names()
            .filter(|name| !all.contains(*name) || selected.contains(*name))
            .map(str::to_owned)
            .collect();
        registry.set_available(available);
        Ok(())
    }

    /// Register every discovered proxy only if none collides with an
    /// existing Norn, custom, or MCP tool name.
    pub fn register_tools(&self, registry: &mut ToolRegistry) -> Result<usize, IntegrationError> {
        let proxies: Vec<_> = self
            .clients
            .values()
            .flat_map(|client| client.proxy_tools())
            .collect();
        let mut names = std::collections::BTreeSet::new();
        for name in proxies.iter().map(|tool| tool.name()) {
            if !names.insert(name) {
                return Err(IntegrationError::McpError {
                    reason: format!("multiple MCP tools resolve to provider name '{name}'"),
                });
            }
            if registry.is_registered(name) {
                return Err(IntegrationError::McpError {
                    reason: format!("MCP tool name '{name}' collides with an existing tool"),
                });
            }
        }
        let count = proxies.len();
        for proxy in proxies {
            registry.register(proxy);
        }
        Ok(count)
    }

    fn knows_server(&self, name: &str) -> bool {
        self.clients.contains_key(name)
            || self.failures.contains_key(name)
            || self.statuses.contains_key(name)
    }
}

impl std::fmt::Debug for McpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpRuntime")
            .field("server_names", &self.clients.keys().collect::<Vec<_>>())
            .field(
                "failed_server_names",
                &self.failures.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "mcp_runtime_tests.rs"]
pub(crate) mod tests;
