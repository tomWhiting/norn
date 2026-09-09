//! Terminal user interface for the Norn agent runtime.
//!
//! Retains semantic conversation history and paints an owned full-screen
//! workspace above a full-width composer.

mod error;

pub mod agents;
pub mod app;
pub mod events;
pub mod frontend_preferences;
pub mod input;
pub mod render;
pub mod terminal;
pub mod tools;

pub use app::{AppState, TuiInputs, run_app};
pub use error::TuiError;

use terminal::setup::TerminalGuard;

/// Set up the raw-mode terminal guard and progressive rendering capabilities.
///
/// This is a low-level entry point used by examples and tests that do
/// not need a full agent runtime. Production callers should invoke
/// [`run_app`] with a [`TuiInputs`] bundle from `norn-cli`.
///
/// Note on the brief's R9 dependency direction (NT-011): the literal
/// signature `run_tui(cli: &Cli) -> ExitCode` from the brief cannot
/// live in `norn-tui` because [`norn_cli::cli::Cli`] and
/// [`norn_cli::cli::ExitCode`] are types in the `norn-cli` crate, and
/// `norn-cli` already depends on `norn-tui` (one direction). The
/// `&Cli → ExitCode` binding lives in
/// `norn-cli/src/tui/driver.rs::run(cli)` which dispatches into
/// [`run_app`].
///
/// # Errors
///
/// Returns [`TuiError::Io`] on terminal I/O errors.
pub fn run_tui() -> Result<(), TuiError> {
    let guard = TerminalGuard::new()?;
    drop(guard);
    Ok(())
}
