//! Concrete bridge from live MCP configuration to immutable tool generations.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use super::mcp_channel_settings::McpChannelSelection;
use super::mcp_control::{
    McpActivationCandidate, McpActivationRequest, McpCandidateBuilder, McpCandidateError,
};
use super::{McpChannelHost, McpChannelSettings};
use crate::config::McpConfigSnapshot;
use crate::tool::ToolGeneration;

/// Builds an MCP runtime and its matching immutable tool generation.
pub struct McpRuntimeCandidateBuilder {
    working_dir: PathBuf,
    selected_servers: Option<Arc<[String]>>,
    channels: Option<(McpChannelHost, McpChannelSettings)>,
}

impl McpRuntimeCandidateBuilder {
    /// Build candidates relative to `working_dir` with every active server.
    #[must_use]
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            selected_servers: None,
            channels: None,
        }
    }

    /// Restrict the generation's MCP tools to a named server view.
    ///
    /// The runtime still connects and reports status for the complete active
    /// server set so independently selected child views can share it.
    #[must_use]
    pub fn with_selected_servers(mut self, servers: Vec<String>) -> Self {
        self.selected_servers = Some(Arc::from(servers));
        self
    }

    /// Bind newly connected channel sources to the existing root-owned inbox.
    #[must_use]
    pub fn with_channels(mut self, host: McpChannelHost, settings: McpChannelSettings) -> Self {
        self.channels = Some((host, settings));
        self
    }
}

#[async_trait]
impl McpCandidateBuilder for McpRuntimeCandidateBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        let mut servers = BTreeMap::new();
        for server in request.active_servers().iter() {
            if servers
                .insert(server.name().to_owned(), server.clone())
                .is_some()
            {
                return Err(McpCandidateError::DuplicateServer {
                    name: server.name().to_owned(),
                });
            }
        }
        let snapshot = McpConfigSnapshot::new(servers);
        let previous_runtime = request.previous_runtime();
        let runtime_candidate = match self.channels.as_ref() {
            Some((host, settings)) => {
                previous_runtime
                    .build_channel_candidate(&snapshot, &self.working_dir, host, settings)
                    .await
            }
            None => {
                previous_runtime
                    .build_candidate(&snapshot, &self.working_dir)
                    .await
            }
        };
        if let Some((_, settings)) = self.channels.as_ref() {
            for (name, reason) in runtime_candidate.failures() {
                let required = snapshot.get(name).is_some_and(|server| {
                    matches!(
                        settings.selection(name, server.definition().command.is_some()),
                        McpChannelSelection::Required(_)
                    )
                });
                if required {
                    return Err(crate::error::IntegrationError::McpError {
                        reason: format!("MCP channel source '{name}' failed to connect: {reason}"),
                    }
                    .into());
                }
            }
        }
        let complete_generation = ToolGeneration::replacing_dynamic_tools(
            request.previous().as_ref(),
            runtime_candidate.proxy_tools(),
            request.revision(),
        )
        .map_err(McpCandidateError::from)?;
        let generation = if let Some(selected) = self.selected_servers.as_ref() {
            ToolGeneration::replacing_dynamic_tools(
                request.previous().as_ref(),
                runtime_candidate
                    .proxy_tools_for_servers(selected)
                    .map_err(McpCandidateError::from)?,
                request.revision(),
            )
            .map_err(McpCandidateError::from)?
        } else {
            complete_generation
        };
        let runtime = Arc::new(runtime_candidate.into_runtime());
        let candidate = McpActivationCandidate::new(Arc::new(generation), runtime);
        Ok(if self.channels.is_some() {
            candidate.with_channel_lifecycle()
        } else {
            candidate
        })
    }
}

#[cfg(test)]
#[path = "mcp_candidate_builder_tests.rs"]
mod tests;
