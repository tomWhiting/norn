//! Slash-command surface for the Norn CLI (NC-006).
//!
//! Eleven CLI-built-in slash commands (`help`, `tools`, `model`, `schema`,
//! `compact`, `clear`, `session`, `name`, `variables`, `exit`, `quit`) are
//! registered as [`SlashCommandHandler::Custom`](norn::r#loop::commands::SlashCommandHandler)
//! entries that capture shared mutable runtime state via [`SlashState`].
//! Profile-registered slash commands flow into the same
//! [`SlashCommandRegistry`](norn::r#loop::commands::SlashCommandRegistry)
//! via [`build_slash_registry`] — CLI builtins win on name collision so a
//! user-defined `/help` never displaces the CLI surface.
//!
//! Layout:
//! - [`state`] — [`SlashState`] shared cells (model, output schema, name,
//!   cumulative usage, event store, command snapshot, action flags).
//! - [`registry`] — [`build_slash_registry`] merging profile commands with
//!   CLI builtins and the per-handler closures.
//! - [`dispatch`] — [`dispatch_input`] and the [`DispatchOutcome`] enum
//!   used by the print orchestrator and REPL to intercept CLI builtins
//!   before [`run_agent_step`](norn::r#loop::runner::run_agent_step).

pub mod actions;
pub mod dispatch;
pub mod registry;
pub mod state;

pub use actions::{CompactOutcome, apply_clear_request, apply_compact_request};
pub use dispatch::{DispatchOutcome, dispatch_input};
pub use registry::{BUILTIN_DESCRIPTIONS, CLI_BUILTIN_NAMES, build_slash_registry};
pub use state::SlashState;
