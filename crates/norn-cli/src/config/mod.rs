//! Configuration pipeline — profile loading, overrides, variables, schemas,
//! extensions, rules, and path resolution.

pub mod assembly;
pub mod event_schemas;
pub mod extensions;
pub mod overrides;
pub mod paths;
pub mod profile_loader;
pub mod rules;
pub mod variables;

pub use assembly::{ConfigOverrides, ProviderConfigOverrides, parse_duration, parse_kv};
pub use event_schemas::{merge_event_schemas, parse_inline_or_file};
pub use extensions::collect_extension_uris;
pub use overrides::{
    AppliedOverrides, apply_cli_profile_overrides, apply_config_overrides_to_loop,
    apply_loop_config_overrides, apply_settings_reasoning_to_profile,
    apply_settings_to_agent_config, apply_working_dir, default_agent_loop_config,
    effective_step_timeout, overlay_cli_provider_overrides, provider_overrides_from_settings,
    retry_policy_from_settings_and_overrides,
};
pub use paths::session_data_dir;
pub use profile_loader::resolve_profile;
pub use rules::load_rule_engine;
pub use variables::build_variable_store;
