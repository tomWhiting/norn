//! Clap argument tree for the `norn` binary (NC-001 R2).
//!
//! Defines the full top-level command shape, value enums, and subcommand
//! groups. No business logic lives here — the parsed [`Cli`] struct is
//! consumed by `main.rs`, which dispatches into either the agent path
//! (REPL or print) or one of the [`Subcommand`] handlers.
//!
//! Per `DESIGN.md` NC2/NC3/NC4/NC5/NC6, NC13–NC17, every shared flag is
//! defined once on the top-level struct so it is available in both REPL
//! and print modes.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use super::mcp_args::McpCmd;
use super::policy_args::PolicyCmd;

/// Norn — agent runtime CLI: interactive REPL or one-shot headless execution.
#[derive(Parser, Debug)]
#[command(
    name = "norn",
    version,
    about = "Norn — agent runtime CLI: interactive REPL or one-shot headless execution."
)]
pub struct Cli {
    // -- Mode control (NC5) --
    /// Force non-interactive print mode (suppresses the REPL).
    #[arg(short = 'p', long)]
    pub print: bool,

    // -- Agent configuration (NC3) --
    /// Model identifier (overrides the profile's model).
    #[arg(short = 'm', long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Profile to load — file path (TOML/JSON/Markdown) or bare name
    /// resolved from `{cwd}/.norn/profiles/`, `{cwd}/.meridian/profiles/`,
    /// or `~/.norn/profiles/`.
    #[arg(long, value_name = "PATH|NAME")]
    pub profile: Option<String>,

    /// System prompt — overrides the profile's system instructions.
    #[arg(short = 'S', long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Append text to the profile's system instructions (additive).
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// Tool allow-list: comma-separated exact tool names; only the named
    /// tools are available to the agent.
    #[arg(long, value_name = "NAMES")]
    pub allowed_tools: Option<String>,

    /// Tool deny-list: comma-separated exact tool names, removed from the
    /// available set even when `--allowed-tools` names them.
    #[arg(long, value_name = "NAMES")]
    pub disallowed_tools: Option<String>,

    /// Reasoning effort level.
    #[arg(long, value_name = "LEVEL", value_enum)]
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Provider service tier.
    #[arg(long, value_name = "TIER", value_enum)]
    pub service_tier: Option<ServiceTier>,

    /// Enable the provider's fast service tier.
    #[arg(long, conflicts_with = "service_tier")]
    pub fast: bool,

    /// Maximum provider round-trips per agent step.
    #[arg(long, value_name = "N")]
    pub max_turns: Option<u32>,

    /// Step timeout (duration string, e.g. `2m`, `30s`).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,

    /// Working directory for tool execution.
    #[arg(short = 'C', long, value_name = "DIR")]
    pub working_dir: Option<PathBuf>,

    /// Confine the file tools (read/write/edit/patch) to this directory:
    /// any path resolving outside it after symlink-aware canonicalization
    /// is refused. When omitted, path resolution is unconfined.
    #[arg(long, value_name = "DIR")]
    pub workspace_root: Option<PathBuf>,

    /// Runtime config override (`KEY=VALUE`), repeatable.
    #[arg(short = 'c', long = "config", value_name = "KEY=VALUE")]
    pub config: Vec<String>,

    /// Rules YAML file.
    #[arg(long, value_name = "PATH")]
    pub rules: Option<PathBuf>,

    /// Session variable for `{{key}}` expansion (`KEY=VALUE`), repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub variables: Vec<String>,

    /// Connect MCP extension by URI, repeatable.
    #[arg(short = 'e', long = "extension", value_name = "URI")]
    pub extension: Vec<String>,

    // -- Output control (NC4) --
    /// JSON Schema for structured model output — inline JSON if value
    /// starts with `{`, otherwise a file path.
    #[arg(short = 's', long, value_name = "JSON|PATH")]
    pub output_schema: Option<String>,

    /// Per-event-type schema (`TYPE=JSON|PATH`), repeatable. TYPE is one
    /// of: `assistant_message`, `spoken_response`, `tool_call_envelope`,
    /// `stop_output`, `question`, `handoff`, `review`, `progress`.
    #[arg(long, value_name = "TYPE=JSON|PATH")]
    pub event_schema: Vec<String>,

