use clap::CommandFactory;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unexpected_command(expected: &'static str) -> Box<dyn std::error::Error> {
    std::io::Error::other(expected).into()
}

#[test]
fn cli_argument_parser_is_well_formed() {
    // clap's debug_assert validates every #[arg]/#[command] attribute at
    // construction time, including conflicting shorts and missing value names.
    Cli::command().debug_assert();
}

#[test]
fn parses_print_flag_with_positional_prompt() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-p", "hello"])?;
    assert!(cli.print);
    assert_eq!(cli.prompt, vec!["hello".to_string()]);
    assert!(cli.command.is_none());
    Ok(())
}

#[test]
fn parses_model_and_inline_schema_with_prompt() -> TestResult {
    let cli = Cli::try_parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "-s",
        r#"{"type":"object"}"#,
        "test prompt",
    ])?;
    assert_eq!(cli.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(cli.output_schema.as_deref(), Some(r#"{"type":"object"}"#));
    assert_eq!(cli.prompt, vec!["test prompt".to_string()]);
    Ok(())
}

#[test]
fn parses_multiple_config_overrides() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-c", "timeout=30s", "-c", "max_turns=5"])?;
    assert_eq!(
        cli.config,
        vec!["timeout=30s".to_string(), "max_turns=5".to_string()]
    );
    Ok(())
}

#[test]
fn resume_with_no_argument_is_empty_string_sentinel() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--resume"])?;
    assert_eq!(cli.resume.as_deref(), Some(""));
    Ok(())
}

#[test]
fn resume_with_argument_captures_id() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--resume", "abcd1234"])?;
    assert_eq!(cli.resume.as_deref(), Some("abcd1234"));
    Ok(())
}

#[test]
fn resume_if_exists_requires_session_id() -> TestResult {
    assert!(Cli::try_parse_from(["norn", "--resume-if-exists"]).is_err());
    let cli = Cli::try_parse_from(["norn", "--session-id", "wf-run-42", "--resume-if-exists"])?;
    assert_eq!(cli.session_id.as_deref(), Some("wf-run-42"));
    assert!(cli.resume_if_exists);
    Ok(())
}

#[test]
fn fork_with_no_argument_is_empty_string_sentinel() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--fork"])?;
    assert_eq!(cli.fork.as_deref(), Some(""));
    Ok(())
}

#[test]
fn session_list_subcommand_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "session", "list", "--all"])?;
    assert!(matches!(
        cli.command,
        Some(Command::Session {
            command: SessionCmd::List { all: true, .. },
        })
    ));
    Ok(())
}

#[test]
fn auth_login_subcommand_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "auth", "login"])?;
    assert!(matches!(
        cli.command,
        Some(Command::Auth {
            command: AuthCmd::Login { codex_home: None },
        })
    ));
    Ok(())
}

#[test]
fn mcp_connect_subcommand_requires_uri() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "mcp", "connect", "stdio://path/to/server"])?;
    assert!(matches!(
        cli.command,
        Some(Command::Mcp {
            command: McpCmd::Connect { uri },
        }) if uri == "stdio://path/to/server"
    ));
    Ok(())
}

#[test]
fn mcp_approval_subcommands_parse_name_or_all() -> TestResult {
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
fn mcp_add_parses_scoped_stdio_definition() -> TestResult {
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
fn doctor_subcommand_takes_no_args() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "doctor"])?;
    assert!(matches!(cli.command, Some(Command::Doctor)));
    Ok(())
}

#[test]
fn completion_subcommand_captures_shell() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "completion", "zsh"])?;
    assert!(matches!(
        cli.command,
        Some(Command::Completion(args)) if args.shell == "zsh"
    ));
    Ok(())
}

#[test]
fn init_conventions_subcommand_parses_without_output() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "init", "conventions"])?;
    let Some(Command::Init {
        command:
            InitCmd::Conventions {
                upgrade,
                input,
                output,
            },
    }) = cli.command
    else {
        return Err(unexpected_command("expected init conventions subcommand"));
    };
    assert!(!upgrade);
    assert!(input.is_none());
    assert!(output.is_none());
    Ok(())
}

#[test]
fn init_conventions_subcommand_captures_output_flag() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "init", "conventions", "--output", "alt.toml"])?;
    let Some(Command::Init {
        command:
            InitCmd::Conventions {
                upgrade,
                input,
                output,
            },
    }) = cli.command
    else {
        return Err(unexpected_command("expected init conventions subcommand"));
    };
    assert!(!upgrade);
    assert!(input.is_none());
    assert_eq!(output, Some(PathBuf::from("alt.toml")));
    Ok(())
}

#[test]
fn init_conventions_upgrade_flags_parse() -> TestResult {
    let cli = Cli::try_parse_from([
        "norn",
        "init",
        "conventions",
        "--upgrade",
        "--input",
        "legacy.toml",
        "--output",
        "new.toml",
    ])?;
    let Some(Command::Init {
        command:
            InitCmd::Conventions {
                upgrade,
                input,
                output,
            },
    }) = cli.command
    else {
        return Err(unexpected_command("expected init conventions subcommand"));
    };
    assert!(upgrade);
    assert_eq!(input, Some(PathBuf::from("legacy.toml")));
    assert_eq!(output, Some(PathBuf::from("new.toml")));
    Ok(())
}

