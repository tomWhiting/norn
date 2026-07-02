//! Runtime assembly — building the [`RuntimeBundle`] from CLI flags and profiles.

pub mod builder;
pub mod bundle;
pub mod from_cli;
pub mod wiring;

pub use builder::{apply_system_prompt, build_runtime};
pub use bundle::{RuntimeBundle, RuntimeInputs};
pub use from_cli::builder_from_cli;
// `register_standard_tools` moved to the `norn` library
// (`norn::tools::registry_builder`) so the `AgentBuilder` can assemble the
// standard tool set without depending on this crate. Re-exported here so the
// CLI's existing `crate::runtime::register_standard_tools` call sites keep
// resolving.
pub use norn::tools::register_standard_tools;
pub use wiring::{
    build_diagnostic_collector, build_skill_catalog, build_skill_search_paths,
    build_slash_state_from_bundle, build_slash_state_with_schema, build_write_tool,
    cli_coordination_envelope, install_action_log, install_agent_tool_infra,
    install_child_result_sender, install_headless_reclamation,
    install_pending_agent_messages_for_loop, install_shared_agent_event_channel,
    iteration_monitor_from_profile, length_limit_from_profile,
};
