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
use super::session_args::SessionCmd;

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

    /// Operator instructions that replace the profile at Developer authority.
    /// The flag name is retained for CLI compatibility.
    #[arg(short = 'S', long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Append operator instructions at Developer authority (additive).
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

    /// Allow a coherent migrated legacy session to resume from a fresh
    /// provider epoch when its exact provider transcript cannot be replayed.
    /// Corrupt or ambiguous legacy records remain inspect/export-only.
    #[arg(long, conflicts_with = "no_session")]
    pub allow_degraded_session: bool,

    /// OAuth account for this agent run. Resumed, forked, and
    /// open-or-resume runs require an explicit account to avoid silently
    /// changing credential affinity; use `default` for the compatibility slot.
    #[arg(long, value_name = "ALIAS|default")]
    pub account: Option<String>,

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

impl Cli {
    /// Return whether this agent run may attach to existing session state.
    ///
    /// Pre-P5 session records do not persist account affinity. Resuming,
    /// forking, or conditionally opening an existing session therefore
    /// requires an explicit selection instead of consulting mutable active
    /// account state.
    #[must_use]
    pub const fn agent_run_may_reuse_session(&self) -> bool {
        self.resume.is_some() || self.fork.is_some() || self.resume_if_exists
    }
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

/// Auth subcommands (NC13).
#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// OAuth PKCE login flow (opens browser).
    Login {
        /// Publish the login as a named Norn-owned account.
        #[arg(long, value_name = "ALIAS")]
        name: Option<String>,
    },
    /// Clear stored credentials.
    Logout {
        /// Named account to remove; omit for the compatibility slot.
        #[arg(value_name = "ALIAS")]
        name: Option<String>,
        /// Remove the compatibility slot and every named account.
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
    /// Show side-effect-free local credential state without account identity.
    Status {
        /// Named account to inspect; omit for the compatibility slot.
        #[arg(value_name = "ALIAS")]
        name: Option<String>,
    },
    /// List the compatibility slot and published named accounts.
    List,
    /// Select an account for subsequently constructed providers.
    Use {
        /// Named alias or `default` compatibility slot.
        #[arg(value_name = "ALIAS|default")]
        name: String,
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
        assert!(!cli.allow_degraded_session);
    }

    #[test]
    fn degraded_session_approval_parses_on_agent_and_session_resume_paths()
    -> Result<(), clap::Error> {
        let agent =
            Cli::try_parse_from(["norn", "--resume", "abcd1234", "--allow-degraded-session"])?;
        assert!(agent.allow_degraded_session);

        let subcommand = Cli::try_parse_from([
            "norn",
            "session",
            "resume",
            "abcd1234",
            "--allow-degraded-session",
        ])?;
        assert!(matches!(
            subcommand.command,
            Some(Command::Session {
                command: SessionCmd::Resume {
                    allow_degraded_session: true,
                    ..
                },
            })
        ));
        Ok(())
    }

    #[test]
    fn resume_if_exists_requires_session_id() {
        assert!(Cli::try_parse_from(["norn", "--resume-if-exists"]).is_err());
        let cli = Cli::try_parse_from(["norn", "--session-id", "wf-run-42", "--resume-if-exists"])
            .unwrap();
        assert_eq!(cli.session_id.as_deref(), Some("wf-run-42"));
        assert!(cli.resume_if_exists);
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
    fn session_migrate_subcommand_parses() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from(["norn", "session", "migrate"])?;
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCmd::Migrate,
            })
        ));
        Ok(())
    }

    #[test]
    fn session_legacy_export_subcommand_parses() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "norn",
            "session",
            "legacy",
            "export",
            "legacy-0123456789abcdef",
        ])?;
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCmd::Legacy {
                    command: crate::cli::LegacySessionCmd::Export { catalog_id },
                },
            }) if catalog_id == "legacy-0123456789abcdef"
        ));
        Ok(())
    }

    #[test]
    fn session_legacy_verify_subcommand_parses() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from(["norn", "session", "legacy", "verify"])?;
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: SessionCmd::Legacy {
                    command: crate::cli::LegacySessionCmd::Verify,
                },
            })
        ));
        Ok(())
    }

    #[test]
    fn auth_login_subcommand_parses() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from(["norn", "auth", "login"])?;
        assert!(matches!(
            cli.command,
            Some(Command::Auth {
                command: AuthCmd::Login { name: None },
            })
        ));
        Ok(())
    }

    #[test]
    fn named_auth_commands_and_logout_exclusion_parse() -> Result<(), clap::Error> {
        let login = Cli::try_parse_from(["norn", "auth", "login", "--name", "work"])?;
        assert!(matches!(
            login.command,
            Some(Command::Auth {
                command: AuthCmd::Login { name: Some(ref name) },
            }) if name == "work"
        ));

        let use_account = Cli::try_parse_from(["norn", "auth", "use", "work"])?;
        assert!(matches!(
            use_account.command,
            Some(Command::Auth {
                command: AuthCmd::Use { ref name },
            }) if name == "work"
        ));
        assert!(Cli::try_parse_from(["norn", "auth", "logout", "work", "--all"]).is_err());
        Ok(())
    }

    #[test]
    fn auth_login_rejects_path_override_flags() {
        for flag in ["--codex-home", "--auth-root"] {
            assert!(Cli::try_parse_from(["norn", "auth", "login", flag, "/tmp/auth"]).is_err());
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
    fn mcp_approval_subcommands_parse_name_or_all() -> Result<(), clap::Error> {
        let named = Cli::try_parse_from(["norn", "mcp", "approve", "docs"])?;
        assert!(matches!(
            named.command,
            Some(Command::Mcp {
                command: McpCmd::Approve {
                    name: Some(ref name),
                    all: false,
                },
            }) if name == "docs"
        ));

        let all = Cli::try_parse_from(["norn", "mcp", "revoke", "--all"])?;
        assert!(matches!(
            all.command,
            Some(Command::Mcp {
                command: McpCmd::Revoke {
                    name: None,
                    all: true,
                },
            })
        ));
        Ok(())
    }

    #[test]
    fn mcp_add_parses_scoped_stdio_definition() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "norn",
            "mcp",
            "add",
            "docs",
            "--scope",
            "project",
            "--command",
            "npx",
            "--arg",
            "-y",
            "--arg",
            "@example/docs",
            "--env",
            "TOKEN=secret",
        ])?;
        assert!(matches!(
            cli.command,
            Some(Command::Mcp {
                command: McpCmd::Add {
                    name,
                    scope: crate::cli::McpPersistenceScope::Project,
                    command: Some(command),
                    args,
                    url: None,
                    env,
                    ..
                },
            }) if name == "docs"
                && command == "npx"
                && args == ["-y", "@example/docs"]
                && env == ["TOKEN=secret"]
        ));
        Ok(())
    }

    #[test]
    fn mcp_add_requires_exactly_one_transport() {
        assert!(Cli::try_parse_from(["norn", "mcp", "add", "docs"]).is_err());
        assert!(
            Cli::try_parse_from([
                "norn",
                "mcp",
                "add",
                "docs",
                "--command",
                "server",
                "--url",
                "https://example.test/mcp",
            ])
            .is_err()
        );
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
    fn reasoning_effort_accepts_canonical_values() {
        let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "high"]).unwrap();
        assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::High));

        let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "xhigh"]).unwrap();
        assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::XHigh));

        let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "max"]).unwrap();
        assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::Max));

        assert!(Cli::try_parse_from(["norn", "--reasoning-effort", "x-high"]).is_err());
    }

    #[test]
    fn service_tier_and_fast_flags_parse() {
        let tier = Cli::try_parse_from(["norn", "--service-tier", "fast"]).unwrap();
        assert_eq!(tier.service_tier, Some(ServiceTier::Fast));
        assert!(!tier.fast);

        let fast = Cli::try_parse_from(["norn", "--fast"]).unwrap();
        assert_eq!(fast.service_tier, None);
        assert!(fast.fast);
    }

    #[test]
    fn output_format_stream_json_parses() {
        let cli = Cli::try_parse_from(["norn", "-f", "stream-json"]).unwrap();
        assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    }

    #[test]
    fn protocol_jsonrpc_parses() {
        let cli = Cli::try_parse_from(["norn", "--protocol", "jsonrpc"]).unwrap();
        assert_eq!(cli.protocol, Some(Protocol::Jsonrpc));
    }

    #[test]
    fn protocol_absent_is_none() {
        let cli = Cli::try_parse_from(["norn", "-p", "hello"]).unwrap();
        assert_eq!(cli.protocol, None);
    }

    #[test]
    fn protocol_is_independent_of_output_format() {
        let cli =
            Cli::try_parse_from(["norn", "--protocol", "jsonrpc", "-f", "stream-json"]).unwrap();
        assert_eq!(cli.protocol, Some(Protocol::Jsonrpc));
        assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    }

    #[test]
    fn provider_kind_claude_runner_parses() {
        let cli = Cli::try_parse_from(["norn", "--provider", "claude-runner"]).unwrap();
        assert_eq!(cli.provider, Some(ProviderKind::ClaudeRunner));
    }

    #[test]
    fn provider_kind_openai_compatible_parses() {
        let cli = Cli::try_parse_from(["norn", "--provider", "openai-compatible"]).unwrap();
        assert_eq!(cli.provider, Some(ProviderKind::OpenaiCompatible));
    }

    #[test]
    fn api_shape_and_provider_profile_parse() {
        let cli = Cli::try_parse_from([
            "norn",
            "--api-shape",
            "openai-chat-completions",
            "--provider-profile",
            "lmstudio",
        ])
        .unwrap();
        assert_eq!(cli.api_shape, Some(ApiShapeKind::OpenaiChatCompletions));
        assert_eq!(cli.provider_profile.as_deref(), Some("lmstudio"));
    }

    #[test]
    fn provider_conflicts_with_api_shape_path() {
        let err = Cli::try_parse_from([
            "norn",
            "--provider",
            "openai-compatible",
            "--api-shape",
            "openai-responses",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
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
