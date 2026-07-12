//! Library surface of the `norn` CLI binary.
//!
//! Organised into seven top-level modules:
//!
//! - [`cli`] — argument parsing, mode detection, exit codes, error types
//! - [`config`] — profile loading, overrides, variables, schemas, paths
//! - [`runtime`] — CLI resolution (`resolve_invocation`, `builder_from_cli`)
//!   onto the library `AgentBuilder`, and CLI-side wiring helpers
//! - [`print`] — print-mode orchestration, output formatting, provider construction
//! - [`commands`] — subcommand handlers (auth, doctor, mcp, session, slash)
//! - [`session`] — JSONL-backed session persistence
//!
//! The binary is the only stable artefact published. Other workspace
//! crates that want the runtime types depend on `norn-cli` as a path
//! dependency and use the modules below.

pub mod cli;
pub mod commands;
pub mod config;
pub mod nofile;
pub mod print;
pub mod runtime;
pub mod session;
pub mod tui;
