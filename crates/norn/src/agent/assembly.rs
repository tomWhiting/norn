//! Cohesive assembly phases for [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).

mod paths;
mod runtime;
mod tooling;

pub use paths::validate_workspace_root;
pub(crate) use paths::{resolve_base_profile, resolve_working_dir};
pub(crate) use runtime::{
    AgentConfigPresence, AgentInfraParts, OverlayOverrides, RuntimeOverlay,
    apply_base_to_loop_context, effective_agent_config, install_runtime_base_extensions,
    populate_loop_context, resolve_runtime_overlay, restore_session_state,
};
pub(crate) use tooling::{
    ExtensionInstaller, ToolContextParts, assemble_tool_context, collect_tool_definitions,
    install_agent_infra, install_tool_catalog,
};
