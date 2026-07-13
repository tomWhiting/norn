//! Per-child MCP server view selection.

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;

pub(super) fn apply_mcp_server_selection(
    ctx: &ToolContext,
    parent_registry: &ToolRegistry,
    base_allow_list: Option<Vec<String>>,
    selection: Option<&[String]>,
) -> Result<Option<Vec<String>>, ToolError> {
    let Some(selection) = selection else {
        return Ok(base_allow_list);
    };
    let runtime = ctx
        .get_extension::<crate::integration::McpRuntime>()
        .ok_or_else(|| ToolError::ExecutionFailed {
            reason: "spawn_agent: mcp_servers was supplied but no MCP runtime is connected"
                .to_owned(),
        })?;
    let all: std::collections::HashSet<_> = runtime.tool_names().into_iter().collect();
    let selected =
        runtime
            .tool_names_for_servers(selection)
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!("spawn_agent: {error}"),
            })?;
    let mut available =
        base_allow_list.unwrap_or_else(|| parent_registry.names().map(str::to_owned).collect());
    available.retain(|name| !all.contains(name));
    for name in selected {
        if parent_registry.get(&name).is_some() && !available.contains(&name) {
            available.push(name);
        }
    }
    Ok(Some(available))
}