#[test]
fn init_conventions_help_mentions_upgrade_review() -> TestResult {
    let Some(mut cmd) = conventions_help_command(Cli::command()) else {
        return Err(unexpected_command("expected conventions command"));
    };
    let help = cmd.render_long_help().to_string();
    assert!(help.contains("--upgrade"));
    assert!(help.contains("review"));
    Ok(())
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
fn reasoning_effort_accepts_canonical_values() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "high"])?;
    assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::High));

    let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "xhigh"])?;
    assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::XHigh));

    let cli = Cli::try_parse_from(["norn", "--reasoning-effort", "max"])?;
    assert_eq!(cli.reasoning_effort, Some(ReasoningEffort::Max));

    assert!(Cli::try_parse_from(["norn", "--reasoning-effort", "x-high"]).is_err());
    Ok(())
}

#[test]
fn service_tier_and_fast_flags_parse() -> TestResult {
    let tier = Cli::try_parse_from(["norn", "--service-tier", "fast"])?;
    assert_eq!(tier.service_tier, Some(ServiceTier::Fast));
    assert!(!tier.fast);

    let fast = Cli::try_parse_from(["norn", "--fast"])?;
    assert_eq!(fast.service_tier, None);
    assert!(fast.fast);
    Ok(())
}

#[test]
fn output_format_stream_json_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-f", "stream-json"])?;
    assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    Ok(())
}

#[test]
fn protocol_jsonrpc_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--protocol", "jsonrpc"])?;
    assert_eq!(cli.protocol, Some(Protocol::Jsonrpc));
    Ok(())
}

#[test]
fn protocol_absent_is_none() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-p", "hello"])?;
    assert_eq!(cli.protocol, None);
    Ok(())
}

#[test]
fn protocol_is_independent_of_output_format() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--protocol", "jsonrpc", "-f", "stream-json"])?;
    assert_eq!(cli.protocol, Some(Protocol::Jsonrpc));
    assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    Ok(())
}

#[test]
fn provider_kind_claude_runner_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--provider", "claude-runner"])?;
    assert_eq!(cli.provider, Some(ProviderKind::ClaudeRunner));
    Ok(())
}

#[test]
fn provider_kind_openai_compatible_parses() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--provider", "openai-compatible"])?;
    assert_eq!(cli.provider, Some(ProviderKind::OpenaiCompatible));
    Ok(())
}

#[test]
fn api_shape_and_provider_profile_parse() -> TestResult {
    let cli = Cli::try_parse_from([
        "norn",
        "--api-shape",
        "openai-chat-completions",
        "--provider-profile",
        "lmstudio",
    ])?;
    assert_eq!(cli.api_shape, Some(ApiShapeKind::OpenaiChatCompletions));
    assert_eq!(cli.provider_profile.as_deref(), Some("lmstudio"));
    Ok(())
}

#[test]
fn provider_conflicts_with_api_shape_path() -> TestResult {
    let result = Cli::try_parse_from([
        "norn",
        "--provider",
        "openai-compatible",
        "--api-shape",
        "openai-responses",
    ]);
    let Err(err) = result else {
        return Err(unexpected_command("expected provider argument conflict"));
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    Ok(())
}

#[test]
fn repeatable_flags_collect_all_values() -> TestResult {
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
    ])?;
    assert_eq!(cli.extension.len(), 2);
    assert_eq!(cli.variables.len(), 2);
    assert_eq!(cli.event_schema.len(), 2);
    Ok(())
}

#[test]
fn invalid_flag_returns_clap_error() {
    let result = Cli::try_parse_from(["norn", "--invalid-flag"]);
    assert!(result.is_err());
}

#[test]
fn debug_api_with_no_argument_is_empty_string_sentinel() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--debug-api"])?;
    assert_eq!(cli.debug_api.as_deref(), Some(""));
    Ok(())
}

#[test]
fn debug_api_with_path_captures_value() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--debug-api", "/tmp/debug"])?;
    assert_eq!(cli.debug_api.as_deref(), Some("/tmp/debug"));
    Ok(())
}

#[test]
fn debug_api_does_not_consume_positional_prompt() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--debug-api", "/tmp/debug", "hello world"])?;
    assert_eq!(cli.debug_api.as_deref(), Some("/tmp/debug"));
    assert_eq!(cli.prompt, vec!["hello world".to_string()]);
    Ok(())
}

#[test]
fn flags_after_prompt_are_not_consumed_as_prompt_text() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-p", "hello world", "-f", "stream-json"])?;
    assert!(cli.print);
    assert_eq!(cli.output_format, Some(OutputFormat::StreamJson));
    assert_eq!(cli.prompt, vec!["hello world".to_string()]);
    Ok(())
}

#[test]
fn double_dash_passes_flag_like_strings_as_prompt() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "-p", "--", "--help", "me", "with", "flags"])?;
    assert!(cli.print);
    assert_eq!(
        cli.prompt,
        vec!["--help", "me", "with", "flags"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
    Ok(())
}
