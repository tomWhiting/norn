//! Session-management command arguments.

use clap::{Subcommand, ValueEnum};

/// Session subcommands (NC14).
#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    /// List sessions (defaults to the current working directory).
    List {
        /// Show sessions from all directories, not just the current one.
        #[arg(long)]
        all: bool,
        /// Maximum number of sessions to list.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Output format: `table` (default) or `json`.
        #[arg(long, value_name = "FORMAT", value_enum)]
        format: Option<SessionListFormat>,
    },
    /// Show session metadata and event summary.
    Show {
        /// Session ID or name (ID accepts an 8-character minimum prefix).
        #[arg(value_name = "ID|NAME")]
        id: String,
    },
    /// Resume a session interactively.
    Resume {
        /// Session ID or name.
        #[arg(value_name = "ID|NAME")]
        id: String,
        /// Allow a coherent migrated legacy session to resume from a fresh
        /// provider epoch when exact replay is unavailable.
        #[arg(long)]
        allow_degraded_session: bool,
    },
    /// Fork a session and enter the REPL on the new copy.
    Fork {
        /// Source session ID or name.
        #[arg(value_name = "ID|NAME")]
        id: String,
        /// Allow a coherent migrated legacy source to fork from a fresh
        /// provider epoch when exact replay is unavailable.
        #[arg(long)]
        allow_degraded_session: bool,
    },
    /// Export a session to a file.
    Export {
        /// Session ID or name.
        #[arg(value_name = "ID|NAME")]
        id: String,
        /// Export format.
        #[arg(long, value_name = "FORMAT", value_enum)]
        format: Option<SessionExportFormat>,
    },
    /// Remove a session and its index entry.
    Remove {
        /// Session ID or name.
        #[arg(value_name = "ID|NAME")]
        id: String,
    },
    /// Explicitly migrate the legacy session tree into the strict store.
    Migrate,
    /// Inspect or export ambiguous legacy sources retained by migration.
    Legacy {
        /// Read-only legacy catalog operation.
        #[command(subcommand)]
        command: LegacySessionCmd,
    },
}

/// Read-only operations over inspect-only migration records.
#[derive(Subcommand, Debug)]
pub enum LegacySessionCmd {
    /// Fully verify the active store, immutable backup, and live legacy tree.
    Verify,
    /// List inspect-only records and their stable catalog identifiers.
    List {
        /// Output format: `table` (default) or `json`.
        #[arg(long, value_name = "FORMAT", value_enum)]
        format: Option<SessionListFormat>,
    },
    /// Show one inspect-only migration record as JSON.
    Show {
        /// Stable `legacy-...` catalog identifier.
        #[arg(value_name = "CATALOG_ID")]
        catalog_id: String,
    },
    /// Stream the exact retained source bytes to stdout.
    Export {
        /// Stable `legacy-...` catalog identifier.
        #[arg(value_name = "CATALOG_ID")]
        catalog_id: String,
    },
}

/// Output formats for `session list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SessionListFormat {
    /// Human-readable table.
    Table,
    /// JSON array.
    Json,
}

/// Output formats for `session export`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SessionExportFormat {
    /// NDJSON of every `SessionEvent`.
    Jsonl,
    /// Single JSON document.
    Json,
    /// Human-readable Markdown transcript.
    Markdown,
}
