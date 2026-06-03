//! Norn: headless agent runtime for Yggdrasil.

pub mod agent;
pub mod config;
pub mod context;
pub mod error;
pub mod integration;
pub mod internal;
pub mod r#loop;
pub mod profile;
pub mod provider;
pub mod rules;
pub mod runtime_init;
pub mod session;

pub mod skill;
pub mod system_prompt;
pub mod tool;
pub mod tools;
pub mod util;

#[cfg(test)]
mod tests;

pub use crate::profile::{Capability, Profile, PromptCommand, from_profile};
