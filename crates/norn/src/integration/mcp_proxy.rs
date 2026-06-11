//! [`McpProxyTool`] — `Tool` implementation that forwards `execute` calls
//! to a remote MCP server via a shared [`McpClientInner`] handle.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};

use super::mcp_client::{McpClientInner, McpToolDef, ToolCallContent, ToolsCallResult};

/// `Tool` implementation that forwards `execute` calls to a remote MCP
/// server.
pub struct McpProxyTool {
    def: McpToolDef,
    client: Arc<McpClientInner>,
}

impl McpProxyTool {
    /// Construct a proxy tool from a definition and a shared client handle.
    #[must_use]
    pub fn new(def: McpToolDef, client: Arc<McpClientInner>) -> Self {
        Self { def, client }
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn input_schema(&self) -> Value {
        self.def.input_schema.clone()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Network
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let params = serde_json::json!({
            "name": self.def.name,
            "arguments": envelope.model_args,
        });
        let value = self.client.rpc("tools/call", params).await.map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("MCP call '{}' failed: {e}", self.def.name),
            }
        })?;
        let parsed: ToolsCallResult =
            serde_json::from_value(value.clone()).unwrap_or_else(|_| ToolsCallResult {
                content: vec![ToolCallContent::Text {
                    text: value.to_string(),
                }],
                is_error: false,
            });

        let mut text = String::new();
        for item in &parsed.content {
            if let ToolCallContent::Text { text: t } = item {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
        let content = serde_json::json!({
            "text": text,
            "is_error": parsed.is_error,
            "raw": value,
        });
        if parsed.is_error {
            return Ok(ToolOutput::failure_with_content(
                content,
                crate::tool::failure::ToolErrorPayload::new(
                    crate::tool::failure::ToolErrorKind::ExternalService,
                    format!("MCP tool '{}' reported an error", self.def.name),
                ),
            ));
        }
        Ok(ToolOutput::success(content))
    }
}
