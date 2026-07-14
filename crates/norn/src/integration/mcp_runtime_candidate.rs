//! Immutable incremental candidates for live MCP runtime publication.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use super::McpRuntime;
use crate::config::{
    EffectiveMcpServer, McpConfigLayer, McpConfigSnapshot, McpDefinitionFingerprint,
};
use crate::error::IntegrationError;
use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpTransport,
};
use crate::tool::traits::Tool;

/// Connection outcome for one effective MCP server definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpRuntimeServerState {
    /// The effective definition has a connected client.
    Connected,
    /// The effective definition was disabled and was not connected.
    Disabled,
    /// The effective definition failed validation or connection.
    Failed,
}

/// Immutable status for one server in a runtime generation.
#[derive(Clone, PartialEq, Eq)]
pub struct McpRuntimeServerStatus {
    name: String,
    source: McpConfigLayer,
    fingerprint: McpDefinitionFingerprint,
    state: McpRuntimeServerState,
    failure: Option<String>,
}

impl McpRuntimeServerStatus {
    /// Logical configured server name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Configuration layer that supplied the effective definition.
    #[must_use]
    pub const fn source(&self) -> McpConfigLayer {
        self.source
    }

    /// Stable identity of the effective definition.
    #[must_use]
    pub const fn fingerprint(&self) -> &McpDefinitionFingerprint {
        &self.fingerprint
    }

    /// Connection outcome for this runtime generation.
    #[must_use]
    pub const fn state(&self) -> McpRuntimeServerState {
        self.state
    }

    /// Failure diagnostic, present only when [`Self::state`] is failed.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub(super) fn failed_liveness(mut status: Self) -> Self {
        status.state = McpRuntimeServerState::Failed;
        status.failure = Some("the MCP client connection is no longer live".to_owned());
        status
    }

    pub(super) fn failed_refresh(mut self, failure: String) -> Self {
        self.state = McpRuntimeServerState::Failed;
        self.failure = Some(failure);
        self
    }
}

impl std::fmt::Debug for McpRuntimeServerStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpRuntimeServerStatus")
            .field("name", &self.name)
            .field("source", &self.source)
            .field("fingerprint", &self.fingerprint)
            .field("state", &self.state)
            .field("failure_present", &self.failure.is_some())
            .finish()
    }
}

/// Fully assembled, unpublished MCP runtime generation.
pub struct McpRuntimeCandidate {
    runtime: McpRuntime,
}

impl McpRuntime {
    /// Assemble an immutable candidate from one effective config snapshot.
    /// Healthy clients are retained only when their definition fingerprint
    /// is unchanged. Changed and new definitions connect independently.
    pub async fn build_candidate(
        &self,
        snapshot: &McpConfigSnapshot,
        working_dir: &Path,
    ) -> McpRuntimeCandidate {
        build_candidate(self, snapshot, working_dir, McpClient::connect).await
    }

