//! CLI flag overrides applied to a loaded [`norn::profile::Profile`] and the agent
//! configuration triple (NC-004 R2 / R8).
//!
//! Order of application matters: profile loaders (R1) construct the base
//! [`norn::profile::Profile`]; this module mutates non-prompt profile fields and retains
//! prompt flags as operator-owned side channels; then
//! [`apply_loop_config_overrides`] folds CLI-derived
//! values onto an [`norn::agent_loop::config::AgentLoopConfig`]. The orchestrator
//! (R8) then layers the `-c key=value` [`crate::config::ConfigOverrides`] on top.
//!
//! The disallowed-tools list lives on the [`AppliedOverrides`] return
//! type rather than the profile because libnorn has no top-level
//! `disallowed_tools` field on [`norn::profile::Profile`]; the list is carried
//! separately through to `builder_from_cli`
//! ([`AgentBuilder::disallowed_tools`](norn::agent::AgentBuilder::disallowed_tools)).

mod loop_config;
mod process;
mod profile;
mod provider;

pub use loop_config::{
    DEFAULT_INDEX_LOCK_DEADLINE_MS, apply_config_overrides_to_loop, apply_loop_config_overrides,
    apply_settings_to_agent_config, default_agent_loop_config, effective_step_timeout,
    resolve_index_lock_deadline, retry_policy_from_settings_and_overrides,
};
pub use process::apply_working_dir;
pub use profile::{apply_cli_profile_overrides, apply_settings_reasoning_to_profile};
pub use provider::{
    overlay_cli_provider_overrides, overlay_provider_profile_overrides,
    provider_overrides_from_settings,
};

/// Side-channel outputs produced when applying CLI overrides that do not
/// fit on the [`norn::profile::Profile`] type itself.
#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    /// Tool names added by `--disallowed-tools` (exact names, matching
    /// the `--allowed-tools` semantics). `builder_from_cli` passes them to
    /// [`AgentBuilder::disallowed_tools`](norn::agent::AgentBuilder::disallowed_tools),
    /// which applies them via
    /// [`ToolRegistry::set_disallowed`](norn::tool::registry::ToolRegistry::set_disallowed)
    /// — deny wins over the allow-list. Also fed to
    /// [`warn_unmatched_tool_flag_names`](crate::runtime::warn_unmatched_tool_flag_names)
    /// after `build()` to flag a bogus name that matches no real tool.
    pub disallowed_tools: Vec<String>,
    /// Tool names supplied via the `--allowed-tools` flag specifically
    /// (empty when the flag is absent). The allow-list itself rides on
    /// [`Profile::tools`](norn::profile::Profile::tools) (also populatable by the
    /// profile file); this flag-only copy is kept separately so
    /// [`warn_unmatched_tool_flag_names`](crate::runtime::warn_unmatched_tool_flag_names)
    /// can warn about a flag-supplied name that matches no registered tool
    /// (a partial typo like `--allowed-tools read,serch`) without
    /// mis-flagging profile-declared lists.
    pub allowed_tools: Vec<String>,
    /// Explicit operator replacement for profile instructions.
    pub system_prompt: Option<String>,
    /// Explicit operator fragment appended after resolved profile instructions.
    pub append_system_prompt: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
