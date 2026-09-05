//! MCP runtime attachment for [`AgentBuilder`](super::AgentBuilder).

use std::path::Path;
use std::sync::Arc;

use super::AgentBuilder;
use crate::config::{McpApprovalStore, McpConfigState};
use crate::error::{ConfigError, NornError};
use crate::integration::{
    McpCandidateBuilder, McpChannelHost, McpChannelSettings, McpControlHandle, McpRuntime,
    McpRuntimeCandidateBuilder, McpRuntimeStore,
};
use crate::r#loop::LoopContext;
use crate::tool::ToolGenerationStore;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;

#[derive(Default)]
pub(super) struct McpAttachment {
    runtime: Option<Arc<McpRuntime>>,
    servers: Option<Vec<String>>,
    state: Option<McpConfigState>,
    channels: Option<McpChannelSettings>,
}

pub(super) struct AgentToolRuntime {
    pub(super) tools: Arc<ToolGenerationStore>,
    pub(super) mcp_control: Option<McpControlHandle>,
}

impl McpAttachment {
    pub(super) fn assemble(
        &self,
        working_dir: &Path,
        registry: ToolRegistry,
        context: &ToolContext,
        channel_host: Option<McpChannelHost>,
    ) -> Result<(Arc<ToolRegistry>, AgentToolRuntime), NornError> {
        let registry = Arc::new(registry);
        let tools = Arc::new(ToolGenerationStore::from_registry(registry.as_ref()));
        let mcp_control = self.start_with_channels(working_dir, &tools, context, channel_host)?;
        Ok((registry, AgentToolRuntime { tools, mcp_control }))
    }

    pub(super) fn state(&self) -> Option<McpConfigState> {
        self.state.clone()
    }

    pub(super) fn runtime(&self) -> Arc<McpRuntime> {
        self.runtime
            .as_ref()
            .map_or_else(|| Arc::new(McpRuntime::empty()), Arc::clone)
    }

    pub(super) fn servers(&self) -> Option<Vec<String>> {
        self.servers.clone()
    }

    fn start_with_channels(
        &self,
        working_dir: &Path,
        generations: &Arc<ToolGenerationStore>,
        context: &ToolContext,
        channel_host: Option<McpChannelHost>,
    ) -> Result<Option<McpControlHandle>, NornError> {
        let Some(state) = self.state() else {
            return Ok(None);
        };
        let mut builder = McpRuntimeCandidateBuilder::new(working_dir.to_path_buf());
        if let Some(settings) = self.channels.as_ref() {
            let host = channel_host.ok_or_else(|| {
                NornError::Config(ConfigError::InvalidConfig {
                    reason: "MCP channel runtime has no session-owned inbox".to_owned(),
                })
            })?;
            builder = builder.with_channels(host, settings.clone());
        }
        if let Some(servers) = self.servers() {
            builder = builder.with_selected_servers(servers);
        }
        let approvals = match McpApprovalStore::open() {
            Ok(approvals) => Some(approvals),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "project MCP approval is unavailable; direct-scope live control remains active",
                );
                None
            }
        };
        let snapshot = state.snapshot()?;
        let runtime = Arc::new(self.runtime().with_config_snapshot(&snapshot));
        let runtimes = Arc::new(McpRuntimeStore::new(generations.snapshot(), runtime));
        context.insert_extension(Arc::clone(generations));
        context.insert_extension(Arc::clone(&runtimes));
        McpControlHandle::spawn(
            state,
            approvals,
            Arc::new(builder) as Arc<dyn McpCandidateBuilder>,
            Arc::clone(generations),
            runtimes,
        )
        .map(Some)
        .map_err(|error| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: format!("failed to start the live MCP control plane: {error}"),
            })
        })
    }

    pub(super) fn register_tools(&self, registry: &mut ToolRegistry) -> Result<(), NornError> {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.register_tools(registry)?;
        }
        Ok(())
    }

    pub(super) fn install_channels(
        &self,
        context: &mut LoopContext,
    ) -> Result<Option<McpChannelHost>, NornError> {
        let Some(settings) = self.channels.as_ref() else {
            return Ok(None);
        };
        if self
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.is_empty())
        {
            return Err(NornError::Config(ConfigError::InvalidConfig {
                reason: "MCP channels require root registration before connecting servers; remove the preconnected runtime".to_owned(),
            }));
        }
        let state = self.state.as_ref().ok_or_else(|| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: "MCP channels require retained MCP configuration".to_owned(),
            })
        })?;
        settings.validate_startup(&state.snapshot()?)?;
        context
            .install_mcp_channel_inbox(settings.limits())
            .map(Some)
            .map_err(NornError::Config)
    }

    pub(super) fn restrict_tools(&self, registry: &mut ToolRegistry) -> Result<(), NornError> {
        if let (Some(runtime), Some(servers)) = (self.runtime.as_ref(), self.servers.as_deref()) {
            runtime.restrict_registry_to_servers(registry, servers)?;
        }
        Ok(())
    }
}

impl AgentBuilder {
    /// Enable explicit channel sources after this agent's real identity is registered.
    ///
    /// Retained MCP configuration is required. The launcher must await the built
    /// handle's MCP control `initialize` operation before starting the first turn.
    #[must_use]
    pub fn mcp_channels(mut self, settings: McpChannelSettings) -> Self {
        self.mcp.channels = Some(settings);
        self
    }

    /// Retain a named MCP tool view while connections await root initialization.
    ///
    /// Names are validated against the candidate runtime before it is published;
    /// this selection does not enable channel input from those servers.
    #[must_use]
    pub fn mcp_selected_servers(mut self, servers: Vec<String>) -> Self {
        self.mcp.servers = Some(servers);
        self
    }

    /// Attach already-connected MCP clients to this agent and its inherited
    /// child tool surface. Connection remains an async launcher concern.
    #[must_use]
    pub fn mcp_runtime(mut self, runtime: Arc<McpRuntime>) -> Self {
        self.mcp.runtime = Some(Arc::clone(&runtime));
        self.mcp.servers = None;
        self.extension(runtime)
    }

    /// Attach the retained layered MCP state used by live runtime control.
    #[must_use]
    pub fn mcp_config_state(mut self, state: McpConfigState) -> Self {
        self.mcp.state = Some(state);
        self
    }

    /// Attach a selected MCP server view to this agent while retaining the
    /// shared connected runtime for independently configured children.
    pub fn mcp_runtime_for_servers(
        mut self,
        runtime: Arc<McpRuntime>,
        servers: &[String],
    ) -> Result<Self, NornError> {
        runtime.tool_names_for_servers(servers)?;
        self.mcp.runtime = Some(Arc::clone(&runtime));
        self.mcp.servers = Some(servers.to_vec());
        Ok(self.extension(runtime))
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_channel_builder_tests.rs"]
mod channel_tests;
