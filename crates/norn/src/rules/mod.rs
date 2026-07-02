//! Rules engine: contextual guidance based on path globs and command matches.

pub use crate::error::RulesError;

pub mod engine;
pub mod lifecycle;
pub mod parser;
pub mod triggers;
pub mod types;
