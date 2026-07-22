//! Configuration pipeline — profile loading, overrides, variables, schemas,
//! extensions, rules, and path resolution.

pub mod assembly;
pub mod event_schemas;
pub mod extensions;
pub mod model_aliases;
pub mod overrides;
pub mod paths;
pub mod profile_loader;
mod provider_auth;
pub mod provider_selection;
pub mod rules;

pub use assembly::{ConfigOverrides, ProviderConfigOverrides, parse_duration, parse_kv};
pub use event_schemas::{merge_event_schemas, parse_inline_or_file};
pub use extensions::{collect_extension_servers, collect_extension_uris};
pub use model_aliases::{ResolvedModelSelection, resolve_model_alias, resolve_model_selection};
pub use overrides::{
    AppliedOverrides, DEFAULT_INDEX_LOCK_DEADLINE_MS, apply_cli_profile_overrides,
    apply_config_overrides_to_loop, apply_loop_config_overrides,
    apply_settings_reasoning_to_profile, apply_settings_to_agent_config, apply_working_dir,
    default_agent_loop_config, effective_step_timeout, overlay_cli_provider_overrides,
    overlay_provider_profile_overrides, provider_overrides_from_settings,
    resolve_index_lock_deadline, retry_policy_from_settings_and_overrides,
};
pub use paths::session_data_dir;
pub use profile_loader::{CliProfileSource, resolve_profile_with_origin};
pub(crate) use provider_auth::{ResolvedProviderAuth, resolve_provider_auth};
pub use provider_selection::{ProviderSelection, resolve_provider_selection};
pub use rules::load_rule_engine;
