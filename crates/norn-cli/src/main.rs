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
    // (DESIGN CO5, D9). Losing the install means another global
    // subscriber owns the process's tracing and norn's stderr routing was
    // discarded — never silently: stdout is the machine-output channel in
    // `-f json` / `-f stream-json`, and `tracing_subscriber`'s own default
    // writer is stdout.
    if !print::ensure_stderr_tracing() {
        eprintln!(
            "[WARN] A tracing subscriber was already installed by this process, so norn could \
             not route engine diagnostics to stderr; with `-f json` / `-f stream-json` that \
             subscriber must not write to stdout or it will corrupt the output stream."
        );
    }

    let mut cli = Cli::parse();
    let command = cli.command.take();
    let agent_fn: &dyn Fn(&Cli) -> ExitCode = &run_agent;

    let result = match command {
        Some(Command::Session { command }) => run_session(cli, command, agent_fn),
        Some(Command::Auth { command }) => run_auth(&command),
        Some(Command::Mcp { command }) => run_mcp(&cli, command),
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
