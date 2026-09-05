//! Runtime assembly — resolving a CLI invocation onto the single
//! library-owned [`AgentBuilder`](norn::agent::AgentBuilder) assembler.

pub mod channel_config;
pub mod channel_startup;
pub mod from_cli;
mod mcp;
pub mod resolve;
pub mod wiring;

pub use channel_config::{resolve_channel_config, validate_channel_mode};
pub use channel_startup::{
    initialize_cli_channels, prepare_cli_mcp, warn_unmatched_runtime_tool_flag_names,
};
pub use from_cli::builder_from_cli;
pub use mcp::{McpStartup, connect_mcp_runtime};
pub use resolve::{ResolvedInvocation, resolve_invocation};
pub use wiring::{
    DEFAULT_DELEGATION_DEPTH, SlashStateInputs, build_slash_state_from_bundle,
    build_slash_state_with_schema, build_write_tool, cli_coordination_envelope,
    length_limit_from_profile, warn_unmatched_tool_flag_names,
};
