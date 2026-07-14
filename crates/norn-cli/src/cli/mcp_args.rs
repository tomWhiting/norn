//! Command-line arguments for MCP client and server operations.

use clap::{Subcommand, ValueEnum};

/// MCP subcommands (NC15).
#[derive(Subcommand, Debug)]
pub enum McpCmd {
    /// Run Norn as an MCP server on stdio.
    Serve,
    /// Test connection to an MCP server by URI.
    Connect {
        /// MCP server URI.
        #[arg(value_name = "URI")]
        uri: String,
    },
    /// List effective configured MCP servers and their activation state.
    List,
    /// Inspect every configuration layer for one MCP server name.
    Inspect {
        /// Logical server name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Add or wholly replace one persistent MCP server definition.
    Add {
        /// Logical server name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Persistent settings scope to update.
        #[arg(long, value_enum, default_value = "local")]
        scope: McpPersistenceScope,
        /// Stdio server executable.
        #[arg(
            long,
            value_name = "COMMAND",
            conflicts_with = "url",
            required_unless_present = "url"
        )]
        command: Option<String>,
        /// Stdio server argument; repeat for multiple arguments.
        #[arg(
            long = "arg",
            value_name = "ARG",
            allow_hyphen_values = true,
            requires = "command"
        )]
        args: Vec<String>,
        /// Streamable HTTP MCP endpoint.
        #[arg(
            long,
            value_name = "URL",
            conflicts_with = "command",
            required_unless_present = "command"
        )]
        url: Option<String>,
        /// Stdio environment entry (`KEY=VALUE`); repeatable.
        #[arg(long, value_name = "KEY=VALUE", requires = "command")]
        env: Vec<String>,
        /// HTTP header (`KEY=VALUE`); repeatable.
        #[arg(long, value_name = "KEY=VALUE", requires = "url")]
        header: Vec<String>,
    },
    /// Remove one definition from a persistent scope, revealing lower layers.
    Remove {
        /// Logical server name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Persistent settings scope to update.
        #[arg(long, value_enum, default_value = "local")]
        scope: McpPersistenceScope,
    },
    /// Enable one definition in a persistent scope.
    Enable {
        /// Logical server name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Persistent settings scope to update.
        #[arg(long, value_enum, default_value = "local")]
        scope: McpPersistenceScope,
    },
    /// Disable one definition in a persistent scope without deleting it.
    Disable {
        /// Logical server name.
        #[arg(value_name = "NAME")]
        name: String,
        /// Persistent settings scope to update.
        #[arg(long, value_enum, default_value = "local")]
        scope: McpPersistenceScope,
    },
    /// Approve one or every shared-project MCP definition.
    Approve {
        /// Server name to approve.
        #[arg(value_name = "NAME", required_unless_present = "all")]
        name: Option<String>,
        /// Approve every effective shared-project server.
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
    /// Revoke approval for one or every shared-project MCP name.
    Revoke {
        /// Server name to revoke.
        #[arg(value_name = "NAME", required_unless_present = "all")]
        name: Option<String>,
        /// Revoke every effective shared-project server.
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
}

/// Persistent MCP configuration scopes accepted by `norn mcp` mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum McpPersistenceScope {
    /// User-wide trusted settings.
    User,
    /// Shared checked-in project settings; activation requires approval.
    Project,
    /// Workspace-local settings; direct local configuration without approval.
    WorkspaceLocal,
    /// Trusted machine-local settings for this project under `NORN_HOME`.
    Local,
}