    /// CLI rendering format.
    #[arg(short = 'f', long, value_name = "FORMAT", value_enum)]
    pub output_format: Option<OutputFormat>,

    /// Driven-mode transport protocol. When set to `jsonrpc`, Norn runs a
    /// bidirectional JSON-RPC 2.0 channel over stdin+stdout (stderr stays
    /// human logs) instead of the one-shot render path: it answers an
    /// `initialize` handshake, serves a single `run/execute` request whose
    /// response is the final result, and streams `event/*` notifications
    /// as the run proceeds. This is a transport flag, deliberately NOT an
    /// `--output-format` variant, so `-o` redirection and `--partial` do
    /// not implicitly apply. When absent, every existing render/TUI path is
    /// byte-for-byte unchanged.
    #[arg(long, value_name = "PROTOCOL", value_enum)]
    pub protocol: Option<Protocol>,

    /// Write final output to file.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Suppress progress and tool output on stderr.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Include incremental deltas in stream-json output. When omitted,
    /// only complete events are emitted.
    #[arg(long)]
    pub partial: bool,

    // -- Session control (NC6) --
    /// Resume a session by ID or name (no argument = most recent in cwd).
    #[arg(
        short = 'r',
        long,
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "ID|NAME",
        conflicts_with = "fork"
    )]
    pub resume: Option<String>,

    /// Fork a session by ID or name (no argument = most recent in cwd).
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "ID|NAME")]
    pub fork: Option<String>,

    /// Do not persist this session to disk.
    #[arg(long, conflicts_with_all = ["resume", "fork"])]
    pub no_session: bool,

    /// Human-readable name for the session.
    #[arg(long, value_name = "TEXT")]
    pub session_name: Option<String>,

    /// Create the session under this exact ID (fails if it already
    /// exists unless --resume-if-exists is also supplied; resume an
    /// existing session with --resume). The ID names the on-disk
    /// session file, so it must start with a letter or digit and
    /// contain only `[A-Za-z0-9._-]`.
    #[arg(
        long,
        value_name = "ID",
        conflicts_with_all = ["resume", "fork", "no_session"]
    )]
    pub session_id: Option<String>,

    /// With --session-id, resume that exact ID when it already exists.
    #[arg(long, requires = "session_id")]
    pub resume_if_exists: bool,

    /// Provider backend selection.
    #[arg(long, value_name = "PROVIDER", value_enum, conflicts_with_all = ["api_shape", "provider_profile"])]
    pub provider: Option<ProviderKind>,

    /// Provider wire API shape. Prefer this with --provider-profile for
    /// non-default deployments; --provider remains as a compatibility alias.
    #[arg(long, value_name = "API_SHAPE", value_enum)]
    pub api_shape: Option<ApiShapeKind>,

    /// Named provider profile from `settings.provider_profiles`.
    #[arg(long, value_name = "NAME")]
    pub provider_profile: Option<String>,

    /// Dump raw API requests and responses to a directory for debugging.
    /// Defaults to `~/.norn/debug/` when used without a value.
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "DIR"
    )]
    pub debug_api: Option<String>,

    /// Positional prompt words. Use `--` to pass flag-like strings as prompt text.
    #[arg(num_args = 0..)]
    pub prompt: Vec<String>,

    /// Subcommand. When omitted, the agent path runs (REPL or print).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Reasoning-effort levels accepted by `--reasoning-effort` and threaded
/// into `LoopContext` by the runtime wiring (future briefs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    /// No reasoning.
    None,
    /// Minimum reasoning budget.
    Low,
    /// Balanced reasoning budget.
    Medium,
    /// High reasoning budget.
    High,
    /// Extra-high reasoning budget.
    #[value(name = "xhigh")]
    XHigh,
    /// Maximum reasoning budget.
    Max,
}

/// Service tiers accepted by `--service-tier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ServiceTier {
    /// Faster provider execution when supported by the selected model/backend.
    Fast,
}

/// CLI rendering formats accepted by `--output-format` (NC4/NC18).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Human-readable text — final output on stdout, progress on stderr.
    Text,
    /// Single JSON envelope written to stdout at completion.
    Json,
    /// NDJSON streaming — one JSON event per line on stdout.
    StreamJson,
}

