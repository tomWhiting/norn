//! MCP runtime attachment for [`AgentBuilder`](super::AgentBuilder).

use std::sync::Arc;

use super::AgentBuilder;
use crate::error::NornError;
use crate::integration::McpRuntime;

impl AgentBuilder {
    /// Attach already-connected MCP clients to this agent and its inherited
    /// child tool surface. Connection remains an async launcher concern.
    #[must_use]
    pub fn mcp_runtime(mut self, runtime: Arc<McpRuntime>) -> Self {
        self.extra_tools.extend(runtime.proxy_tools());
        self.extension(runtime)
    }

    /// Attach a selected MCP server view to this agent while retaining the
    /// shared connected runtime for independently configured children.
    pub fn mcp_runtime_for_servers(
        mut self,
        runtime: Arc<McpRuntime>,
        servers: &[String],
    ) -> Result<Self, NornError> {
        self.extra_tools
            .extend(runtime.proxy_tools_for_servers(servers)?);
        Ok(self.extension(runtime))
    }
}
