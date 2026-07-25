use claude_runner::QueryError;
use std::path::PathBuf;

use crate::resource::DescriptorAdmissionError;

/// A failure while configuring, running, or controlling a Norn-wrapped
/// Claude Code session.
#[derive(Debug, thiserror::Error)]
pub enum NornWrappedClaudeError {
    /// The configured MCP command line contained no executable.
    #[error("Norn MCP server command must contain an executable")]
    EmptyMcpCommand,

    /// The configured MCP command line ended inside a single-quoted word.
    #[error("Norn MCP server command has an unterminated single quote")]
    UnterminatedMcpSingleQuote,

    /// The configured MCP command line ended inside a double-quoted word.
    #[error("Norn MCP server command has an unterminated double quote")]
    UnterminatedMcpDoubleQuote,

    /// The configured MCP command line ended with an incomplete escape.
    #[error("Norn MCP server command ends with an incomplete escape")]
    TrailingMcpCommandEscape,

    /// Claude Runner's command builder cannot preserve a non-UTF-8
    /// executable path.
    #[error("Claude Code executable path is not valid UTF-8: {path:?}")]
    NonUtf8ClaudeCodePath {
        /// Original executable path, retained without lossy conversion.
        path: PathBuf,
    },

    /// A selected Norn tool name cannot be represented safely by Claude
    /// Code's comma-delimited `--allowedTools` argument.
    #[error("Norn tool at index {index} is invalid: {reason}")]
    InvalidNornToolName {
        /// Index of the invalid entry in the configured tool list.
        index: usize,
        /// Exact validation failure.
        reason: &'static str,
    },

    /// The MCP configuration could not be serialized.
    #[error("failed to serialize Norn MCP configuration: {0}")]
    McpConfigurationSerialization(#[source] serde_json::Error),

    /// Norn's process-wide descriptor governor rejected the subprocess.
    #[error(transparent)]
    DescriptorAdmission(#[from] DescriptorAdmissionError),

    /// The descriptor admission could not be reduced to the two handles
    /// retained by a live SDK query after the spawn boundary.
    #[error(
        "Claude SDK descriptor admission invariant failed: could not retain {retained} of {peak} admitted descriptors"
    )]
    DescriptorAdmissionInvariant {
        /// Factual number of parent pipe handles retained after spawn.
        retained: u32,
        /// Factual peak used for the spawn boundary.
        peak: u32,
    },

    /// The SDK session was polled without an active Tokio runtime.
    #[error("Norn-wrapped Claude SDK session requires an active Tokio runtime: {0}")]
    RuntimeUnavailable(#[source] tokio::runtime::TryCurrentError),

    /// Claude Code has no explicit `none` effort value; omission is distinct.
    #[error(
        "reasoning effort 'none' is not supported by Claude Code; omit reasoning_effort to use the Claude default"
    )]
    UnsupportedReasoningEffortNone,

    /// The high-level Claude SDK query rejected or failed an operation.
    #[error(transparent)]
    Query(#[from] QueryError),

    /// The Claude event stream failed while decoding or reading an event.
    #[error(transparent)]
    Event(#[from] claude_runner::Error),
}