/// Driven-mode transport protocols accepted by `--protocol`.
///
/// A transport is a distinct concern from an [`OutputFormat`]: it takes
/// ownership of the full stdin+stdout duplex and speaks a framed wire
/// protocol, rather than rendering a one-shot result. Modelling it as its
/// own flag (not a fourth [`OutputFormat`] variant) keeps the render-only
/// concerns — `-o` redirection and `--partial` — from implicitly applying
/// to a duplex channel they do not make sense for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Protocol {
    /// Bidirectional JSON-RPC 2.0 over stdin+stdout: `initialize`
    /// handshake, one `run/execute` request whose response is the final
    /// result, and live `event/*` notifications. stderr stays human logs.
    Jsonrpc,
}

/// Provider backend choices for `--provider` (NC23).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// `OpenAiProvider` — OAuth via `OpenAI` `ChatGPT`, Responses API (default).
    Openai,
    /// OpenAI-compatible Chat Completions endpoint.
    OpenaiCompatible,
    /// `ClaudeRunnerAdapter` — routes through Claude Code CLI.
    ClaudeRunner,
}

/// Provider API-shape choices for `--api-shape`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ApiShapeKind {
    /// `OpenAI` Responses-compatible request and stream shape.
    OpenaiResponses,
    /// `OpenAI` Chat Completions-compatible request and stream shape.
    OpenaiChatCompletions,
    /// Anthropic Messages-compatible request and stream shape.
    AnthropicMessages,
    /// `OpenAI` Harmony prompt/response format.
    OpenaiHarmony,
    /// LM Studio native API shape.
    LmstudioNative,
    /// Local/remote agent RPC adapter.
    AgentRpc,
    /// Agent Client Protocol integration.
    AgentClientProtocol,
}

/// Top-level subcommands. The agent path runs when `command` is `None`.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Session management (NC14).
    Session {
        /// Session subcommand.
        #[command(subcommand)]
        command: SessionCmd,
    },
    /// Authentication (NC13).
    Auth {
        /// Auth subcommand.
        #[command(subcommand)]
        command: AuthCmd,
    },
    /// MCP server operations (NC15).
    Mcp {
        /// MCP subcommand.
        #[command(subcommand)]
        command: McpCmd,
    },
    /// Repository policy evaluation.
    Policy {
        /// Policy subcommand.
        #[command(subcommand)]
        command: PolicyCmd,
    },
    /// Run setup health checks (NC16).
    Doctor,
    /// Generate shell completion scripts (NC17).
    Completion(CompletionArgs),
    /// Initialise project configuration files (NTC-004).
    Init {
        /// Init subcommand.
        #[command(subcommand)]
        command: InitCmd,
    },
}

/// `init` subcommands (NTC-004).
#[derive(Subcommand, Debug)]
pub enum InitCmd {
    /// Scan the project and generate a starter `CONVENTIONS.toml`.
    Conventions {
        /// Upgrade a legacy `CONVENTIONS.toml` that uses `advise_on`/`block_on`
        /// groups into flat tool activations. Prints to stdout by default so
        /// you can review the migrated output before replacing the original.
        #[arg(long)]
        upgrade: bool,
        /// Read this legacy conventions file when using `--upgrade`.
        /// Defaults to `CONVENTIONS.toml` in the current working directory.
        #[arg(long, value_name = "PATH", requires = "upgrade")]
        input: Option<PathBuf>,
        /// Write to this path instead of `CONVENTIONS.toml` in the
        /// current working directory. With `--upgrade`, writes migrated output
        /// to this file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

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
    },
    /// Fork a session and enter the REPL on the new copy.
    Fork {
        /// Source session ID or name.
        #[arg(value_name = "ID|NAME")]
        id: String,
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

/// Auth subcommands (NC13).
#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// OAuth PKCE login flow (opens browser).
    Login {
        /// Override the codex home directory.
        #[arg(long, value_name = "DIR")]
        codex_home: Option<PathBuf>,
    },
    /// Clear stored credentials.
    Logout,
    /// Show auth state: logged in, token expiry, account ID.
    Status,
}

/// Arguments for the `completion` subcommand (NC17).
#[derive(Args, Debug)]
pub struct CompletionArgs {
    /// Target shell — `bash`, `zsh`, or `fish`.
    #[arg(value_name = "SHELL")]
    pub shell: String,
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "args_policy_tests.rs"]
mod policy_tests;
