//! `norn` — agent runtime CLI binary.
//!
//! Thin entry point: parses arguments via [`norn_cli::cli::Cli`], detects
//! the execution mode, and dispatches to either the print orchestrator
//! ([`norn_cli::print::run`], NC-003), the TUI
//! ([`norn_tui::run_tui`], NT-001), or one of the subcommand handlers.

use std::io::IsTerminal;
use std::process::ExitCode as ProcessExitCode;

use clap::Parser;

use norn_cli::cli::{Cli, Command, ExitCode, Mode, detect_mode};
use norn_cli::commands::{run_auth, run_completion, run_doctor, run_init, run_mcp, run_session};
use norn_cli::print;

fn main() -> ProcessExitCode {
    let nofile = norn_cli::nofile::initialize();
    if let norn_cli::nofile::NofileOutcome::Failed { reason } = &nofile.outcome {
        eprintln!(
            "[WARN] File-descriptor capacity hardening failed ({reason}); run `norn doctor` for diagnostics."
        );
    }
    // Send tracing output to stderr so stdout stays clean for piping
    // (DESIGN CO5). The subscriber is best-effort: if a global subscriber
    // is already installed (tests, embedding), silently continue.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let mut cli = Cli::parse();
    let command = cli.command.take();
    let agent_fn: &dyn Fn(&Cli) -> ExitCode = &run_agent;

    let result = match command {
        Some(Command::Session { command }) => run_session(cli, command, agent_fn),
        Some(Command::Auth { command }) => run_auth(command),
        Some(Command::Mcp { command }) => run_mcp(command),
        Some(Command::Doctor) => run_doctor(),
        Some(Command::Completion(ref args)) => run_completion(args),
        Some(Command::Init { command }) => run_init(command),
        None => run_agent(&cli),
    };

    result.into()
}

/// Dispatch into either the TUI or the print orchestrator based on the
/// detected execution mode.
fn run_agent(cli: &Cli) -> ExitCode {
    let stdin_is_tty = std::io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();

    match detect_mode(cli.print, stdin_is_tty, stdout_is_tty) {
        Mode::Print => print::run(cli),
        Mode::Tui => norn_cli::tui::run(cli),
    }
}
