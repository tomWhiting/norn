//! Error types for the norn-tui crate.

use std::io;

/// Errors that can occur during TUI operation.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// The terminal does not meet minimum requirements for the TUI.
    #[error("unsupported terminal: {0}")]
    UnsupportedTerminal(String),

    /// An I/O error occurred during terminal operations.
    #[error(transparent)]
    Io(#[from] io::Error),
}
