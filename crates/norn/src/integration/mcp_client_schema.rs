use std::collections::HashSet;

use super::McpToolDef;
use crate::error::IntegrationError;

pub(super) fn validate_discovered_tools(tools: &[McpToolDef]) -> Result<(), IntegrationError> {
    let mut names = HashSet::new();
    for tool in tools {
        if tool.name.trim().is_empty() {
            return Err(IntegrationError::McpError {
                reason: "MCP tools/list returned an empty tool name".to_owned(),
            });
        }
        if !names.insert(tool.name.as_str()) {
            return Err(IntegrationError::McpError {
                reason: format!("MCP tools/list returned duplicate tool '{}'", tool.name),
            });
        }
        if schema_uses_envelope_key(&tool.input_schema) {
            return Err(IntegrationError::McpError {
                reason: format!(
                    "MCP tool '{}' uses a Norn-reserved tool envelope property",
                    tool.name,
                ),
            });
        }
    }
    Ok(())
}

fn schema_uses_envelope_key(schema: &serde_json::Value) -> bool {
    let declares = |value: &serde_json::Value| {
        value
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key(crate::tool::envelope::ENVELOPE_DESCRIPTION_KEY)
                    || properties.contains_key(crate::tool::envelope::ENVELOPE_METADATA_KEY)
            })
    };
    declares(schema)
        || schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|variants| variants.iter().any(declares))
}
