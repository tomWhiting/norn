//! Shared runtime initialisation for CLI and in-process library consumers.
//!
//! This module contains the settings/context/profile pieces that must be
//! identical whether Norn is launched by `norn-cli` or embedded in another
//! process.

pub mod base;
pub mod extensions;
pub mod hooks;

pub use base::{
    LoadedRuntimeBase, ProviderSettingsResolved, agent_config_from_settings,
    apply_settings_reasoning_to_profile, load_merged_settings, load_runtime_base,
    provider_settings_from_settings,
};
pub use extensions::{
    install_agent_handles, install_context_search_paths, install_permission_policy,
    install_runtime_extensions, install_skill_infra, install_terminal_reclamation,
    install_tool_catalog, install_tool_output_budget, install_variant_catalog,
};
pub use hooks::assemble_hook_registry;
