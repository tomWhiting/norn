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
    /// Resume a session by ID or name (no argument = most recent).
    #[arg(
        short = 'r',
        long,
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "ID|NAME"
    )]
    pub resume: Option<String>,

    /// Fork a session by ID or name (no argument = most recent).
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "ID|NAME")]
    pub fork: Option<String>,

    /// Do not persist this session to disk.
    #[arg(long)]
    pub no_session: bool,

    /// Human-readable name for the session.
    #[arg(long, value_name = "TEXT")]
    pub session_name: Option<String>,

    /// Provider backend selection.
    #[arg(long, value_name = "PROVIDER", value_enum)]
    pub provider: Option<ProviderKind>,

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
    /// Maximum reasoning budget.
    High,
    /// Extended reasoning budget.
    XHigh,
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

/// Provider backend choices for `--provider` (NC23).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// `OpenAiProvider` — OAuth via `OpenAI` `ChatGPT`, Responses API (default).
    Openai,
    /// `ClaudeRunnerAdapter` — routes through Claude Code CLI.
    ClaudeRunner,
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
}

/// Arguments for the `completion` subcommand (NC17).
#[derive(Args, Debug)]
pub struct CompletionArgs {
    /// Target shell — `bash`, `zsh`, or `fish`.
    #[arg(value_name = "SHELL")]
    pub shell: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_argument_parser_is_well_formed() {
        // clap's debug_assert validates every #[arg]/#[command] attribute at
        // construction time — this catches conflicting shorts, missing
        // value_names, and similar mistakes without invoking the binary.
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_print_flag_with_positional_prompt() {
        let cli = Cli::try_parse_from(["norn", "-p", "hello"]).unwrap();
        assert!(cli.print);
        assert_eq!(cli.prompt, vec!["hello".to_string()]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_model_and_inline_schema_with_prompt() {
        let cli = Cli::try_parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "-s",
            r#"{"type":"object"}"#,
            "test prompt",
        ])
        .unwrap();
        assert_eq!(cli.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(cli.output_schema.as_deref(), Some(r#"{"type":"object"}"#));
        assert_eq!(cli.prompt, vec!["test prompt".to_string()]);
    }

    #[test]
    fn parses_multiple_config_overrides() {
        let cli = Cli::try_parse_from(["norn", "-c", "timeout=30s", "-c", "max_turns=5"]).unwrap();
        assert_eq!(
            cli.config,
            vec!["timeout=30s".to_string(), "max_turns=5".to_string()]
        );
    }

    #[test]
    fn resume_with_no_argument_is_empty_string_sentinel() {
        let cli = Cli::try_parse_from(["norn", "--resume"]).unwrap();
        assert_eq!(cli.resume.as_deref(), Some(""));
    }

    #[test]
    fn resume_with_argument_captures_id() {
        let cli = Cli::try_parse_from(["norn", "--resume", "abcd1234"]).unwrap();
        assert_eq!(cli.resume.as_deref(), Some("abcd1234"));
    }

    #[test]
    fn fork_with_no_argument_is_empty_string_sentinel() {
        let cli = Cli::try_parse_from(["norn", "--fork"]).unwrap();
        assert_eq!(cli.fork.as_deref(), Some(""));
    }

    #[test]
    fn session_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["norn", "session", "list", "--all"]).unwrap();
        match cli.command {
            Some(Command::Session {
                command: SessionCmd::List { all, .. },
            }) => assert!(all),
            other => panic!("expected session list subcommand, got {other:?}"),
        }
    }

    #[test]
    fn auth_login_subcommand_parses() {
        let cli = Cli::try_parse_from(["norn", "auth", "login"]).unwrap();
        match cli.command {
            Some(Command::Auth {
                command: AuthCmd::Login { codex_home },
            }) => assert!(codex_home.is_none()),
            other => panic!("expected auth login subcommand, got {other:?}"),
        }
    }

    #[test]
    fn mcp_connect_subcommand_requires_uri() {
        let cli =
            Cli::try_parse_from(["norn", "mcp", "connect", "stdio://path/to/server"]).unwrap();
        match cli.command {
            Some(Command::Mcp {
                command: McpCmd::Connect { uri },
            }) => assert_eq!(uri, "stdio://path/to/server"),
            other => panic!("expected mcp connect subcommand, got {other:?}"),
        }
    }

    #[test]
    fn doctor_subcommand_takes_no_args() {
        let cli = Cli::try_parse_from(["norn", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Doctor)));
    }

    #[test]
    fn completion_subcommand_captures_shell() {
        let cli = Cli::try_parse_from(["norn", "completion", "zsh"]).unwrap();
        match cli.command {
            Some(Command::Completion(args)) => assert_eq!(args.shell, "zsh"),
            other => panic!("expected completion subcommand, got {other:?}"),
        }
    }

    #[test]
    fn init_conventions_subcommand_parses_without_output() {
        let cli = Cli::try_parse_from(["norn", "init", "conventions"]).unwrap();
        match cli.command {
            Some(Command::Init {
                command:
                    InitCmd::Conventions {
                        upgrade,
                        input,
                        output,
                    },
            }) => {
                assert!(!upgrade);
                assert!(input.is_none());
                assert!(output.is_none());
            }
            other => panic!("expected init conventions subcommand, got {other:?}"),
        }
    }

    #[test]
    fn init_conventions_subcommand_captures_output_flag() {
        let cli =
            Cli::try_parse_from(["norn", "init", "conventions", "--output", "alt.toml"]).unwrap();
        match cli.command {
            Some(Command::Init {
                command:
                    InitCmd::Conventions {
                        upgrade,
                        input,
                        output,
                    },
            }) => {
                assert!(!upgrade);
                assert!(input.is_none());
                assert_eq!(output, Some(PathBuf::from("alt.toml")));
            }
            other => panic!("expected init conventions subcommand, got {other:?}"),
        }
    }

    #[test]
    fn init_conventions_upgrade_flags_parse() {
        let cli = Cli::try_parse_from([
            "norn",
            "init",
            "conventions",
            "--upgrade",
            "--input",
            "legacy.toml",
            "--output",
            "new.toml",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Init {
                command:
                    InitCmd::Conventions {
                        upgrade,
                        input,
                        output,
                    },
            }) => {
                assert!(upgrade);
                assert_eq!(input, Some(PathBuf::from("legacy.toml")));
                assert_eq!(output, Some(PathBuf::from("new.toml")));
            }
            other => panic!("expected init conventions subcommand, got {other:?}"),
        }
    }

    #[test]
    fn init_conventions_help_mentions_upgrade_review() {
        let mut cmd = conventions_help_command(Cli::command()).expect("conventions command");
        let help = cmd.render_long_help().to_string();
        assert!(help.contains("--upgrade"));
        assert!(help.contains("review"));
    }

    fn conventions_help_command(mut cmd: clap::Command) -> Option<clap::Command> {
        let init = cmd.find_subcommand_mut("init")?;
        init.find_subcommand("conventions").cloned()
    }

    #[test]
    fn init_subcommand_registered_in_command_tree() {
        let cmd = Cli::command();
        assert!(
            cmd.get_subcommands().any(|s| s.get_name() == "init"),
            "init subcommand must appear in the clap command tree"
        );
    }

    #[test]
    fn reasoning_effort_accepts_kebab_case_values() {
        let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "high"]).unwrap();
        assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn output_format_stream_json_parses() {
        let cli = Cli::try_parse_from(["norn", "-f", "stream-json"]).unwrap();
        assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    }

