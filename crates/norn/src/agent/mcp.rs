//! MCP runtime attachment for [`AgentBuilder`](super::AgentBuilder).

use std::sync::Arc;

use super::AgentBuilder;
use crate::error::NornError;
use crate::integration::McpRuntime;
use crate::tool::registry::ToolRegistry;

#[derive(Default)]
pub(super) struct McpAttachment {
    runtime: Option<Arc<McpRuntime>>,
    servers: Option<Vec<String>>,
}

impl McpAttachment {
    pub(super) fn register_tools(&self, registry: &mut ToolRegistry) -> Result<(), NornError> {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.register_tools(registry)?;
        }
        Ok(())
    }

    pub(super) fn restrict_tools(&self, registry: &mut ToolRegistry) -> Result<(), NornError> {
        if let (Some(runtime), Some(servers)) = (self.runtime.as_ref(), self.servers.as_deref()) {
            runtime.restrict_registry_to_servers(registry, servers)?;
        }
        Ok(())
    }
}

impl AgentBuilder {
    /// Attach already-connected MCP clients to this agent and its inherited
    /// child tool surface. Connection remains an async launcher concern.
    #[must_use]
    pub fn mcp_runtime(mut self, runtime: Arc<McpRuntime>) -> Self {
        self.mcp.runtime = Some(Arc::clone(&runtime));
        self.mcp.servers = None;
        self.extension(runtime)
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
