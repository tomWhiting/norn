//! Defer channel connections until root registration, then report committed startup state.

use std::path::Path;
use std::sync::Arc;

use norn::agent::AgentParts;
use norn::config::{McpApprovalState, ResolvedMcpServers};
use norn::error::{IntegrationError, NornError};
use norn::integration::{McpChannelPolicy, McpChannelSettings, McpRuntimeStore};

use crate::config::AppliedOverrides;

use super::{McpStartup, connect_mcp_runtime, warn_unmatched_tool_flag_names};

/// Include the published executable tool view in exact-name startup diagnostics.
pub fn warn_unmatched_runtime_tool_flag_names(parts: &AgentParts, applied: &AppliedOverrides) {
    let generation = parts.tool_runtime.snapshot();
    let names: std::collections::BTreeSet<_> = generation.names().collect();
    // Configured sources and tools outside the selected executable view do not
    // count as matches. The original registry still covers its physical tools.
    let unmatched = AppliedOverrides {
        allowed_tools: applied
            .allowed_tools
            .iter()
            .filter(|name| !names.contains(name.as_str()))
            .cloned()
            .collect(),
        disallowed_tools: applied
            .disallowed_tools
            .iter()
            .filter(|name| !names.contains(name.as_str()))
            .cloned()
            .collect(),
        ..AppliedOverrides::default()
    };
    warn_unmatched_tool_flag_names(&parts.registry, &unmatched);
}

/// Ordinary MCP startup connects immediately; channels wait for the registered root.
pub async fn prepare_cli_mcp(
    project_root: &Path,
    servers: &ResolvedMcpServers,
    channels: Option<&McpChannelSettings>,
) -> Result<McpStartup, NornError> {
    if channels.is_none() {
        return connect_mcp_runtime(project_root, servers).await;
    }
    Ok(McpStartup {
        runtime: None,
        pending_project_servers: Vec::new(),
        project_approval_error: None,
        failed_servers: Vec::new(),
    })
}

/// Publish the initial channel-aware runtime before the driver starts any turn.
pub async fn initialize_cli_channels(
    parts: &mut AgentParts,
    channels: Option<&McpChannelSettings>,
) -> Result<(), NornError> {
    let Some(settings) = channels else {
        return Ok(());
    };
    let session = parts
        .loop_context
        .mcp_channel_session
        .as_ref()
        .ok_or_else(|| {
            startup_error(&format!(
                "agent {} has no installed channel inbox",
                parts.id
            ))
        })?;
    if session.recipient_id() != parts.id {
        return Err(startup_error(&format!(
            "agent {} has channel inbox for {}",
            parts.id,
            session.recipient_id()
        )));
    }
    let host = session.host();
    let control = parts
        .mcp_control
        .as_ref()
        .ok_or_else(|| startup_error(&format!("agent {} has no MCP startup control", parts.id)))?;
    control
        .initialize()
        .await
        .map_err(|error| startup_error(&error.to_string()))?;
    let statuses = control
        .list()
        .await
        .map_err(|error| startup_error(&error.to_string()))?;
    let runtimes = parts
        .tool_runtime
        .snapshot()
        .context()
        .get_extension::<McpRuntimeStore>()
        .ok_or_else(|| {
            startup_error(&format!(
                "agent {} has no committed MCP runtime store",
                parts.id
            ))
        })?;
    let committed = runtimes.snapshot();
    let generation = committed.generation();
    parts.tool_defs = generation.definitions().to_vec();
    Arc::make_mut(&mut parts.info).tool_names = generation.names().map(str::to_owned).collect();
    let runtime = committed.runtime();
    for status in &statuses {
        if status.approval == McpApprovalState::Pending {
            eprintln!(
                "norn: MCP server '{}' is waiting for shared-project approval; from {} run `norn mcp approve {}`",
                status.name,
                parts.info.working_dir.display(),
                status.name,
            );
        }
    }
    for (name, reason) in runtime.failures() {
        eprintln!("norn: MCP server '{name}' is unavailable; continuing without it: {reason}");
    }
    for (name, policy) in settings.sources() {
        let policy = match policy {
            McpChannelPolicy::Hold => "hold",
            McpChannelPolicy::NextTurn => "next-turn",
            McpChannelPolicy::Wake => "wake",
        };
        let status = statuses
            .iter()
            .find(|status| status.name == *name)
            .ok_or_else(|| {
                startup_error(&format!(
                    "configured channel source '{name}' has no startup status"
                ))
            })?;
        eprintln!(
            "norn: channel '{name}': policy={policy}, recipient={}, approval={:?}, connection={:?}",
            parts.id, status.approval, status.runtime_state,
        );
    }
    let status = host.status();
    eprintln!(
        "norn: channel inbox: max_retained_messages={}, max_retained_bytes={}, overflow=reject-new, retained_messages={}, retained_bytes={}, rejected={}",
        status.limits.max_retained_messages(),
        status.limits.max_retained_bytes(),
        status.retained_messages,
        status.retained_bytes,
        status.rejected,
    );
    if let Some(rejection) = status.last_rejection {
        eprintln!(
            "norn: channel '{}' generation {} refused input: {}",
            rejection.source, rejection.generation, rejection.reason,
        );
    }
    Ok(())
}

fn startup_error(reason: &str) -> NornError {
    IntegrationError::McpError {
        reason: format!("channel startup: {reason}"),
    }
    .into()
}
