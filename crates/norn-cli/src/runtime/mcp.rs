//! Approval-aware MCP startup shared by print, driven, and TUI paths.

use std::path::Path;
use std::sync::Arc;

use norn::config::{McpApprovalState, McpApprovalStore, McpConfigSource, ResolvedMcpServers};
use norn::error::NornError;
use norn::integration::McpRuntime;

/// Connected runtime plus shared-project definitions awaiting approval.
pub struct McpStartup {
    /// Connected direct or approved server runtime.
    pub runtime: Option<Arc<McpRuntime>>,
    /// Pending shared-project server names, in deterministic order.
    pub pending_project_servers: Vec<String>,
    /// Approval-ledger failure that left project servers pending while direct
    /// operator scopes continued to connect.
    pub project_approval_error: Option<String>,
    /// Servers that failed independently while healthy peers remained active.
    pub failed_servers: Vec<(String, String)>,
}

/// Resolve approval before performing any process spawn or network request,
/// then connect every active server sequentially.
pub async fn connect_mcp_runtime(
    project_root: &Path,
    servers: &ResolvedMcpServers,
) -> Result<McpStartup, NornError> {
    let has_project_servers = servers
        .iter()
        .any(|server| server.source() == McpConfigSource::Project && server.enabled());
    let (mut approvals, mut project_approval_error) = if has_project_servers {
        match McpApprovalStore::open() {
            Ok(store) => (Some(store), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let mut configs = Vec::new();
    let mut pending_project_servers = Vec::new();
    for server in servers.iter().filter(|server| server.enabled()) {
        let state = if server.source() == McpConfigSource::Project {
            match approvals
                .as_ref()
                .map(|store| store.state(project_root, server))
            {
                Some(Ok(state)) => state,
                Some(Err(error)) => {
                    project_approval_error = Some(error.to_string());
                    approvals = None;
                    McpApprovalState::Pending
                }
                None => McpApprovalState::Pending,
            }
        } else {
            McpApprovalState::NotRequired
        };
        if state == McpApprovalState::Pending {
            pending_project_servers.push(server.name().to_owned());
            continue;
        }
        if let Some(config) = server.client_config(project_root)? {
            configs.push(config);
        }
    }
    let runtime = if configs.is_empty() {
        None
    } else {
        Some(Arc::new(McpRuntime::connect(configs).await))
    };
    let failed_servers = runtime
        .as_ref()
        .into_iter()
        .flat_map(|runtime| runtime.failures())
        .map(|(name, reason)| (name.to_owned(), reason.to_owned()))
        .collect();
    Ok(McpStartup {
        runtime,
        pending_project_servers,
        project_approval_error,
        failed_servers,
    })
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
