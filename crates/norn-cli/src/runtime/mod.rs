//! Runtime assembly — resolving a CLI invocation onto the single
//! library-owned [`AgentBuilder`](norn::agent::AgentBuilder) assembler.

pub mod from_cli;
pub mod resolve;
pub mod wiring;

pub use from_cli::builder_from_cli;
pub use resolve::{ResolvedInvocation, resolve_invocation};
pub use wiring::{
    SlashStateInputs, build_slash_state_from_bundle, build_slash_state_with_schema,
    build_write_tool, cli_coordination_envelope, length_limit_from_profile,
    warn_unmatched_tool_flag_names,
};
