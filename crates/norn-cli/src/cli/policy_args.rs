//! Repository-policy command arguments.

use clap::{Subcommand, ValueEnum};

/// Repository-policy operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Subcommand)]
pub enum PolicyCmd {
    /// Evaluate the complete repository against its checked-in policy.
    Check {
        /// Machine-readable output representation.
        #[arg(long, value_enum)]
        format: PolicyOutputFormat,
    },
}

/// Output representations supported by `norn policy check`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum PolicyOutputFormat {
    /// One complete JSON policy state on stdout.
    Json,
}
