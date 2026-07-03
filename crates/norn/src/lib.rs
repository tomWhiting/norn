//! Norn: headless agent runtime for Yggdrasil.

pub mod agent;
pub mod config;
pub mod context;
pub mod error;
pub mod integration;
pub mod internal;
pub(crate) mod r#loop;
pub mod model_catalog;
pub mod process;
pub mod profile;
pub mod provider;
pub mod rules;
pub mod runtime_init;
pub mod schedule;
pub mod session;

pub mod skill;
pub mod system_prompt;
pub mod tool;
pub mod tools;
pub mod util;

#[cfg(test)]
mod tests;

/// The agent loop: runner, configuration, retry policy, conversation
/// state, schema enforcement, compaction, and the inbound steering
/// channel.
///
/// This is the public face of the internal `loop` module — exposed as
/// `agent_loop` so embedders never have to escape the `loop` keyword
/// with a raw identifier (`r#loop`).
pub mod agent_loop {
    pub use crate::r#loop::*;
}

pub use crate::profile::{Capability, Profile, PromptCommand, from_profile};
