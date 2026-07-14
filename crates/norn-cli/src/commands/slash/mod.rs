//! Slash-command surface for the Norn CLI (NC-006).
//!
//! CLI-built-in slash commands are registered as
//! [`SlashCommandHandler::Custom`](norn::agent_loop::commands::SlashCommandHandler)
//! entries that capture shared mutable runtime state via [`SlashState`].
//! Profile-registered slash commands flow into the same
//! [`SlashCommandRegistry`](norn::agent_loop::commands::SlashCommandRegistry)
//! via [`build_slash_registry`] — CLI builtins win on name collision so a
//! user-defined `/help` never displaces the CLI surface.
//!
//! Layout:
//! - [`state`] — [`SlashState`] shared cells (model, service tier, output
//!   schema, name, cumulative usage, event store, command snapshot, action flags).
//! - [`registry`] — [`build_slash_registry`] merging profile commands with
//!   CLI builtins and the per-handler closures.
//! - [`dispatch`] — [`dispatch_input`] and the [`DispatchOutcome`] enum
//!   used by the print orchestrator and REPL to intercept CLI builtins
//!   before [`run_agent_step`](norn::agent_loop::runner::run_agent_step).

pub mod actions;
pub mod dispatch;
pub mod registry;
pub mod state;

pub use actions::{ClearOutcome, CompactOutcome, apply_clear_request, apply_compact_request};
pub use dispatch::{DispatchOutcome, dispatch_input, dispatch_input_with_mcp};
pub use registry::{build_slash_registry, builtin_descriptions, cli_builtin_names};
pub use state::SlashState;
