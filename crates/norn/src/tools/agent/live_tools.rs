//! Immutable child tool views over the committed live MCP pool.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::infra::SubAgentExecutor;
use crate::error::ToolError;
use crate::integration::{McpRuntimeSnapshot, McpRuntimeStore};
use crate::r#loop::config::{DispatchOutcome, ToolExecutionSnapshot, ToolExecutor};
use crate::provider::request::ToolDefinition;
use crate::tool::{ToolContext, ToolGeneration, ToolRegistry};

/// MCP server selection inherited by descendants of a selected child.
pub(crate) struct McpServerView(pub(crate) Arc<[String]>);

pub(super) struct ChildToolSnapshot {
    pub(super) executor: SubAgentExecutor,
    pub(super) definitions: Vec<ToolDefinition>,
}

pub(super) fn child_tool_snapshot(
    parent_context: &ToolContext,
    parent_registry: &Arc<ToolRegistry>,
    allow_list: Option<Vec<String>>,
    requested_servers: Option<Vec<String>>,
    child_context: Arc<ToolContext>,
) -> Result<ChildToolSnapshot, ToolError> {
    let Some(runtimes) = parent_context.get_extension::<McpRuntimeStore>() else {
        return static_snapshot(
            parent_context,
            parent_registry,
            allow_list,
            requested_servers.as_deref(),
            child_context,
        );
    };
    let inherited = parent_context
        .get_extension::<McpServerView>()
        .map(|view| view.0.to_vec());
    let servers = requested_servers.or(inherited);
    let live = Arc::new(LiveChildTools::new(
        runtimes,
        allow_list,
        servers.clone(),
        Arc::clone(&child_context),
    )?);
    if let Some(servers) = servers {
        child_context.insert_extension(Arc::new(McpServerView(Arc::from(servers))));
    }
    let definitions = live.current.read().definitions().to_vec();
    Ok(ChildToolSnapshot {
        executor: SubAgentExecutor::from_live(live as Arc<dyn ToolExecutor>, child_context),
        definitions,
    })
}

struct LiveChildTools {
    runtimes: Arc<McpRuntimeStore>,
    available: Option<Arc<BTreeSet<String>>>,
    servers: Option<Arc<[String]>>,
    context: Arc<ToolContext>,
    current: RwLock<Arc<ToolGeneration>>,
}

impl LiveChildTools {
    fn new(
        runtimes: Arc<McpRuntimeStore>,
        available: Option<Vec<String>>,
        servers: Option<Vec<String>>,
        context: Arc<ToolContext>,
    ) -> Result<Self, ToolError> {
        let available = available.map(|names| Arc::new(names.into_iter().collect()));
        let servers = servers.map(Arc::from);
        let initial = build_generation(
            &runtimes.snapshot(),
            available.as_deref(),
            servers.as_deref(),
            Arc::clone(&context),
            true,
        )?;
        Ok(Self {
            runtimes,
            available,
            servers,
            context,
            current: RwLock::new(initial),
        })
    }

    fn latest(&self) -> Result<Arc<ToolGeneration>, ToolError> {
        let live = self.runtimes.snapshot();
        let current = self.current.read().clone();
        if current.revision() == live.revision() {
            return Ok(current);
        }
        let next = build_generation(
            &live,
            self.available.as_deref(),
            self.servers.as_deref(),
            Arc::clone(&self.context),
            false,
        )?;
        *self.current.write() = Arc::clone(&next);
        Ok(next)
    }
}

#[async_trait]
impl ToolExecutor for LiveChildTools {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.latest()?.execute(name, call_id, arguments).await
    }

    async fn execute_with_outcome(
        &self,
        name: &str,
        call_id: &str,
        arguments: serde_json::Value,
    ) -> Result<DispatchOutcome, ToolError> {
        self.latest()?
            .execute_with_outcome(name, call_id, arguments)
            .await
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.context))
    }

    fn execution_snapshot(&self) -> Option<ToolExecutionSnapshot> {
        match self.latest() {
            Ok(generation) => Some(ToolGeneration::execution_lease(&generation)),
            Err(error) => {
                tracing::error!(%error, "retaining the last valid child tool generation");
                Some(ToolGeneration::execution_lease(&self.current.read()))
            }
        }
    }
}

fn build_generation(
    live: &McpRuntimeSnapshot,
    base_available: Option<&BTreeSet<String>>,
    servers: Option<&[String]>,
    context: Arc<ToolContext>,
    strict_selection: bool,
) -> Result<Arc<ToolGeneration>, ToolError> {
    let source = live.generation();
    let runtime = live.runtime();
    let mut available = base_available.cloned();
    let dynamic = if let Some(servers) = servers {
        let mut dynamic = Vec::new();
        for server in servers {
            match runtime.proxy_tools_for_servers(std::slice::from_ref(server)) {
                Ok(tools) => dynamic.extend(tools),
                Err(error) if strict_selection => {
                    return Err(ToolError::ExecutionFailed {
                        reason: format!("child MCP server selection failed: {error}"),
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        server,
                        %error,
                        "removing a no-longer-configured MCP server from the child tool view",
                    );
                }
            }
        }
        let available =
            available.get_or_insert_with(|| source.names().map(str::to_owned).collect());
        let all: BTreeSet<_> = runtime.tool_names().into_iter().collect();
        let source_dynamic: BTreeSet<_> = source
            .dynamic_prompt_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        available.retain(|name| !all.contains(name) && !source_dynamic.contains(name));
        available.extend(dynamic.iter().map(|tool| tool.name().to_owned()));
        Some(dynamic)
    } else {
        None
    };
    crate::tool::ToolGeneration::child_view(source.as_ref(), dynamic, available.as_ref(), context)
        .map(Arc::new)
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!("child tool generation could not be assembled: {error}"),
        })
}

fn static_snapshot(
    parent_context: &ToolContext,
    parent_registry: &Arc<ToolRegistry>,
    allow_list: Option<Vec<String>>,
    servers: Option<&[String]>,
    child_context: Arc<ToolContext>,
) -> Result<ChildToolSnapshot, ToolError> {
    let allow_list = super::mcp_selection::apply_mcp_server_selection(
        parent_context,
        parent_registry,
        allow_list,
        servers,
    )?;
    let definitions = match allow_list.as_deref() {
        Some(names) => crate::provider::surface::collect_registered_function_definitions(
            parent_registry,
            names,
        ),
        None => crate::provider::surface::collect_function_definitions(parent_registry, None),
    };
    Ok(ChildToolSnapshot {
        executor: SubAgentExecutor::new(Arc::clone(parent_registry), allow_list, child_context),
        definitions,
    })
}

#[cfg(test)]
#[path = "live_tools_tests.rs"]
mod tests;
