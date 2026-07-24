//! Terminal user interface for the Norn agent runtime.
//!
//! Renders streaming agent output in native terminal scrollback using
//! DECSTBM scroll regions, with a dynamic fixed panel for input, agent
//! status, and streaming indicators.

mod error;

pub mod agents;
pub mod app;
pub mod events;
pub mod input;
pub mod render;
pub mod terminal;
pub mod tools;

pub use app::{AppState, TuiInputs, TuiLifecycleEvent, run_app};
pub use error::TuiError;

use terminal::caps::TerminalCaps;
use terminal::setup::TerminalGuard;

/// Validate hard requirements and set up the raw-mode terminal guard.
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
/// Returns [`TuiError::UnsupportedTerminal`] if the terminal does not
/// meet minimum requirements, [`TuiError::Io`] on terminal I/O errors.
pub fn run_tui() -> Result<(), TuiError> {
    TerminalCaps::check_hard_requirements()?;
    let _guard = TerminalGuard::new()?;
    Ok(())
}
