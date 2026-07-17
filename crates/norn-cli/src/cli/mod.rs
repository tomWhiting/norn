//! CLI surface — argument parsing, mode detection, exit codes, and error types.

pub mod args;
pub mod error;
pub mod exit;
mod mcp_args;
pub mod mode;
mod session_args;

pub use args::*;
pub use error::BuildError;
pub use exit::ExitCode;
pub use mcp_args::{McpCmd, McpPersistenceScope};
pub use mode::{Mode, detect_mode};
pub use session_args::{LegacySessionCmd, SessionCmd, SessionExportFormat, SessionListFormat};
