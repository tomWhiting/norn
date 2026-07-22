//! Runtime and configuration phases for agent assembly.
//!
//! The build phases are split by responsibility so runtime-base resolution,
//! loop configuration, session restoration, and child infrastructure remain
//! independently reviewable without changing the facade consumed by
//! [`AgentBuilder`](crate::agent::builder::AgentBuilder).

mod base;
mod config;
mod infra;
mod session;

pub(crate) use base::{
    OverlayOverrides, RuntimeOverlay, apply_base_to_loop_context, install_runtime_base_extensions,
    resolve_runtime_overlay,
};
pub(crate) use config::{AgentConfigPresence, effective_agent_config, populate_loop_context};
pub(crate) use infra::AgentInfraParts;
pub(crate) use session::restore_session_state;

#[cfg(test)]
mod tests;
