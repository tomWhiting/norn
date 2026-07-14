//! [`McpProxyTool`] — `Tool` implementation that forwards `execute` calls
//! to a remote MCP server via a shared [`McpClientInner`] handle.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};

use super::mcp_client::{McpClientInner, McpToolDef, ToolCallContent, ToolsCallResult};

/// `Tool` implementation that forwards `execute` calls to a remote MCP
/// server.
pub struct McpProxyTool {
    qualified_name: String,
    def: McpToolDef,
    client: Arc<McpClientInner>,
}

impl McpProxyTool {
    /// Construct a proxy tool from a definition and a shared client handle.
    #[must_use]
    pub fn new(server_name: &str, def: McpToolDef, client: Arc<McpClientInner>) -> Self {
        let qualified_name = qualified_tool_name(server_name, &def.name);
        Self {
            qualified_name,
            def,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.qualified_name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn input_schema(&self) -> Value {
        self.def.input_schema.clone()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Unknown
    }

    fn runtime_dynamic(&self) -> bool {
        true
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
            serde_json::from_value(value.clone()).map_err(|error| ToolError::ExecutionFailed {
                reason: format!(
                    "MCP tool '{}' returned an invalid result: {error}",
                    self.def.name
                ),
            })?;

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

pub(crate) fn qualified_tool_name(server_name: &str, tool_name: &str) -> String {
    let server = provider_name_segment(server_name, 12);
    let tool = provider_name_segment(tool_name, 20);
    let mut digest = Sha256::new();
    digest.update(server_name.as_bytes());
    digest.update([0]);
    digest.update(tool_name.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    format!("mcp_{server}_{tool}_{}", &digest[..24])
}

fn provider_name_segment(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
#[path = "mcp_proxy_tests.rs"]
mod tests;
