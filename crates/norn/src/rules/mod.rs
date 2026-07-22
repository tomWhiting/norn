//! Rules engine: contextual guidance based on path globs and command matches.

pub use crate::error::RulesError;

pub mod engine;
pub mod lifecycle;
pub mod parser;
pub(crate) mod projection;
pub mod source;
pub mod triggers;
pub mod types;

#[cfg(test)]
mod authority_tests;
