//! CLI surface — argument parsing, mode detection, exit codes, and error types.

pub mod args;
pub mod error;
pub mod exit;
pub mod mode;

pub use args::*;
pub use error::BuildError;
pub use exit::ExitCode;
pub use mode::{Mode, detect_mode};