    #[test]
    fn provider_kind_claude_runner_parses() {
        let cli = Cli::try_parse_from(["norn", "--provider", "claude-runner"]).unwrap();
        assert_eq!(cli.provider, Some(ProviderKind::ClaudeRunner));
    }

    #[test]
    fn repeatable_flags_collect_all_values() {
        let cli = Cli::try_parse_from([
            "norn",
            "-e",
            "stdio://a",
            "--extension",
            "stdio://b",
            "--variables",
            "project=yggdrasil",
            "--variables",
            "env=staging",
            "--event-schema",
            "spoken_response=tts.json",
            "--event-schema",
            r#"assistant_message={"type":"object"}"#,
        ])
        .unwrap();
        assert_eq!(cli.extension.len(), 2);
        assert_eq!(cli.variables.len(), 2);
        assert_eq!(cli.event_schema.len(), 2);
    }

    #[test]
    fn invalid_flag_returns_clap_error() {
        let result = Cli::try_parse_from(["norn", "--invalid-flag"]);
        assert!(result.is_err());
    }

    #[test]
    fn debug_api_with_no_argument_is_empty_string_sentinel() {
        let cli = Cli::try_parse_from(["norn", "--debug-api"]).unwrap();
        assert_eq!(cli.debug_api.as_deref(), Some(""));
    }

    #[test]
    fn debug_api_with_path_captures_value() {
        let cli = Cli::try_parse_from(["norn", "--debug-api", "/tmp/debug"]).unwrap();
        assert_eq!(cli.debug_api.as_deref(), Some("/tmp/debug"));
    }

    #[test]
    fn debug_api_does_not_consume_positional_prompt() {
        let cli =
            Cli::try_parse_from(["norn", "--debug-api", "/tmp/debug", "hello world"]).unwrap();
        assert_eq!(cli.debug_api.as_deref(), Some("/tmp/debug"));
        assert_eq!(cli.prompt, vec!["hello world".to_string()]);
    }

    #[test]
    fn flags_after_prompt_are_not_consumed_as_prompt_text() {
        let cli = Cli::try_parse_from(["norn", "-p", "hello world", "-f", "stream-json"]).unwrap();
        assert!(cli.print);
        assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
        assert_eq!(cli.prompt, vec!["hello world".to_string()]);
    }

    #[test]
    fn double_dash_passes_flag_like_strings_as_prompt() {
        let cli =
            Cli::try_parse_from(["norn", "-p", "--", "--help", "me", "with", "flags"]).unwrap();
        assert!(cli.print);
        assert_eq!(
            cli.prompt,
            vec!["--help", "me", "with", "flags"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }
}
