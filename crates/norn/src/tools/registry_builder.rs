//! Default tool-set assembly shared by the CLI runtime and the
//! [`AgentBuilder`](crate::agent::builder::AgentBuilder).
//!
//! [`register_standard_tools`] registers the full, curated Norn tool set into
//! a [`ToolRegistry`]. Every tool exists for a reason; the set is purposeful,
//! not a grab-bag. Callers that need a narrower set exclude specific tools
//! after registration via [`ToolRegistry::remove`] (the builder's
//! `.without_tools` path), and callers that need extra tools register them
//! with [`ToolRegistry::register`] (the builder's `.tool` path).
//!
//! This lives in the library (not `norn-cli`) so the builder — itself a
//! library type — can assemble the standard set without depending on the CLI
//! crate. `norn-cli` re-exports this function for its own runtime wiring.

use std::sync::Arc;

use crate::tool::registry::ToolRegistry;
use crate::tools::action_log::ActionLogTool;
use crate::tools::agent::{CloseAgentTool, ForkTool, SignalAgentTool, SpawnAgentTool};
use crate::tools::bash::BashTool;
use crate::tools::edit::EditTool;
use crate::tools::lsp::{LspBackend, LspTool};
use crate::tools::patch::ApplyPatchTool;
use crate::tools::read::ReadTool;
use crate::tools::search::SearchTool;
use crate::tools::task::TaskTool;
use crate::tools::tool_search::ToolSearchTool;
use crate::tools::web::{WebFetchTool, WebSearchTool};
use crate::tools::write::WriteTool;

/// Register the full standard Norn tool set into `registry`.
///
/// The set: `read`, `write`, `edit`, `bash`, `apply_patch`, `search`, `lsp`,
/// `task`, `tool_search`, `action_log`, `web_fetch`, `web_search`, and the
/// four agent-coordination tools (`spawn_agent`, `fork`, `signal_agent`,
/// `close_agent`).
///
/// `action_log` reads an [`ActionLog`](crate::session::action_log::ActionLog)
/// published as a [`ToolContext`](crate::tool::context::ToolContext)
/// extension by the agent builder; it errors at call time when that extension
/// is absent (e.g. when the registry is assembled outside the builder).
///
/// [`WriteTool`] is registered with its default length limits. Callers that
/// need configured limits (the CLI's profile/`-c`-derived limits) register
/// their own [`WriteTool`] afterwards; [`ToolRegistry::register`] keys on the
/// tool name, so the later registration replaces this default in place.
///
/// When `lsp_backend` is `Some`, the LSP tool is wired to a live backend;
/// otherwise it is registered with no backend (every call returns an error
/// directing the caller to configure one).
///
/// The web tools are fallible to construct (they validate environment
/// configuration at build time). A construction failure is logged and the
/// tool is skipped rather than aborting the whole registration — the agent
/// still gets every other tool. This is the one place a tool may be absent
/// from the "standard" set, and it is surfaced via a `tracing::warn!` line
/// rather than failing silently.
pub fn register_standard_tools(
    registry: &mut ToolRegistry,
    lsp_backend: Option<Arc<dyn LspBackend>>,
) {
    registry.register(Box::new(ReadTool::new()));
    registry.register(Box::new(WriteTool::new()));
    registry.register(Box::new(EditTool::new()));
    registry.register(Box::new(BashTool::new()));
    registry.register(Box::new(ApplyPatchTool::new()));
    registry.register(Box::new(SearchTool::new()));
    match lsp_backend {
        Some(backend) => registry.register(Box::new(LspTool::with_backend(backend))),
        None => registry.register(Box::new(LspTool::new())),
    }
    registry.register(Box::new(TaskTool::new()));
    registry.register(Box::new(ToolSearchTool::new()));
    registry.register(Box::new(ActionLogTool::new()));

    match WebFetchTool::new() {
        Ok(fetch) => registry.register(Box::new(fetch)),
        Err(err) => {
            tracing::warn!(error = %err, "web_fetch tool unavailable; skipping registration");
        }
    }
    match WebSearchTool::new() {
        Ok(search) => registry.register(Box::new(search)),
        Err(err) => {
            tracing::warn!(error = %err, "web_search tool unavailable; skipping registration");
        }
    }

    registry.register(Box::new(SpawnAgentTool::new()));
    registry.register(Box::new(ForkTool::new()));
    registry.register(Box::new(SignalAgentTool::new()));
    registry.register(Box::new(CloseAgentTool::new()));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_full_standard_set() {
        let mut registry = ToolRegistry::new();
        register_standard_tools(&mut registry, None);

        for name in [
            "read",
            "write",
            "edit",
            "bash",
            "apply_patch",
            "search",
            "lsp",
            "task",
            "tool_search",
            "action_log",
            "spawn_agent",
            "fork",
            "signal_agent",
            "close_agent",
        ] {
            assert!(
                registry.get(name).is_some(),
                "standard tool '{name}' must be registered",
            );
        }
    }

    /// Every provider requires a function tool's parameter schema to be an
    /// object schema at the root — `OpenAI` hard-rejects the whole request
    /// with HTTP 400 `invalid_function_parameters` otherwise (regression:
    /// the `task` tool's derived `oneOf` schema shipped without a root
    /// `type`). Guard the entire standard set, not just the tool that broke.
    #[test]
    fn every_standard_tool_schema_root_is_an_object() {
        let mut registry = ToolRegistry::new();
        register_standard_tools(&mut registry, None);

        let names: Vec<String> = registry.names().map(str::to_owned).collect();
        for name in names {
            let tool = registry
                .get(&name)
                .expect("name came from the registry's own iterator");
            let schema = tool.input_schema();
            assert_eq!(
                schema.get("type").and_then(serde_json::Value::as_str),
                Some("object"),
                "tool '{name}' parameter schema root must be type \"object\", got: {schema}",
            );
        }
    }

    #[test]
    fn write_tool_is_in_the_default_set() {
        let mut registry = ToolRegistry::new();
        register_standard_tools(&mut registry, None);
        assert!(
            registry.get("write").is_some(),
            "WriteTool must be in the default set with default config",
        );
    }

    #[test]
    fn later_write_registration_replaces_the_default_in_place() {
        let mut registry = ToolRegistry::new();
        register_standard_tools(&mut registry, None);
        let before = registry.len();
        // Registering another WriteTool keys on the same name and replaces
        // the default rather than adding a duplicate — this is what lets the
        // CLI overlay its configured WriteTool on top of the default.
        registry.register(Box::new(WriteTool::new()));
        assert_eq!(
            registry.len(),
            before,
            "re-registering write must not add a duplicate"
        );
    }
}
