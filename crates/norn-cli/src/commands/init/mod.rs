//! `norn init …` subcommand dispatcher (NTC-004).
//!
//! Thin dispatcher: [`dispatch`] matches the parsed [`InitCmd`](crate::cli::InitCmd)
//! and delegates to the per-target handler. Generation logic lives in the
//! sibling modules (e.g. [`conventions`]) so this stays a routing seam.

pub mod conventions;
mod dispatch;
pub mod upgrade;

pub use dispatch::run_init;