    #[cfg(test)]
    pub(crate) fn from_test_statuses(
        entries: Vec<(EffectiveMcpServer, McpRuntimeServerState, Option<String>)>,
    ) -> Self {
        let mut failures = BTreeMap::new();
        let statuses = entries
            .into_iter()
            .map(|(server, state, failure)| {
                if let Some(reason) = failure.as_ref() {
                    failures.insert(server.name().to_owned(), reason.clone());
                }
                (server.name().to_owned(), status(&server, state, failure))
            })
            .collect();
        Self {
            clients: BTreeMap::new(),
            failures,
            statuses,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_connected_servers(
        entries: Vec<(EffectiveMcpServer, McpClient)>,
    ) -> Self {
        let mut clients = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        for (server, client) in entries {
            clients.insert(server.name().to_owned(), Arc::new(client));
            statuses.insert(
                server.name().to_owned(),
                status(&server, McpRuntimeServerState::Connected, None),
            );
        }
        Self {
            clients,
            failures: BTreeMap::new(),
            statuses,
        }
    }

    #[cfg(test)]
    pub(super) async fn build_candidate_with<C, F>(
        &self,
        snapshot: &McpConfigSnapshot,
        working_dir: &Path,
        connect: C,
    ) -> McpRuntimeCandidate
    where
        C: Fn(McpClientConfig) -> F,
        F: Future<Output = Result<McpClient, IntegrationError>>,
    {
        build_candidate(self, snapshot, working_dir, connect).await
    }

    fn reusable_client(&self, server: &EffectiveMcpServer) -> Option<Arc<McpClient>> {
        self.statuses
            .get(server.name())
            .filter(|status| {
                status.state == McpRuntimeServerState::Connected
                    && status.fingerprint == *server.fingerprint()
            })
            .and_then(|_status| self.clients.get(server.name()))
            .filter(|client| client.is_live())
            .cloned()
    }

    pub(crate) fn with_config_snapshot(&self, snapshot: &McpConfigSnapshot) -> Self {
        let mut statuses = BTreeMap::new();
        for server in snapshot.iter() {
            let outcome = if self.clients.contains_key(server.name()) {
                Some((McpRuntimeServerState::Connected, None))
            } else if let Some(reason) = self.failures.get(server.name()) {
                Some((McpRuntimeServerState::Failed, Some(reason.clone())))
            } else if !server.enabled() {
                Some((McpRuntimeServerState::Disabled, None))
            } else {
                None
            };
            if let Some((state, failure)) = outcome {
                statuses.insert(server.name().to_owned(), status(server, state, failure));
            }
        }
        Self {
            clients: self.clients.clone(),
            failures: self.failures.clone(),
            statuses,
        }
    }
}

impl McpRuntimeCandidate {
    /// Inspect the unpublished runtime without making it active.
    #[must_use]
    pub const fn runtime(&self) -> &McpRuntime {
        &self.runtime
    }

    /// Consume the candidate at the controller's atomic publication point.
    #[must_use]
    pub fn into_runtime(self) -> McpRuntime {
        self.runtime
    }

    /// Per-server outcomes in deterministic server-name order.
    pub fn server_statuses(&self) -> impl Iterator<Item = &McpRuntimeServerStatus> {
        self.runtime.server_statuses()
    }

    /// Failed server names and diagnostics in deterministic order.
    pub fn failures(&self) -> impl Iterator<Item = (&str, &str)> {
        self.runtime.failures()
    }

    /// Build every proxy tool in this candidate.
    pub fn proxy_tools(&self) -> Vec<Box<dyn Tool + Send + Sync>> {
        self.runtime.proxy_tools()
    }

    /// Build proxies for a selected server view.
    pub fn proxy_tools_for_servers(
        &self,
        servers: &[String],
    ) -> Result<Vec<Box<dyn Tool + Send + Sync>>, IntegrationError> {
        self.runtime.proxy_tools_for_servers(servers)
    }
}

async fn build_candidate<C, F>(
    current: &McpRuntime,
    snapshot: &McpConfigSnapshot,
    working_dir: &Path,
    connect: C,
) -> McpRuntimeCandidate
where
    C: Fn(McpClientConfig) -> F,
    F: Future<Output = Result<McpClient, IntegrationError>>,
{
    let mut clients = BTreeMap::new();
    let mut failures = BTreeMap::new();
    let mut statuses = BTreeMap::new();
    let mut attempts = Vec::new();

    for server in snapshot.iter() {
        if !server.enabled() {
            statuses.insert(
                server.name().to_owned(),
                status(server, McpRuntimeServerState::Disabled, None),
            );
            continue;
        }
        if let Some(client) = current.reusable_client(server) {
            clients.insert(server.name().to_owned(), client);
            statuses.insert(
                server.name().to_owned(),
                status(server, McpRuntimeServerState::Connected, None),
            );
            continue;
        }
        match client_config(server, working_dir) {
            Ok(config) => {
                let name = server.name().to_owned();
                let source = server.source();
                let fingerprint = server.fingerprint().clone();
                let attempt = connect(config);
                attempts.push(async move { (name, source, fingerprint, attempt.await) });
            }
            Err(reason) => {
                failures.insert(server.name().to_owned(), reason.clone());
                statuses.insert(
                    server.name().to_owned(),
                    status(server, McpRuntimeServerState::Failed, Some(reason)),
                );
            }
        }
    }

    for (name, source, fingerprint, result) in futures_util::future::join_all(attempts).await {
        match result {
            Ok(client) => {
                clients.insert(name.clone(), Arc::new(client));
                statuses.insert(
                    name.clone(),
                    McpRuntimeServerStatus {
                        name,
                        source,
                        fingerprint,
                        state: McpRuntimeServerState::Connected,
                        failure: None,
                    },
                );
            }
            Err(error) => {
                let reason = error.to_string();
                failures.insert(name.clone(), reason.clone());
                statuses.insert(
                    name.clone(),
                    McpRuntimeServerStatus {
                        name,
                        source,
                        fingerprint,
                        state: McpRuntimeServerState::Failed,
                        failure: Some(reason),
                    },
                );
            }
        }
    }
    McpRuntimeCandidate {
        runtime: McpRuntime {
            clients,
            failures,
            statuses,
        },
    }
}

fn status(
    server: &EffectiveMcpServer,
    state: McpRuntimeServerState,
    failure: Option<String>,
) -> McpRuntimeServerStatus {
    McpRuntimeServerStatus {
        name: server.name().to_owned(),
        source: server.source(),
        fingerprint: server.fingerprint().clone(),
        state,
        failure,
    }
}

fn client_config(
    server: &EffectiveMcpServer,
    working_dir: &Path,
) -> Result<McpClientConfig, String> {
    let definition = server.definition();
    let transport = if let Some(command) = definition.command.as_ref() {
        McpTransport::Stdio {
            command: command.clone(),
            args: definition.args.clone().unwrap_or_default(),
        }
    } else if let Some(url) = definition.url.as_ref() {
        McpTransport::Http { url: url.clone() }
    } else {
        return Err(format!(
            "MCP server '{}' has no active transport",
            server.name()
        ));
    };
    Ok(McpClientConfig {
        name: server.name().to_owned(),
        transport,
        env: definition
            .env
            .as_ref()
            .map_or_else(HashMap::new, |values| values.clone().into_iter().collect()),
        headers: definition
            .headers
            .as_ref()
            .map_or_else(HashMap::new, |values| values.clone().into_iter().collect()),
        working_dir: Some(working_dir.to_path_buf()),
        max_inbound_message_bytes: definition
            .max_inbound_message_bytes
            .unwrap_or(DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES),
        request_timeout_ms: definition.request_timeout_ms,
    })
}
