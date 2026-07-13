//! MCP runtime attachment for [`AgentBuilder`](super::AgentBuilder).

use std::sync::Arc;

use super::AgentBuilder;
use crate::integration::McpRuntime;

impl AgentBuilder {
    /// Attach already-connected MCP clients to this agent and its inherited
    /// child tool surface. Connection remains an async launcher concern.
    #[must_use]
    pub fn mcp_runtime(mut self, runtime: Arc<McpRuntime>) -> Self {
        self.mcp_runtime = Some(runtime);
        self
    }

    /// Select the MCP server view for this root agent. The connected runtime
    /// remains shared; this only narrows the agent-facing tool catalogue.
    #[must_use]
    pub fn mcp_servers(mut self, servers: Vec<String>) -> Self {
        self.mcp_server_selection = Some(servers);
        self
    }
}
