//! Subcommand handlers for the `norn` binary (NC-008).
//!
//! Each subcommand group lives in its own file and exposes a single
//! `run_*` dispatcher consumed by `main.rs`. This module file is a thin
//! index — only `pub mod` declarations and `pub use` re-exports per the
//! `mod.rs`-discipline rule in `CLAUDE.md`.

pub mod auth;
pub mod completion;
pub mod doctor;
pub mod init;
pub mod mcp;
mod mcp_config;
pub mod session;
pub mod session_export;
mod session_legacy;
mod session_output;
pub mod slash;

pub use auth::run_auth;
pub use completion::run_completion;
pub use doctor::run_doctor;
pub use init::run_init;
pub use mcp::run_mcp;
pub use session::run_session;
pub use slash::{DispatchOutcome, SlashState, build_slash_registry, dispatch_input};
